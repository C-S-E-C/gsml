// ── 自动更新（GitHub Release）───────────────────────────────────────────
// 前端 settings.js / updater.js 调用这里的三个命令：
//   · check_update    → 拉 releases 列表，按当前渠道（is_debug → 允许
//                       prerelease）选目标版本，按平台挑 asset；
//   · download_update → 带进度事件下载选中 asset 到临时目录；
//   · apply_update    → 按安装形态落地：便携包走 taskkill/rm/mv 自替换并
//                       重启；安装器直接拉起安装程序；dmg/deb 由系统打开。
use std::path::PathBuf;
use std::process::Command;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::minecraft_manager::http_client;

const DEFAULT_API_BASE: &str = "https://api.github.com/repos/C-S-E-C/gsml/releases";

/// 资产安装形态：决定 apply_update 的落地方式
#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)] // SelfReplace 仅在 Linux 分支构造，Windows 构建下视为未用
pub enum InstallMode {
    /// 便携单文件（AppImage 等）：结束当前进程 → rm 旧 → mv 新 → 重启
    SelfReplace,
    /// 安装器（NSIS setup.exe）：退出当前程序并拉起安装器
    Installer,
    /// 仅下载并由系统打开（dmg / deb / rpm），安装交给用户
    Manual,
}

#[derive(Serialize)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub is_prerelease: bool,
    pub notes: String,
    pub asset_name: String,
    pub asset_url: String,
    pub asset_size: u64,
    pub mode: InstallMode,
    /// true → 本地已是最新（前端据此提示，无需下载）
    pub up_to_date: bool,
}

/// 进度事件负载（对齐 minecraft_manager 的 install-progress 风格，事件名独立）
#[derive(Serialize, Clone)]
struct UpdateProgress<'a> {
    stage: &'a str,
    done: u64,
    total: u64,
    label: &'a str,
}

fn emit(app: &AppHandle, stage: &str, done: u64, total: u64, label: &str) {
    let _ = app.emit(
        "update-progress",
        UpdateProgress { stage, done, total, label },
    );
}

/// x.y.z 三段版本比较（release tag 可能带 v 前缀或后缀，先剥掉）
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let digits: String = text
        .trim()
        .trim_start_matches(['v', 'V'])
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = digits.split('.');
    let a = parts.next()?.parse().ok()?;
    let b = parts.next().unwrap_or("0").parse().ok()?;
    let c = parts.next().unwrap_or("0").parse().ok()?;
    Some((a, b, c))
}

/// 本地版本是否 ≥ 目标版本；解析不出时保守返回 false（允许更新）
fn version_at_least(current: &str, target: &str) -> bool {
    match (parse_version(current), parse_version(target)) {
        (Some(c), Some(t)) => c >= t,
        _ => false,
    }
}

/// 把配置的源展开成候选 URL 列表（可多行 / 逗号分隔）。
/// URL 一律**原样使用**，不再做 github.com → api.github.com 之类的改写：
/// UpdateSource.url 里直接填完整的 releases 接口，例如
///   https://api.github.com/repos/owner/repo/releases
/// 只有 `append_per_page` 打开且地址里没有 `?` 时才追加 ?per_page=20。
fn api_candidates(server_base: Option<&str>, append_per_page: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(raw) = server_base {
        for part in raw
            .split(|c: char| c == '\n' || c == '\r' || c == ',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let url = part.trim_end_matches('/').to_string();
            let api = if url.contains('?') || !append_per_page {
                url
            } else {
                format!("{url}?per_page=20")
            };
            if !out.contains(&api) {
                out.push(api);
            }
        }
    }
    if out.is_empty() {
        let url = DEFAULT_API_BASE.trim_end_matches('/').to_string();
        out.push(if append_per_page {
            format!("{url}?per_page=20")
        } else {
            url
        });
    }
    out
}

