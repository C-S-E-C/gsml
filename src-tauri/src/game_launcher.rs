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
use crate::java_manager;
use crate::minecraft_manager;

/// 正在运行的游戏进程句柄（全局唯一）。
static RUNNING: AtomicBool = AtomicBool::new(false);
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// 是否有游戏正在启动/运行（供「游戏运行时不测速」等场景判断）。
pub(crate) fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// 前端查询游戏是否在跑（例如「运行中跳过 Java 扫描」的提示）。
#[tauri::command]
pub fn is_game_running() -> bool {
    is_running()
}

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
        rusqlite::Error::QueryReturnedNoRows => format!("Account not found (id={account_id})"),
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
        fs::read_to_string(&path).map_err(|e| format!("Failed to read version JSON {id}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("Failed to parse version JSON {id}: {e}"))
}

/// 单个参数条目是否在当前平台/特性下启用（rules ± features）。
/// os 缺省 = 适用；features 只判断已知 bool 键，未知按 false 处理。
fn arg_allowed(entry: &Value) -> bool {
    let Some(rules) = entry["rules"].as_array() else { return true };
    let mut allowed = false;
    for r in rules {
        let os_ok = r["os"]["name"].as_str().map_or(true, |n| n == "windows");
        // os.arch 只在部分 Mojang 规则出现（x86 专用参数）；架构不匹配则规则不生效。
        let arch_ok = match r["os"]["arch"].as_str() {
            None => true,
            Some("x86") => cfg!(target_arch = "x86"),
            Some("x86_64") | Some("amd64") => cfg!(target_arch = "x86_64"),
            Some("arm") | Some("arm32") => cfg!(target_arch = "arm"),
            Some("arm64") | Some("aarch64") => cfg!(target_arch = "aarch64"),
            Some(_) => true,
        };
        let feat_ok = match r["features"].as_object() {
            // 本启动器不启用任何 feature（demo/自定义分辨率/quick play），
            // 因此仅当规则要求的所有特性都为“不需要(=false)”时该参数才生效。
            None => true,
            Some(f) => f
                .iter()
                .all(|(_k, v)| !v.as_bool().unwrap_or(false)),
        };
        if !(os_ok && arch_ok && feat_ok) {
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
    server: Option<String>,
) -> Result<(), String> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("A game is already starting or running; stop it first".to_string());
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
                server.as_deref(),
            )
        }));
        let result = match launch_res {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_panic) => Err("Internal launcher error (background task aborted)".to_string()),
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

/// 当前平台在 version JSON `natives` 映射里的键名（windows / linux / osx）。
fn native_os_key() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

/// 某库条目在当前平台对应的 natives 分类 jar 相对路径（非 natives 库返回 None）。
fn native_jar_rel(lib: &Value) -> Option<String> {
    let classifier = lib["natives"][native_os_key()].as_str()?;
    if let Some(p) = lib["downloads"]["classifiers"][classifier]["path"].as_str() {
        return Some(p.to_string());
    }
    // 只有 Maven 坐标的第三方条目：group:artifact:version:classifier
    maven_rel_path(&format!("{}:{classifier}", lib["name"].as_str()?))
}

/// 是否是当前平台的原生库文件（Windows .dll / macOS .dylib|.jnilib / Linux .so）。
fn is_native_lib(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    if cfg!(windows) {
        ext == "dll"
    } else if cfg!(target_os = "macos") {
        ext == "dylib" || ext == "jnilib"
    } else {
        ext == "so"
    }
}

/// natives 目录里是否已经有本地库。
fn dir_has_natives(dir: &Path) -> bool {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| is_native_lib(&e.file_name().to_string_lossy()))
        })
        .unwrap_or(false)
}

