//! game_launcher —— 启动已安装的版本（vanilla / fabric / forge / neoforge）
//!
//! 命令：
//!   launch_game(versionId, accountId, javaPath?, memoryMb?) -> Void
//!   stop_game() -> Void
//!
//! 事件 launch-status（前端 listen）：
//!   { status, detail, stage?, code?, aliveSecs?, pid?, tail? }
//!     status: starting / running / stopped / error
//!     detail: 人类可读说明（中文）
//!     stage:  error 时的来源 —— resolve / launch（启动失败）或 crash（异常退出）
//!     tail:   crash 时附带的游戏输出尾部（detail 只有摘要）
//!
//! 事件 game-output（前端 listen，透传游戏 stdout/stderr）：
//!   { line }
//!
//! 启动是异步的：命令返回时只代表校验通过、Java 已拉起。之后进程状态/输出
//! 全部走事件通道。同一时间只允许一个游戏进程在跑。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::db_handler::{AccountRow, Db};
use crate::minecraft_manager;

/// 正在运行的游戏进程句柄（全局唯一）。
static RUNNING: AtomicBool = AtomicBool::new(false);
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

// ── 账号凭据 ───────────────────────────────────────────────────────────

fn find_account_row(
    conn: &rusqlite::Connection,
    account_id: u16,
) -> Result<AccountRow, String> {
    conn.query_row(
        "SELECT id, uuid, name, account_type, msa_id, last_used, created_at
         FROM accounts WHERE id = ?1",
        rusqlite::params![account_id],
        |r| {
            Ok(AccountRow {
                id: r.get(0)?,
                uuid: r.get(1)?,
                name: r.get(2)?,
                account_type: r.get(3)?,
                msa_id: r.get(4)?,
                last_used: r.get(5)?,
                created_at: r.get(6)?,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => format!("账号不存在 (id={account_id})"),
        other => other.to_string(),
    })
}

/// 解析启动账号：离线账号用 db 存的固定 uuid + 占位 accessToken；
/// 微软账号用 keyring 缓存的 Minecraft token（失效自动刷新）。
/// 返回 (username, dashless_uuid, accessToken)。
async fn resolve_launch_account(
    db: &Db,
    account_id: u16,
) -> Result<(String, String, String), String> {
    let row = {
        let conn = db.lock()?;
        find_account_row(&conn, account_id)?
    };
    let token = if row.account_type == 0 {
        let (_uuid, msa_id) = {
            let conn = db.lock()?;
            crate::account_manager::lookup_msa_id_pub(&conn, account_id)?
        };
        crate::account_manager::ensure_mc_token_pub(&msa_id).await?
    } else {
        // 离线账号：占位 token（离线启动不校验）
        "0".to_string()
    };
    Ok((row.name, row.uuid, token))
}

// ── 版本 JSON 解析 ─────────────────────────────────────────────────────

fn read_version_json(root: &Path, id: &str) -> Result<Value, String> {
    let path = root.join("versions").join(id).join(format!("{id}.json"));
    let text =
        fs::read_to_string(&path).map_err(|e| format!("读取版本 {id} 的 JSON 失败: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("解析版本 {id} 的 JSON 失败: {e}"))
}

/// 单个参数条目是否在当前平台/特性下启用（rules ± features）。
/// os 缺省 = 适用；features 只判断已知 bool 键，未知按 false 处理。
fn arg_allowed(entry: &Value) -> bool {
    let Some(rules) = entry["rules"].as_array() else { return true };
    let mut allowed = false;
    for r in rules {
        let os_ok = r["os"]["name"].as_str().map_or(true, |n| n == "windows");
        let feat_ok = match r["features"].as_object() {
            // 本启动器不启用任何 feature（demo/自定义分辨率/quick play），
            // 因此仅当规则要求的所有特性都为“不需要(=false)”时该参数才生效。
            None => true,
            Some(f) => f
                .iter()
                .all(|(_k, v)| !v.as_bool().unwrap_or(false)),
        };
        if !(os_ok && feat_ok) {
            continue;
        }
        if r["action"].as_str() == Some("allow") {
            allowed = true;
        } else if r["action"].as_str() == Some("disallow") {
            allowed = false;
        }
    }
    allowed
}

/// 收集一层的 arguments.jvm / arguments.game（兼容字符串与带 rules 的对象条目）。
fn collect_args(json: &Value, section: &str, out: &mut Vec<String>) {
    if let Some(mc_args) = json["arguments"][section].as_str() {
        out.extend(mc_args.split_whitespace().map(str::to_string));
        return;
    }
    if let Some(arr) = json["arguments"][section].as_array() {
        for item in arr {
            if let Some(s) = item.as_str() {
                out.push(s.to_string());
                continue;
            }
            // 对象条目：value 可为字符串或字符串数组，rules 命中才追加
            if !arg_allowed(item) {
                continue;
            }
            match &item["value"] {
                Value::String(s) => out.push(s.clone()),
                Value::Array(vals) => {
                    for v in vals {
                        if let Some(s) = v.as_str() {
                            out.push(s.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// 继承链：末尾是基底（无 inheritsFrom），起点是加载器层（fabric/forge/neoforge）。
fn collect_chain(root: &Path, version_id: &str) -> Result<Vec<(String, Value)>, String> {
    let mut chain: Vec<(String, Value)> = Vec::new();
    let mut cur_id = version_id.to_string();
    loop {
        let json = read_version_json(root, &cur_id)?;
        let inherits = json["inheritsFrom"].as_str().map(str::to_string);
        chain.push((cur_id.clone(), json));
        match inherits {
            Some(base) if base != cur_id => cur_id = base,
            _ => break,
        }
    }
    Ok(chain)
}

// ── Tauri 命令 ─────────────────────────────────────────────────────────

#[tauri::command]
pub fn launch_game(
    app: AppHandle,
    _db: State<'_, Db>,
    version_id: String,
    account_id: u16,
    java_path: Option<String>,
    memory_mb: Option<u32>,
) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("已有游戏正在启动/运行，请先停止".to_string());
    }
    let memory_mb = memory_mb.unwrap_or(1024);

    // 非阻塞启动：命令立即返回（空回调），真正的启动工作（含 MS token 刷新等
    // 耗时 await）在后台任务完成。这样前端 invoke 不会跨页面/窗口重载挂起，
    // 也就不会出现 “Couldn't find callback id”。进度/错误全部走 launch-status 事件。
    //
    // 报错兜底：无论 resolve/launch_blocking 返回 Err 还是在同步执行里 panic，
    // 都转成 launch-status:error 发给前端，绝不静默（否则就会“启动不了且无报错”）。
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let catch_app = app2.clone();
        // resolve_launch_account 返回 Result、少量 await，本身不 panic。
        let resolved = match resolve_launch_account(&app2.state::<Db>(), account_id).await {
            Ok(v) => v,
            Err(e) => {
                log_err(&format!("launch_game 解析账号失败: {e}"));
                emit_status_full(&catch_app, "error", &e, serde_json::json!({ "stage": "resolve" }));
                RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        };
        // launch_blocking 是同步重活，用 catch_unwind 兜住任何 panic。
        let launch_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            launch_blocking(
                &app2,
                &version_id,
                &resolved.0,
                &resolved.1,
                &resolved.2,
                java_path.as_deref(),
                memory_mb,
            )
        }));
        let result = match launch_res {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_panic) => Err("启动器内部错误（后台任务异常中断）".to_string()),
        };
        if let Err(e) = result {
            log_err(&format!("launch_game 失败: {e}"));
            emit_status_full(&catch_app, "error", &e, serde_json::json!({ "stage": "launch" }));
            RUNNING.store(false, Ordering::SeqCst);
        }
    });
    Ok(())
}

#[tauri::command]
pub fn stop_game() -> Result<(), String> {
    if let Some(mut child) = CHILD.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = child.kill();
        let _ = child.wait(); // 监视线程仍会发 stopped，这里不留句柄
    }
    Ok(())
}

// ── 拼装与启动 ─────────────────────────────────────────────────────────

fn emit_status(app: &AppHandle, status: &str, detail: &str) {
    let _ = app.emit("launch-status", serde_json::json!({ "status": status, "detail": detail }));
}

/// 带附加结构字段的 launch-status（error 事件用）：
///   stage: "resolve" / "launch" / "crash" —— 前端据此区分「启动失败」与「游戏异常退出」
///   code / aliveSecs / pid / tail —— crash 时附带的退出码、存活秒数、输出尾部
fn emit_status_full(app: &AppHandle, status: &str, detail: &str, extra: serde_json::Value) {
    let mut payload = serde_json::json!({ "status": status, "detail": detail });
    if let (Some(obj), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    let _ = app.emit("launch-status", payload);
}

/// 把启动失败记录到 .minecraft/launch-error.log，并打到 stderr；
/// 便于“启动无报错”时在本机排查到底发生了什么。
fn log_err(msg: &str) {
    eprintln!("[gsml-launch] {msg}");
    if let Ok(dir) = minecraft_manager::get_minecraft_dir() {
        let _ = std::fs::write(
            dir.join("launch-error.log"),
            format!("[{}] {msg}\n", chrono_like_now()),
        );
    }
}

/// 简单时间戳（避免新增依赖）：UTC 时间，格式 YYYY-MM-DD HH:MM:SS。
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d) = civil_from_days(secs / 86400);
    let tod = secs % 86400;
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y}-{mo:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// 天数 → 公历 (年, 月, 日)；小时/分钟/秒由调用方按天内秒计算。
/// 基于 Howard Hinnant 的 civil_from_days 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn emit_output(app: &AppHandle, line: &str) {
    let _ = app.emit("game-output", serde_json::json!({ "line": line }));
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn classpath_string(paths: &[PathBuf]) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    paths
        .iter()
        .map(|p| path_str(p))
        .collect::<Vec<_>>()
        .join(sep)
}

/// Maven 坐标（`group:artifact:version[:classifier]`）→ 库相对路径，
/// 与 minecraft_manager 安装器的下载路径公式保持一致。
/// Fabric 等第三方版本 JSON 的库条目没有 Mojang 式 downloads.artifact，
/// 只有 name（如 net.fabricmc:fabric-loader:0.16.14），要从坐标推导路径。
fn maven_rel_path(coord: &str) -> Option<String> {
    let parts: Vec<&str> = coord.split(':').collect();
    let (group, artifact, version, classifier) = match parts.as_slice() {
        [g, a, v] => (*g, *a, *v, None),
        [g, a, v, c] => (*g, *a, *v, Some(*c)),
        _ => return None,
    };
    let file = match classifier {
        Some(c) => format!("{artifact}-{version}-{c}.jar"),
        None => format!("{artifact}-{version}.jar"),
    };
    Some(format!(
        "{}/{}/{}/{}",
        group.replace('.', "/"),
        artifact,
        version,
        file
    ))
}

/// 用 token 表替换单个参数里的 ${token}
fn expand(arg: &str, map: &std::collections::HashMap<&str, String>) -> String {
    let mut out = String::with_capacity(arg.len() + 16);
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
            if let Some(close) = arg[i + 2..].find('}') {
                let key = &arg[i + 2..i + 2 + close];
                let full = &format!("${{{key}}}");
                let repl = map.get(key).map(|v| v.as_str()).unwrap_or(full);
                out.push_str(repl);
                i += 2 + close + 1;
                continue;
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn launch_blocking(
    app: &AppHandle,
    version_id: &str,
    username: &str,
    uuid: &str,
    access_token: &str,
    java_path: Option<&str>,
    memory_mb: u32,
) -> Result<(), String> {
    let root = minecraft_manager::get_minecraft_dir()?;
    let java = minecraft_manager::find_java(java_path)?;

    // —— 合并版本信息 ——
    let chain = collect_chain(&root, version_id)?;
    // chain 从启动目标层一路向下到最底层基底（无 inheritsFrom）→ 基底是最后一个
    let effective_base = chain.last().unwrap().0.clone();
    let mut main_class = String::new();
    let mut asset_index = String::new();
    let mut jvm_args: Vec<String> = Vec::new();
    let mut game_args: Vec<String> = Vec::new();
    let mut classpath: Vec<PathBuf> = Vec::new();

    for (i, (id, json)) in chain.iter().enumerate() {
        // mainClass / assetIndex 取最上层（加载器/forge 层）定义者优先
        if i == 0 {
            if let Some(m) = json["mainClass"].as_str() {
                main_class = m.to_string();
            }
            if let Some(a) = json["assetIndex"]["id"].as_str() {
                asset_index = a.to_string();
            }
        } else if main_class.is_empty() {
            if let Some(m) = json["mainClass"].as_str() {
                main_class = m.to_string();
            }
        }
        if asset_index.is_empty() {
            if let Some(a) = json["assetIndex"]["id"].as_str() {
                asset_index = a.to_string();
            }
        }
        // JVM 参数各层合并；游戏参数各层合并（加载器层通常会自带/覆盖基底参数）
        collect_args(json, "jvm", &mut jvm_args);
        let before = game_args.len();
        collect_args(json, "game", &mut game_args);
        let after = game_args.len();
        if after == before {
            if let Some(s) = json["minecraftArguments"].as_str() {
                game_args.extend(s.split_whitespace().map(str::to_string));
            }
        }
        // libraries
        if let Some(libs) = json["libraries"].as_array() {
            for lib in libs {
                if !minecraft_manager::lib_allowed(lib) {
                    continue;
                }
                // Mojang 式条目直接用 downloads.artifact.path；Fabric 等第三方
                // JSON 是 Maven 坐标式（只有 name，可能带 url）。坐标式条目
                // 曾被直接跳过，fabric-loader 的 jar 进不了 classpath，启动
                // 直接报 ClassNotFoundException: ...KnotClient。
                let rel = lib["downloads"]["artifact"]["path"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| maven_rel_path(lib["name"].as_str()?));
                let Some(rel) = rel else {
                    continue;
                };
                // 即使带 native classifier，主 artifact jar 也必须在 classpath 上
                // （如 LWJGL 核心库）；natives 已由安装器解压到 natives 目录，
                // 通过 -Djava.library.path 生效，不入 classpath。
                let abs = root.join("libraries").join(rel);
                if abs.is_file() {
                    classpath.push(abs);
                }
            }
        }
        // 客户端 jar：只有装在 <versions>/<id> 下才存在（vanilla/fabric 有，forge 派生目录无）
        let vjar = root.join("versions").join(id).join(format!("{id}.jar"));
        if vjar.is_file() {
            classpath.push(vjar);
        }
    }

    if main_class.is_empty() {
        return Err(format!("版本 {version_id} 缺少 mainClass"));
    }
    if asset_index.is_empty() {
        return Err(format!("版本 {version_id} 缺少 assetIndex"));
    }

    let natives_dir = root
        .join("versions")
        .join(&effective_base)
        .join("natives");
    fs::create_dir_all(&natives_dir).map_err(|e| format!("创建 natives 目录失败: {e}"))?;
    let game_dir = root.clone();
    let assets_dir = root.join("assets");

    // —— token 替换表 ——
    let user_type = if access_token == "0" { "legacy" } else { "msa" };
    let mut map = std::collections::HashMap::new();
    map.insert("auth_player_name", username.to_string());
    map.insert("auth_uuid", uuid.to_string());
    map.insert("auth_access_token", access_token.to_string());
    map.insert("auth_session", access_token.to_string());
    map.insert("user_type", user_type.to_string());
    map.insert("user_properties", "{}".to_string());
    map.insert("version_name", version_id.to_string());
    map.insert("version_type", "release".to_string());
    map.insert("game_directory", path_str(&game_dir));
    map.insert("game_dir", path_str(&game_dir));
    map.insert("assets_root", path_str(&assets_dir));
    map.insert("assets_dir_name", asset_index.clone());
    map.insert("assets_index_name", asset_index.clone());
    map.insert("natives_directory", path_str(&natives_dir));
    map.insert("classpath", classpath_string(&classpath));

    // —— 组装完整 JVM + mainClass + 游戏参数 ——
    let mut args: Vec<String> = Vec::new();
    args.push(format!("-Xmx{}M", memory_mb));
    args.push(format!("-Xms{}M", (memory_mb / 4).max(256)));
    args.push(format!("-Djava.library.path={}", path_str(&natives_dir)));
    args.push("-cp".to_string());
    args.push(classpath_string(&classpath));

    for a in &jvm_args {
        args.push(expand(a, &map));
    }
    args.push(main_class.clone());
    // 游戏参数逐项展开后再去重（加载器层 + 基底两层可能定义相同参数）
    let mut seen = std::collections::HashSet::new();
    for a in &game_args {
        let expanded = expand(a, &map);
        if seen.insert(expanded.clone()) {
            args.push(expanded);
        }
    }

    emit_status(app, "starting", &format!("正在启动 {}…（Java: {}）", version_id, java.display()));

    // —— spawn（detached，子进程读 stdout/stderr 转发事件）——
    let mut cmd = Command::new(&java);
    cmd.args(&args);
    cmd.current_dir(&game_dir);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // 记录 spawn 时刻，用于判断“进程几乎是启动即崩”的短存活异常。
    let started_at = std::time::Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("无法启动 Java 进程: {e}")),
    };
    let pid = child.id();
    // 先取出 stdout/stderr 管道句柄，交给读者线程转发；Child 本体存入全局供 stop_game
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *CHILD.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);

    emit_status(app, "running", &format!("游戏已启动（PID {pid}），日志将实时显示"));

    let app_out = app.clone();
    let app_wait = app.clone();
    std::thread::spawn(move || {
        // 读者线程：stdout/stderr 按行转发事件；同时各自保留尾部若干行，
        // 用于进程“启动即崩/异常退出”时把真正的输出塞进 error 弹给前端。
        // 崩溃文本可能走 stdout 也可能走 stderr，故两个管道都收集尾部。
        use std::io::BufRead;
        const MAX_TAIL: usize = 30;
        // 环形尾部缓冲：每条记录带 distinct 序号，输出时按原顺序合并。
        // is_err 标记来源（stdout=false / stderr=true），当前展示不区分，仅供扩展。
        let mut tail: Vec<(u64, bool, String)> = Vec::new();

        if let Some(so) = stdout {
            let reader = std::io::BufReader::new(so);
            for line in reader.lines().flatten() {
                let l = line.trim_end_matches('\r').to_string();
                if !l.is_empty() {
                    emit_output(&app_out, &l);
                    tail.push((tail.len() as u64, false, l));
                    if tail.len() > MAX_TAIL {
                        tail.remove(0);
                    }
                }
            }
        }
        if let Some(se) = stderr {
            let reader = std::io::BufReader::new(se);
            for line in reader.lines().flatten() {
                let l = line.trim_end_matches('\r').to_string();
                if !l.is_empty() {
                    emit_output(&app_out, &l);
                    tail.push((tail.len() as u64, true, l));
                    if tail.len() > MAX_TAIL {
                        tail.remove(0);
                    }
                }
            }
        }
        // 进程退出，从全局取回 Child 回收。注意区分：
        //   · CHILD.take() 为 None → 已被 stop_game 抢先取走 ⇒ 用户点了「停止游戏」或
        //     关闭游戏，属正常停止，永不弹错；
        //   · 取得到 Child ⇒ 游戏是自己退出的，需按退出码/存活时长判定是否异常。
        let mut child = CHILD.lock().unwrap_or_else(|e| e.into_inner()).take();
        let code = child.as_mut().and_then(|c| c.wait().ok()).and_then(|s| s.code());
        RUNNING.store(false, Ordering::SeqCst);

        // 用户主动停止：走 stopped（不弹错）。
        if child.is_none() {
            emit_status(&app_wait, "stopped", &format!("游戏已停止（退出码 {:?}）", code));
            return;
        }

        let alive = started_at.elapsed().as_secs();
        // 异常判定：任何一项触发都视为「启动失败/异常退出」并弹错——
        //   ① 退出码非 0
        //   ② 启动后存活极短（< 30s 即退，基本是启动即崩）且确实打印过内容
        //   ③ 退出码为 0 但 stdout/stderr 里出现明显崩溃关键词
        let tail_text = tail.into_iter().map(|(_, _, l)| l).collect::<Vec<_>>().join("\n");
        let looks_crashy = ["Exception", "Error", "at ", "Caused by", "Unable"]
            .iter()
            .any(|kw| tail_text.contains(kw));
        let abnormal = code.map_or(true, |c| c != 0)
            || (alive < 30)
            || looks_crashy;
        if abnormal {
            let summary = format!("游戏进程启动后异常退出（退出码 {:?}，存活 {}s）", code, alive);
            let detail = if tail_text.is_empty() {
                format!("{summary}。无更多输出。")
            } else {
                format!("{summary}。\n{}", tail_text)
            };
            log_err(&detail);
            // 结构化错误事件：detail 只放一行摘要，尾部/退出码/存活时长走独立字段，
            // 前端报错 UI 据此分开展示「摘要 + 元数据 + 可展开的输出日志」。
            emit_status_full(
                &app_wait,
                "error",
                &summary,
                serde_json::json!({
                    "stage": "crash",
                    "code": code,
                    "aliveSecs": alive,
                    "pid": pid,
                    "tail": tail_text,
                }),
            );
        } else {
            emit_status(&app_wait, "stopped", &format!("游戏进程已退出（退出码 {:?}，运行 {}s）", code, alive));
        }
    });

    Ok(())
}