/// 请求一个源的 release 列表（带超时），返回 (api, JSON)。
async fn fetch_releases(api: &str) -> Result<(String, serde_json::Value), String> {
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(12),
        http_client()
            .get(api)
            .header("User-Agent", "gsml-updater")
            .header("Accept", "application/vnd.github+json")
            .send(),
    )
    .await
    .map_err(|_| format!("{api} request timed out"))?
    .map_err(|e| format!("{api} request failed: {e}"))?
    .error_for_status()
    .map_err(|e| format!("{api} request failed: {e}"))?;
    let body = tokio::time::timeout(std::time::Duration::from_secs(12), resp.text())
        .await
        .map_err(|_| format!("{api} read timed out"))?
        .map_err(|e| format!("{api} read failed: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("{api} parse failed: {e}"))?;
    Ok((api.to_string(), value))
}

/// 竞速多个更新源：所有源同时开始，第一个「响应且解析成功」的胜出，
/// 其余还没完成的直接丢弃（接收端关闭后任务自行结束）。
async fn race_releases(candidates: Vec<String>) -> Result<serde_json::Value, String> {
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<Result<serde_json::Value, String>>(candidates.len().max(1));
    for api in candidates {
        let tx = tx.clone();
        tauri::async_runtime::spawn(async move {
            let res = fetch_releases(&api).await.map(|(_, value)| value);
            let _ = tx.send(res).await;
        });
    }
    drop(tx);
    let mut errors: Vec<String> = Vec::new();
    while let Some(res) = rx.recv().await {
        match res {
            Ok(value) => return Ok(value),
            Err(e) => errors.push(e),
        }
    }
    Err(format!("All update sources are unavailable: {}", errors.join("; ")))
}

/// 一个更新源（仓库地址或 releases API 均可，内部统一归一化）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UpdateSource {
    pub id: String,
    pub name: String,
    pub url: String,
    /// 是否在请求地址后追加 `?per_page=20`。GitHub 官方 API 建议加（只取最近 20 条）；
    /// 自建/镜像接口若已有自己的查询串或不支持该参数，可关掉。
    #[serde(default = "default_append_per_page")]
    pub append_per_page: bool,
}

fn default_append_per_page() -> bool {
    true
}