/// 把 natives 分类 jar 里的本地库解压到 natives_dir（已存在的文件跳过）。
/// 有些版本是别的启动器装的（例如 PCL2 把 DLL 放在 `<版本>-natives/`），
/// 只认我们自己的 natives 目录会拿到空目录 → LWJGL2 报
/// "no lwjgl64 in java.library.path"，所以以 libraries 里的 jar 为准补齐。
fn extract_natives_from_jars(jars: &[PathBuf], natives_dir: &Path) -> Result<(), String> {
    for jar in jars {
        let file = fs::File::open(jar)
            .map_err(|e| format!("Failed to open natives jar {}: {e}", jar.display()))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Failed to read natives jar {}: {e}", jar.display()))?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Failed to read natives jar entry: {e}"))?;
            // zip 9.x 的 ZipFile::name() 返回 Result<Cow<str>, ZipError>
            let name = entry
                .name()
                .map_err(|e| format!("Failed to read jar entry name: {e}"))?
                .to_string();
            if name.ends_with('/') || name.contains("META-INF/") {
                continue; // 目录 / Mojang 的 extract.exclude
            }
            let Some(base) = name.rsplit('/').next() else {
                continue;
            };
            if base.is_empty() || !is_native_lib(base) {
                continue;
            }
            let dest = natives_dir.join(base);
            if dest.is_file() {
                continue; // 已解压过（可能是别的启动器放的）
            }
            let mut out = fs::File::create(&dest)
                .map_err(|e| format!("Failed to write {}: {e}", dest.display()))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| format!("Failed to extract {base} into natives: {e}"))?;
        }
    }
    Ok(())
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

/// 版本字符串 → 主版本号：`21.0.4` → 21，`17` → 17，`1.8.0_391` → 8。
fn java_major(version: &str) -> Option<u32> {
    let mut it = version.split(|c: char| matches!(c, '.' | '_' | '-' | '+'));
    let first = it.next()?.trim().parse::<u32>().ok()?;
    if first == 1 {
        // 老式 "1.M.m_u" 命名：主版本在第二段
        it.next()?.trim().parse::<u32>().ok()
    } else {
        Some(first)
    }
}

/// 版本链要求的 Java 主版本（javaVersion.majorVersion，加载器层优先）。
fn required_java_major(chain: &[(String, Value)]) -> Option<u32> {
    chain
        .iter()
        .find_map(|(_, json)| json["javaVersion"]["majorVersion"].as_u64())
        .map(|m| m as u32)
}

/// 依据版本链的 javaVersion 要求选择 Java：
/// 1) 版本没有要求 → 维持原 find_java 逻辑（显式路径 > JAVA_HOME > PATH）；
/// 2) 有要求：显式 javaPath 探测主版本匹配则直接用（不匹配不采用，版本要求
///    优先于全局固定 Java）；否则在本机扫描列表里找主版本相等的安装，其次
///    取主版本 ≥ 要求的最小者；都探不出版本再退回原 find_java 兜底。
/// 返回 (java 可执行文件, 附加说明)，说明会拼进 starting 状态文案。
fn select_java(
    java_path: Option<&str>,
    chain: &[(String, Value)],
) -> Result<(PathBuf, Option<String>), String> {
    let Some(required) = required_java_major(chain) else {
        return Ok((minecraft_manager::find_java(java_path)?, None));
    };

    // 候选：显式 javaPath（若给出且存在）排最前，其后是本机扫描结果
    //（scan 按「默认优先、版本新→旧」排序，同主版本天然取较新者）。
    let mut candidates: Vec<(PathBuf, String, Option<u32>)> = Vec::new();
    if let Some(p) = java_path.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(p);
        let exe = if path.is_file() {
            path
        } else {
            let exe = path.join(java_exe_name());
            if !exe.is_file() {
                return Err(format!("Specified Java path does not exist: {p}"));
            }
            exe
        };
        let ver = java_manager::probe_java(&exe).and_then(|(v, _)| java_major(&v));
        candidates.push((exe, "Manual".to_string(), ver));
    }
    for rt in java_manager::scan_java_installations() {
        candidates.push((
            PathBuf::from(&rt.path),
            rt.source.clone(),
            java_major(&rt.version),
        ));
    }

    // 主版本精确匹配优先。
    if let Some((exe, source, Some(m))) =
        candidates.iter().find(|(_, _, v)| *v == Some(required))
    {
        return Ok((
            exe.clone(),
            Some(format!(", {source}, Java {m} matches this version's requirement")),
        ));
    }
    // 其次：主版本 ≥ 要求的最小者（新 Java 通常可运行旧版本，仅作兜底）。
    if let Some((exe, source, Some(m))) = candidates
        .iter()
        .filter(|(_, _, v)| v.is_some_and(|x| x > required))
        .min_by_key(|(_, _, v)| v.unwrap_or(u32::MAX))
    {
        return Ok((
            exe.clone(),
            Some(format!(
                ", {source}, no Java {required} installed; using Java {m}"
            )),
        ));
    }
    // 扫描一无所获 / 都探不出版本 → 原逻辑兜底（错误文案沿用旧实现）。
    minecraft_manager::find_java(java_path).map(|p| (p, None))
}

fn java_exe_name() -> &'static str {
    if cfg!(windows) {
        "java.exe"
    } else {
        "java"
    }
}

/// 从版本名解析主/次版本号（"1.20.1" → (1,20)；快照/非标准返回 None）。
fn version_major_minor(version_id: &str) -> Option<(u32, u32)> {
    let core = version_id
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|s| !s.is_empty())?;
    let mut parts = core.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

/// 「启动即进服」参数。1.20+ 用 `--quickPlayMultiplayer host:port`，
/// 更早的版本用 `--server host --port port`（后者在 1.20 起不再生效）。
/// 版本号优先用基底版本（effective_base），这样重命名过的整合包也能判断对。
fn multiplayer_args(version_id: &str, server: &str) -> Vec<String> {
    let (host, port) = match server.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.to_string())
        }
        _ => (server.to_string(), "25565".to_string()),
    };
    if host.is_empty() {
        return Vec::new();
    }
    // 1.20+ 与快照（24w14a / 26.2-snapshot-1）走 quickPlay；老版本走 --server
    let modern = match version_major_minor(version_id) {
        Some((1, minor)) => minor >= 20,
        Some((major, _)) => major > 1,
        None => false,
    };
    if modern {
        vec!["--quickPlayMultiplayer".to_string(), format!("{host}:{port}")]
    } else {
        vec![
            "--server".to_string(),
            host,
            "--port".to_string(),
            port,
        ]
    }
}