pub fn default_update_sources() -> Vec<UpdateSource> {
    vec![
        UpdateSource {
            id: "github".to_string(),
            name: "GitHub".to_string(),
            url: DEFAULT_API_BASE.to_string(),
            append_per_page: true,
        },
        UpdateSource {
            id: "cloudflare".to_string(),
            name: "Cloudflare".to_string(),
            url: "https://packages.r2.syntropica.top/top/syntropica/apps/gsml/releases.json".to_string(),
            append_per_page: false,
        },
    ]
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateProbeEntry {
    pub id: String,
    pub name: String,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub chosen: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UpdateProbeResult {
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub latency_ms: Option<u64>,
    pub entries: Vec<UpdateProbeEntry>,
    pub error: Option<String>,
}

struct ApiOutcome {
    src: UpdateSource,
    latency: Option<u64>,
    error: Option<String>,
    value: Option<serde_json::Value>,
}

fn update_entries(outcomes: &[ApiOutcome], chosen: Option<&str>) -> Vec<UpdateProbeEntry> {
    let mut entries: Vec<UpdateProbeEntry> = outcomes
        .iter()
        .map(|o| UpdateProbeEntry {
            id: o.src.id.clone(),
            name: o.src.name.clone(),
            latency_ms: o.latency,
            error: o.error.clone(),
            chosen: chosen.is_some_and(|c| c == o.src.id),
        })
        .collect();
    entries.sort_by_key(|e| (e.latency_ms.is_none(), e.latency_ms.unwrap_or(u64::MAX)));
    entries
}

/// 同时给所有更新源测速并解析 release 列表：先响应且解析成功的胜出，
/// 其余任务在接收端关闭后自行作废。
#[tauri::command]
pub async fn probe_update_sources(sources: Vec<UpdateSource>) -> Result<UpdateProbeResult, String> {
    if sources.is_empty() {
        return Err("The update source list is empty".to_string());
    }
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<ApiOutcome>(sources.len().max(1));
    for src in sources {
        let tx = tx.clone();
        tauri::async_runtime::spawn(async move {
            // 地址原样使用：UpdateSource.url 就是完整的 releases 接口
            let base = src.url.trim().trim_end_matches('/');
            let api = if base.contains('?') || !src.append_per_page {
                base.to_string()
            } else {
                format!("{base}?per_page=20")
            };
            let started = std::time::Instant::now();
            let fetched = fetch_releases(&api).await;
            let elapsed = started.elapsed().as_millis() as u64;
            let mut outcome = ApiOutcome {
                src,
                latency: None,
                error: None,
                value: None,
            };
            match fetched {
                Ok((_, value)) => {
                    outcome.latency = Some(elapsed);
                    outcome.value = Some(value);
                }
                Err(e) => outcome.error = Some(e),
            }
            let _ = tx.send(outcome).await;
        });
    }
    drop(tx);

    let mut outcomes: Vec<ApiOutcome> = Vec::new();
    while let Some(outcome) = rx.recv().await {
        if outcome.value.is_some() {
            let mut res = UpdateProbeResult {
                source_id: Some(outcome.src.id.clone()),
                source_name: Some(outcome.src.name.clone()),
                latency_ms: outcome.latency,
                entries: Vec::new(),
                error: None,
            };
            outcomes.push(ApiOutcome { value: None, ..outcome });
            res.entries = update_entries(&outcomes, res.source_id.as_deref());
            return Ok(res);
        }
        outcomes.push(outcome);
    }

    let entries = update_entries(&outcomes, None);
    let detail = entries
        .iter()
        .map(|e| {
            format!(
                "{} ({})",
                e.name,
                e.error.clone().unwrap_or_else(|| "no response".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    Ok(UpdateProbeResult {
        source_id: None,
        source_name: None,
        latency_ms: None,
        entries,
        error: Some(format!("All update sources are unavailable: {detail}")),
    })
}

/// 前端首次使用时读取内置默认更新源。
#[tauri::command]
pub fn default_update_sources_cmd() -> Vec<UpdateSource> {
    default_update_sources()
}

/// 按当前平台从 assets 里挑目标文件，返回 (url, name, size, mode)
fn pick_asset(assets: &[serde_json::Value]) -> Result<(String, String, u64, InstallMode), String> {
    #[cfg(target_os = "windows")]
    let candidates: &[(&str, InstallMode)] = &[
        // 优选便携单文件（可直接 taskkill/rm/mv 自替换）…
        ("gsml.exe", InstallMode::SelfReplace),
        // …否则退回安装器（NSIS 升级安装最稳，msi 兜底）
        ("x64-setup.exe", InstallMode::Installer),
        (".exe", InstallMode::Installer),
        (".msi", InstallMode::Installer),
    ];
    #[cfg(target_os = "linux")]
    let candidates: &[(&str, InstallMode)] = &[
        (".AppImage", InstallMode::SelfReplace), // 便携包，可整文件自替换
        (".deb", InstallMode::Manual),
        (".rpm", InstallMode::Manual),
    ];
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, InstallMode)] = &[(".dmg", InstallMode::Manual)];

    for (suffix, mode) in candidates {
        if let Some(a) = assets.iter().find(|a| {
            a["name"]
                .as_str()
                .map(|n| n.to_ascii_lowercase().ends_with(suffix))
                == Some(true)
        }) {
            return Ok((
                a["browser_download_url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                a["name"].as_str().unwrap_or_default().to_string(),
                a["size"].as_u64().unwrap_or(0),
                *mode,
            ));
        }
    }
    Err("No release asset available for this platform".into())
}

#[tauri::command]
pub async fn check_update(
    app: AppHandle,
    server_base: Option<String>,
    append_per_page: Option<bool>,
) -> Result<UpdateCheck, String> {
    // 渠道判定：debug_assertions 覆盖 `tauri build --debug`（is_dev 只对
    // `tauri dev` 为 true）；debug/开发构建跟随 prerelease，正式构建只看稳定版
    let allow_prerelease =
        cfg!(debug_assertions) || tauri::is_dev();
    let current_version = app.package_info().version.to_string();

    // 多个更新源同时竞速：谁先响应且解析成功就用谁（其余丢弃）。
    let releases = race_releases(api_candidates(
        server_base.as_deref(),
        append_per_page.unwrap_or(true),
    ))
    .await?;

    let list = releases.as_array().ok_or("Unexpected release list format")?;
    let release = list
        .iter()
        .find(|r| {
            r["draft"].as_bool() != Some(true)
                && (allow_prerelease || r["prerelease"].as_bool() != Some(true))
        })
        .ok_or("No usable release in the repository")?;

    let latest_version = release["tag_name"].as_str().unwrap_or_default().to_string();
    let is_prerelease = release["prerelease"].as_bool() == Some(true);
    let notes = release["body"].as_str().unwrap_or_default().to_string();

    if version_at_least(&current_version, &latest_version) {
        return Ok(UpdateCheck {
            current_version,
            latest_version: latest_version.trim_start_matches(['v', 'V']).to_string(),
            is_prerelease,
            notes,
            asset_name: String::new(),
            asset_url: String::new(),
            asset_size: 0,
            mode: InstallMode::Manual,
            up_to_date: true,
        });
    }

    let assets = release["assets"].as_array().cloned().unwrap_or_default();
    let (asset_url, asset_name, asset_size, mode) = pick_asset(&assets)?;

    Ok(UpdateCheck {
        current_version,
        latest_version: latest_version.trim_start_matches(['v', 'V']).to_string(),
        is_prerelease,
        notes,
        asset_name,
        asset_url,
        asset_size,
        mode,
        up_to_date: false,
    })
}

/// 下载更新包到临时目录，经 "update-progress" 事件回报进度
#[tauri::command]
pub async fn download_update(
    app: AppHandle,
    url: String,
    name: String,
) -> Result<PathBuf, String> {
    if url.is_empty() || name.is_empty() {
        return Err("Missing download URL or file name".into());
    }
    let dest = std::env::temp_dir().join(format!("gsml-update-{}", sanitize_name(&name)));
    let client = http_client();
    let resp = client
        .get(&url)
        .header("User-Agent", "gsml-updater")
        .send()
        .await
        .map_err(|e| format!("Failed to connect to the download source: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Download failed: {e}"))?;

    let total = resp.content_length().unwrap_or(0);
    emit(&app, "download", 0, total.max(1), "Starting download");

    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| format!("Failed to create the temp file: {e}"))?;
    use tokio::io::AsyncWriteExt;

    let mut done: u64 = 0;
    let mut last_pct: u64 = 100;
    let mut stream = resp;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|e| format!("Download interrupted: {e}"))?
    {
        file.write_all(&chunk).await.map_err(|e| format!("Write failed: {e}"))?;
        done += chunk.len() as u64;
        let pct = (done * 100).checked_div(total.max(1)).unwrap_or(100);
        if pct != last_pct {
            last_pct = pct;
            emit(&app, "download", done, total.max(1), &format!("Downloading {pct}%"));
        }
    }
    file.flush().await.map_err(|e| format!("Write failed: {e}"))?;
    drop(file);

    emit(&app, "download", done, total.max(1), "Download complete");
    Ok(dest)
}

/// 文件名里去掉路径分隔符与 Windows 非法字符
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// 应用更新：按 asset 形态落地，成功后本进程退出
#[tauri::command]
pub async fn apply_update(app: AppHandle, file: String) -> Result<(), String> {
    let path = PathBuf::from(&file);
    if !path.is_file() {
        return Err("The update package does not exist; download it again".into());
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW | DETACHED_PROCESS：脚本窗口不闪现
        const FLAGS: u32 = 0x0800_0000 | 0x0000_0008;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.ends_with(".msi") {
            // MSI：静默安装，安装器自己处理替换
            Command::new("msiexec")
                .args(["/i", &file, "/quiet", "/norestart"])
                .creation_flags(FLAGS)
                .spawn()
                .map_err(|e| format!("Failed to launch the installer: {e}"))?;
        } else if name.ends_with("-setup.exe") {
            // NSIS setup.exe：自带升级逻辑，直接拉起；/S 静默
            Command::new(&file)
                .arg("/S")
                .creation_flags(FLAGS)
                .spawn()
                .map_err(|e| format!("Failed to launch the installer: {e}"))?;
        } else {
            // 便携包自替换：分离的 cmd 脚本里 taskkill 本进程 → rm 旧 →
            // mv 新 → 重启。
            // · 不用 /T：/T 会连进程树里的 cmd.exe 一起杀，脚本第一行就
            //   自杀；/IM 本身只按映像名匹配，cmd.exe 不会命中。
            // · 单行 & 链 + raw_arg：避免 Rust 对参数的引号转义被 cmd /C
            //   剥掉后打乱语法。
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let current = exe.to_string_lossy().into_owned();
            let exe_name = exe
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("gsml.exe")
                .to_string();
            let script = format!(
                "taskkill /F /IM \"{exe_name}\" & timeout /t 1 /nobreak >nul & del /f /q \"{current}\" & move /y \"{file}\" \"{current}\" & start \"\" \"{current}\"",
            );
            Command::new("cmd")
                .raw_arg("/C")
                .raw_arg(&script)
                .creation_flags(FLAGS)
                .spawn()
                .map_err(|e| format!("Failed to launch the replace script: {e}"))?;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let Some(current) = exe.to_str().map(str::to_string) else {
            return Err("Cannot locate the current executable path".into());
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.ends_with(".appimage") {
            // 便携自替换：新进程里 sleep 等 h 托管进程退出 → rm 旧 → mv 新 → 重启
            let script = format!(
                "sleep 2; chmod +x \"$1\"; rm -f \"$2\"; mv \"$1\" \"$2\"; nohup \"$2\" >/dev/null 2>&1 &",
            );
            Command::new("sh")
                .args(["-c", &script, "--", &file, &current])
                .spawn()
                .map_err(|e| format!("Failed to launch the replace script: {e}"))?;
        } else {
            // deb/rpm：交给系统安装器（图形化 front-end 会弹窗）
            Command::new("xdg-open")
                .arg(&file)
                .spawn()
                .map_err(|e| format!("Failed to open the installer package: {e}"))?;
            // 交给用户手动安装时不能退出自己
            return Ok(());
        }
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(&file)
            .spawn()
            .map_err(|e| format!("Failed to open the dmg: {e}"))?;
        // dmg 拖拽安装是手动的，保留当前进程
        return Ok(());
    }

    // 自替换/安装器路径：给脚本/安装器一点启动时间后退出本进程
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    emit(&app, "apply", 1, 1, "Applying update…");
    // 直接终止本进程；替换脚本/安装器已在独立的分离进程里运行
    std::process::exit(0);
    #[allow(unreachable_code)]
    Ok(())
}