fn launch_blocking(
    app: &AppHandle,
    version_id: &str,
    username: &str,
    uuid: &str,
    access_token: &str,
    java_path: Option<&str>,
    memory_mb: u32,
    server: Option<&str>,
) -> Result<(), String> {
    let root = minecraft_manager::get_minecraft_dir()?;

    // —— 合并版本信息（先读链：Java 选择依赖版本 JSON 的 javaVersion 要求）——
    let chain = collect_chain(&root, version_id)?;
    let (java, java_note) = select_java(java_path, &chain)?;
    // chain 从启动目标层一路向下到最底层基底（无 inheritsFrom）→ 基底是最后一个
    let effective_base = chain.last().unwrap().0.clone();
    let mut main_class = String::new();
    let mut asset_index = String::new();
    let mut jvm_args: Vec<String> = Vec::new();
    let mut game_args: Vec<String> = Vec::new();
    let mut classpath: Vec<PathBuf> = Vec::new();
    // 当前平台需要的 natives 分类 jar（存在/缺失分开记，供解压与报错用）
    let mut native_jars: Vec<PathBuf> = Vec::new();
    let mut missing_native_jars: Vec<String> = Vec::new();

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
                // LWJGL2 等旧版本的本地库在 natives 分类 jar 里，
                // 稍后统一解压到 natives 目录（-Djava.library.path）。
                if let Some(rel) = native_jar_rel(lib) {
                    let abs = root.join("libraries").join(&rel);
                    if abs.is_file() {
                        native_jars.push(abs);
                    } else {
                        missing_native_jars.push(rel);
                    }
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

    // 同一个库可能被列多次：NeoForge 21.1.x 的 JSON 内部就重复列了 gson，
    // 基底（1.21.1）又列了一次。重复的 jar 会让 BootStrapLauncher 构造
    // UnionFileSystem 时以 jar 路径为键撞键，直接抛
    // "Duplicate key ...jar (attempted merging values ...)"，
    // 所以拼 -cp 之前按路径去重（保留先出现的一层）。
    let mut cp_seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    classpath.retain(|p| cp_seen.insert(p.clone()));

    if main_class.is_empty() {
        return Err(format!("Version {version_id} is missing mainClass"));
    }
    if asset_index.is_empty() {
        return Err(format!("Version {version_id} is missing assetIndex"));
    }

    let natives_dir = root
        .join("versions")
        .join(&effective_base)
        .join("natives");
    fs::create_dir_all(&natives_dir).map_err(|e| format!("Failed to create natives directory: {e}"))?;
    // 别的启动器装的版本常常把 DLL 解压在自己命名的目录里（PCL2 就是
    // `<版本>-natives/`），我们的 natives 目录会是空的。这里从 libraries
    // 里的 natives jar 直接补齐，避免 LWJGL2 版本一启动就
    // "no lwjgl64 in java.library.path"。
    extract_natives_from_jars(&native_jars, &natives_dir)?;
    if !dir_has_natives(&natives_dir) {
        if !missing_native_jars.is_empty() {
            return Err(format!(
                "Missing native libraries: {}. Reinstall this version to restore them.",
                missing_native_jars.join(", ")
            ));
        }
        if !native_jars.is_empty() {
            return Err(
                "Found natives jars but extracted no native libraries; check the libraries directory".to_string(),
            );
        }
    }
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
    // Forge/NeoForge 1.17+ 的 jvm 参数依赖这些 token：-p 用 library_directory
    // 指向 libraries（--add-opens/--add-exports 的目标模块
    // cpw.mods.securejarhandler 也在其中）。缺了它们模块路径会变成字面量
    // "${library_directory}"，securejarhandler 加载失败，IMPL_LOOKUP 反射
    // 抛 InaccessibleObjectException —— 表现为启动约 4 秒即崩（退出码 1）。
    map.insert("library_directory", path_str(&root.join("libraries")));
    map.insert(
        "classpath_separator",
        if cfg!(windows) { ";" } else { ":" }.to_string(),
    );
    map.insert("launcher_name", "gsml".to_string());
    map.insert(
        "launcher_version",
        env!("CARGO_PKG_VERSION").to_string(),
    );

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
    // 「启动即进服」：deeplink start-game 传入的 server（用基底版本判断参数形态）
    if let Some(server) = server.map(str::trim).filter(|s| !s.is_empty()) {
        args.extend(multiplayer_args(&effective_base, server));
    }

    let note = java_note.as_deref().unwrap_or("");
    emit_status(
        app,
        "starting",
        &format!("Launching {version_id}… (Java: {}{note})", java.display()),
    );

    // —— spawn（detached，子进程读 stdout/stderr 转发事件）——
    let mut cmd = Command::new(&java);
    cmd.args(&args);
    cmd.current_dir(&game_dir);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // 用 java.exe 启动时不要弹控制台窗口（输出已经通过管道转成 game-output 事件）
    crate::proc_util::hide_console(&mut cmd);
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    // 记录 spawn 时刻，用于判断“进程几乎是启动即崩”的短存活异常。
    let started_at = std::time::Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to start Java process: {e}")),
    };
    let pid = child.id();
    // 先取出 stdout/stderr 管道句柄，交给读者线程转发；Child 本体存入全局供 stop_game
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *CHILD.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);

    emit_status(app, "running", &format!("Game started (PID {pid}); logs will stream here"));

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
        // 退出码展示统一成纯数字（Option 的 {:?} 会显示成 Some(1)/None）。
        let code_text = code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // 用户主动停止：走 stopped（不弹错）。
        if child.is_none() {
            emit_status(&app_wait, "stopped", &format!("Game stopped (exit code {code_text})"));
            return;
        }

        let alive = started_at.elapsed().as_secs();
        // 异常判定只看退出码，不再叠加启发式：
        //   · Some(0) → 正常退出（玩家自己关闭游戏），绝不弹错；
        //   · Some(非 0) → 异常退出（如 KnotClient 崩溃时的退出码 1）；
        //   · None（进程被信号终止/强杀，取不到退出码）→ 视为异常。
        // 此前还叠加「存活 < 30s 即崩」和「输出含 Exception/Error 等关键词即崩」
        // 两条启发式，但 Minecraft 正常运行的日志里几乎必然出现这些词
        // （log4j 的 ERROR 行、退出时音效引擎/线程中断的堆栈等），
        // 曾导致玩家正常关闭游戏也被误判成崩溃弹窗。
        let tail_text = tail.into_iter().map(|(_, _, l)| l).collect::<Vec<_>>().join("\n");
        let abnormal = code.map_or(true, |c| c != 0);
        if abnormal {
            let summary = format!("Game process exited unexpectedly (exit code {code_text}, alive {alive}s)");
            let detail = if tail_text.is_empty() {
                format!("{summary}. No further output.")
            } else {
                format!("{summary}.\n{}", tail_text)
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
            emit_status(&app_wait, "stopped", &format!("Game process exited (exit code {code_text}, ran {alive}s)"));
        }
    });

    Ok(())
}