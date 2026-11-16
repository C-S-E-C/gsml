//! Java 运行时自动安装（Adoptium 官方 API + Adoptium 目录镜像）。
//!
//! 安装 MC 版本时按版本 JSON 的 `javaVersion.majorVersion` 自动补齐对应大版本
//! 的 JDK/JRE；设置页也可以「一键给所有版本安装 Java」。
//!
//! 镜像源（带类型，不含 isoinfo）：
//!   · api      → Adoptium v3 API 根，如 https://api.adoptium.net
//!   · adoptium → Adoptium 发行包目录根，如 https://mirrors.tuna.tsinghua.edu.cn/Adoptium
//!                解析 `{root}/{major}/{jre|jdk}/{arch}/{os}/` 目录列表里的 zip
//!
//! 下载的运行时解压并**拍平**到 `<安装根>/<major>-<jdk|jre>/bin/java.exe`，
//! java_manager 的扫描会把托管目录当作普通安装列出（source = "Managed"），
//! 启动流程按版本要求自动选中。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::db_handler::data_root;
use crate::java_manager;
use crate::minecraft_manager::http_client;

/// 官方 Adoptium API 根（type = "api" 的默认源）
const OFFICIAL_API_BASE: &str = "https://api.adoptium.net";

/// Java 镜像源类型。
///   · adoptium —— Adoptium 发行包目录镜像根（如 TUNA 的 /Adoptium/），
///                 由 `{root}/{major}/{kind}/{arch}/{os}/` 目录列表里挑 zip
///   · api      —— Adoptium v3 API 根（如 https://api.adoptium.net）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JavaMirror {
    pub url: String,
    #[serde(default = "default_mirror_kind")]
    pub kind: String,
}

fn default_mirror_kind() -> String {
    "adoptium".to_string()
}

#[derive(Debug, Clone)]
pub struct JavaInstallConfig {
    /// 安装新版本时自动补齐 Java
    pub auto: bool,
    /// 优先 JRE（false，默认）还是 JDK
    pub prefer_jdk: bool,
    /// 镜像列表（为空用内置默认）
    pub mirrors: Vec<JavaMirror>,
}

static CONFIG: OnceLock<Mutex<JavaInstallConfig>> = OnceLock::new();

fn config_slot() -> &'static Mutex<JavaInstallConfig> {
    CONFIG.get_or_init(|| {
        Mutex::new(JavaInstallConfig {
            auto: true,
            prefer_jdk: false,
            mirrors: Vec::new(),
        })
    })
}

fn config() -> JavaInstallConfig {
    match config_slot().lock() {
        Ok(mut c) => c.clone(),
        Err(e) => e.into_inner().clone(),
    }
}

/// 安装新版本时是否要自动补 Java。
pub fn auto_enabled() -> bool {
    config().auto
}

/// 前端保存设置后调用一次（install_version 内部也读这份配置）。
#[tauri::command]
pub fn configure_java_installer(
    auto: bool,
    prefer_jdk: bool,
    mirrors: Vec<JavaMirror>,
) {
    if let Ok(mut slot) = config_slot().lock() {
        *slot = JavaInstallConfig {
            auto,
            prefer_jdk,
            mirrors: mirrors
                .into_iter()
                .filter(|m| !m.url.trim().is_empty())
                .map(|m| JavaMirror {
                    url: m.url.trim().to_string(),
                    kind: if m.kind.trim().is_empty() {
                        default_mirror_kind()
                    } else {
                        m.kind.trim().to_ascii_lowercase()
                    },
                })
                .collect(),
        };
    }
}

fn mirrors(cfg: &JavaInstallConfig) -> Vec<JavaMirror> {
    if cfg.mirrors.is_empty() {
        default_java_mirror_entries()
    } else {
        cfg.mirrors.clone()
    }
}

/// 内置默认镜像（带类型）：官方 API + Adoptium 目录镜像。
fn default_java_mirror_entries() -> Vec<JavaMirror> {
    vec![
        JavaMirror {
            url: OFFICIAL_API_BASE.to_string(),
            kind: "api".to_string(),
        },
        JavaMirror {
            url: "https://packages.adoptium.net".to_string(),
            kind: "adoptium".to_string(),
        },
        JavaMirror {
            url: "https://mirrors.tuna.tsinghua.edu.cn/Adoptium".to_string(),
            kind: "adoptium".to_string(),
        },
        JavaMirror {
            url: "https://mirrors.ha.edu.cn/Adoptium".to_string(),
            kind: "adoptium".to_string(),
        },
        JavaMirror {
            url: "https://mirror.nyist.edu.cn/Adoptium".to_string(),
            kind: "adoptium".to_string(),
        },
    ]
}

/// 启动器私有目录（旧位置，保留以兼容已装好的运行时）。
pub fn managed_root() -> Result<PathBuf, String> {
    Ok(data_root()?.join("java"))
}

/// 固定安装根：`<.minecraft>/runtime`（PCL 等启动器能扫到）。
pub fn install_root() -> Result<PathBuf, String> {
    Ok(crate::minecraft_manager::get_minecraft_dir()?.join("runtime"))
}

fn install_target(major: u32, kind: &str) -> Result<PathBuf, String> {
    Ok(install_root()?.join(format!("{major}-{kind}")))
}

fn emit(app: &AppHandle, phase: &str, done: u64, total: u64, label: &str) {
    let _ = app.emit(
        "java-install-progress",
        serde_json::json!({ "phase": phase, "done": done, "total": total, "label": label }),
    );
}

fn os_key() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    }
}

fn arch_key() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x64"
    }
}

/// 现有安装里是否有满足该大版本的 Java（含托管目录）。
pub fn best_for_major(major: u32) -> Option<PathBuf> {
    java_manager::scan_java_installations()
        .into_iter()
        .find(|rt| java_manager::version_major(&rt.version) == Some(major))
        .map(|rt| PathBuf::from(rt.path))
}

/// Adoptium v3 API（`{base}/v3/assets/latest/...`）：取安装包直链（Windows 为 zip）。
async fn resolve_api(base: &str, major: u32, prefer_jdk: bool) -> Option<String> {
    let kind = if prefer_jdk { "jdk" } else { "jre" };
    let base = base.trim().trim_end_matches('/');
    let base = if base.is_empty() { OFFICIAL_API_BASE } else { base };
    let url = format!(
        "{base}/v3/assets/latest/{major}/hotspot?os={}&architecture={}&image_type={kind}",
        os_key(),
        arch_key()
    );
    let resp = http_client()
        .get(&url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let json: Value = resp.json().await.ok()?;
    json.get(0)?
        .get("binary")?
        .get("package")?
        .get("link")?
        .as_str()
        .map(|s| s.to_string())
}

/// Adoptium 目录镜像（如 TUNA 的 `/Adoptium/`）：
/// 抓 `{root}/{major}/{kind}/{arch}/{os}/` 的目录列表，取其中最新版本的 zip。
async fn resolve_adoptium_dir(root: &str, major: u32, prefer_jdk: bool) -> Option<String> {
    let kind = if prefer_jdk { "jdk" } else { "jre" };
    let root = root.trim().trim_end_matches('/');
    if root.is_empty() {
        return None;
    }
    let url = format!("{root}/{major}/{kind}/{}/{}/", arch_key(), os_key());
    let resp = http_client()
        .get(&url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let html = resp.text().await.ok()?;
    // 文件名形如 OpenJDK21U-jre_x64_windows_hotspot_21.0.5_11.zip
    let pattern = format!(
        r#"href="([^"]*OpenJDK{major}U-{kind}_{}_{}_hotspot_[^"]*\.zip)""#,
        arch_key(),
        os_key()
    );
    let re = regex::Regex::new(&pattern).ok()?;
    let mut best: Option<(Vec<u64>, String)> = None;
    for cap in re.captures_iter(&html) {
        let file = cap.get(1)?.as_str().to_string();
        // 用文件名里的数字段做排序键，取最新
        let key: Vec<u64> = file
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<u64>().ok())
            .collect();
        let better = match &best {
            None => true,
            Some((best_key, _)) => key > *best_key,
        };
        if better {
            best = Some((key, file));
        }
    }
    let (_, file) = best?;
    if file.starts_with("http") {
        Some(file)
    } else {
        Some(format!("{url}{file}"))
    }
}

/// 设置页「Java 镜像」列表的默认项（与 Download/Update 源同款结构）。
pub fn default_java_mirrors() -> Vec<Value> {
    vec![
        serde_json::json!({"id":"official","name":"Adoptium","url":OFFICIAL_API_BASE,"kind":"api"}),
        serde_json::json!({"id":"adoptium-packages","name":"Adoptium packages","url":"https://packages.adoptium.net","kind":"adoptium"}),
        serde_json::json!({"id":"cqu","name":"CQU","url":"https://mirrors.cqu.edu.cn/adoptium","kind":"adoptium"}),
        serde_json::json!({"id":"hernet","name":"HERNET","url":"https://mirrors.ha.edu.cn/Adoptium","kind":"adoptium"}),
        serde_json::json!({"id":"nju","name":"NJU","url":"https://mirror.nju.edu.cn/adoptium","kind":"adoptium"}),
        serde_json::json!({"id":"nyist","name":"NYIST","url":"https://mirror.nyist.edu.cn/Adoptium","kind":"adoptium"}),
        serde_json::json!({"id":"tuna","name":"TUNA","url":"https://mirrors.tuna.tsinghua.edu.cn/Adoptium","kind":"adoptium"}),
        serde_json::json!({"id":"USTC","name":"USTC","url":"https://mirrors.ustc.edu.cn/adoptium","kind":"adoptium"}),
    ]
}

#[tauri::command]
pub fn default_java_mirrors_cmd() -> Vec<Value> {
    default_java_mirrors()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct JavaMirrorProbe {
    pub url: String,
    /// TTFB（毫秒）；失败为 None
    pub latency_ms: Option<u64>,
    /// 该镜像是否提供 Adoptium/Temurin 条目
    pub adoptium: bool,
    pub error: Option<String>,
}

/// 按类型探测单个镜像：返回 (可用?, 说明)。
async fn probe_one(url: &str, kind: &str) -> Result<bool, String> {
    match kind {
        "api" => {
            let base = url.trim().trim_end_matches('/');
            let base = if base.is_empty() { OFFICIAL_API_BASE } else { base };
            let probe_url = format!(
                "{base}/v3/assets/latest/21/hotspot?os={}&architecture={}&image_type=jre",
                os_key(),
                arch_key()
            );
            let resp = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                http_client().get(&probe_url).send(),
            )
            .await
            .map_err(|_| "timeout".to_string())?
            .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("HTTP {}", resp.status().as_u16()));
            }
            Ok(true)
        }
        "adoptium" => {
            // 目录镜像：探一下 21/jre/{arch}/{os}/ 里是否有 zip
            resolve_adoptium_dir(url, 21, false)
                .await
                .map(|_| true)
                .ok_or_else(|| "no Windows zip listed".to_string())
        }
        // 未知类型：不再支持 isoinfo
        _ => Err("unsupported mirror type".to_string()),
    }
}

/// 并发测速所有 Java 镜像（列表 UI 用，全部返回，不做竞速）。
#[tauri::command]
pub async fn probe_java_mirrors(mirrors: Vec<JavaMirror>) -> Vec<JavaMirrorProbe> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<JavaMirrorProbe>(mirrors.len().max(1));
    for mirror in mirrors {
        let tx = tx.clone();
        let url = mirror.url.clone();
        let kind = mirror.kind.clone();
        tauri::async_runtime::spawn(async move {
            let started = std::time::Instant::now();
            let res = probe_one(&url, &kind).await;
            let latency = started.elapsed().as_millis() as u64;
            let probe = match res {
                Ok(usable) => JavaMirrorProbe {
                    url,
                    latency_ms: Some(latency),
                    adoptium: usable,
                    error: None,
                },
                Err(e) => JavaMirrorProbe {
                    url,
                    latency_ms: None,
                    adoptium: false,
                    error: Some(e),
                },
            };
            let _ = tx.send(probe).await;
        });
    }
    drop(tx);
    let mut out = Vec::new();
    while let Some(p) = rx.recv().await {
        out.push(p);
    }
    out.sort_by_key(|p| (p.latency_ms.is_none(), p.latency_ms.unwrap_or(u64::MAX)));
    out
}

/// 解析下载地址：并发竞速官方 API + 所有镜像，谁先成功解析用谁
/// （等价于「选延迟最小且可用的源」）。
async fn resolve_download(major: u32, cfg: &JavaInstallConfig) -> Result<(String, String), String> {
    let mut entries: Vec<(String, String, String)> = vec![(
        OFFICIAL_API_BASE.to_string(),
        "Adoptium API".to_string(),
        "api".to_string(),
    )];
    for mirror in mirrors(cfg) {
        entries.push((mirror.url.clone(), mirror.url, mirror.kind));
    }
    let prefer_jdk = cfg.prefer_jdk;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(String, Option<String>)>(entries.len());
    for (url, label, kind) in entries {
        let tx = tx.clone();
        tauri::async_runtime::spawn(async move {
            let attempt = async {
                match kind.as_str() {
                    "api" => resolve_api(&url, major, prefer_jdk).await,
                    // 默认按 adoptium 目录处理（isoinfo 已不再支持）
                    _ => resolve_adoptium_dir(&url, major, prefer_jdk).await,
                }
            };
            // 单个源最多等 20 秒，避免一个卡住的源拖慢整体
            let found = tokio::time::timeout(std::time::Duration::from_secs(20), attempt)
                .await
                .ok()
                .flatten();
            let _ = tx.send((label, found)).await;
        });
    }
    drop(tx);
    while let Some((label, found)) = rx.recv().await {
        if let Some(url) = found {
            return Ok((url, label));
        }
    }
    Err(format!("No download URL for Java {major} (official and all mirrors failed)"))
}

/// 下载 + 解压 + 拍平，返回 java 可执行文件路径。
async fn download_and_extract(
    app: &AppHandle,
    major: u32,
    prefer_jdk: bool,
    url: &str,
) -> Result<PathBuf, String> {
    #[cfg(not(windows))]
    {
        let _ = (app, major, prefer_jdk, url);
        return Err("Auto-install of Java is currently implemented for Windows only".to_string());
    }
    #[cfg(windows)]
    {
        let kind = if prefer_jdk { "jdk" } else { "jre" };
        let root = install_target(major, kind)?;
        if let Some(exe) = find_exe(&root) {
            return Ok(exe);
        }
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("Failed to create {}: {e}", root.display()))?;

        let zip_path = root.join("download.zip");
        emit(app, "download", 0, 0, "Downloading Java…");
        let mut resp = http_client()
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Java download failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Java download failed: {e}"))?;
        let total = resp.content_length().unwrap_or(0);
        let mut file = std::fs::File::create(&zip_path)
            .map_err(|e| format!("Failed to create {}: {e}", zip_path.display()))?;
        {
            use std::io::Write;
            let mut done: u64 = 0;
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| format!("Java download interrupted: {e}"))?
            {
                file.write_all(&chunk)
                    .map_err(|e| format!("Write failed: {e}"))?;
                done += chunk.len() as u64;
                let pct = if total > 0 { done * 100 / total } else { 0 };
                emit(app, "download", done, total, &format!("Downloading Java {pct}%"));
            }
        }
        drop(file);

        emit(app, "extract", 0, 0, "Extracting Java…");
        extract_zip(&zip_path, &root)?;
        let _ = std::fs::remove_file(&zip_path);
        flatten_single_dir(&root)?;
        find_exe(&root)
            .ok_or_else(|| "Downloaded Java archive has no bin/java.exe".to_string())
    }
}

/// 解压 zip 到 dir（保留内部目录结构）。
fn extract_zip(zip_path: &Path, dir: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| format!("Failed to open {}: {e}", zip_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read the Java zip: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read a zip entry: {e}"))?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dir.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)
                .map_err(|e| format!("Failed to create {}: {e}", out.display()))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
        }
        let mut f = std::fs::File::create(&out)
            .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;
        std::io::copy(&mut entry, &mut f)
            .map_err(|e| format!("Failed to extract {}: {e}", out.display()))?;
    }
    Ok(())
}

/// Adoptium 的 zip 通常多一层 `jdk-21.0.x+y/`：把它拍平，让布局变成
/// `<root>/bin/java.exe`，这样 java_manager 的「根 + 一层子目录」扫描能直接命中。
fn flatten_single_dir(root: &Path) -> Result<(), String> {
    if root.join("bin").is_dir() {
        return Ok(());
    }
    let dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| format!("Failed to read {}: {e}", root.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    if dirs.len() != 1 {
        return Ok(());
    }
    let inner = &dirs[0];
    let children: Vec<PathBuf> = std::fs::read_dir(inner)
        .map_err(|e| format!("Failed to read {}: {e}", inner.display()))?
        .flatten()
        .map(|e| e.path())
        .collect();
    for child in children {
        let Some(name) = child.file_name() else { continue };
        let dest = root.join(name);
        if dest.exists() {
            continue;
        }
        std::fs::rename(&child, &dest)
            .map_err(|e| format!("Failed to move {}: {e}", child.display()))?;
    }
    let _ = std::fs::remove_dir(inner);
    Ok(())
}

/// 在目录里找 bin/java.exe（也兼容 macOS 的 Contents/Home 布局）。
fn find_exe(root: &Path) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "java.exe" } else { "java" };
    let direct = root.join("bin").join(exe_name);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let exe = dir.join("bin").join(exe_name);
        if exe.is_file() {
            return Some(exe);
        }
        let mac = dir
            .join("Contents")
            .join("Home")
            .join("bin")
            .join(exe_name);
        if mac.is_file() {
            return Some(mac);
        }
    }
    None
}

/// 确保某大版本的 Java 可用（不满足则自动下载安装）。
pub async fn ensure_for_major(app: &AppHandle, major: u32) -> Result<PathBuf, String> {
    if let Some(exe) = best_for_major(major) {
        return Ok(exe);
    }
    let cfg = config();
    let (url, from) = resolve_download(major, &cfg).await?;
    emit(app, "resolve", 0, 0, &format!("Java {major} · {from}"));
    let exe = download_and_extract(app, major, cfg.prefer_jdk, &url).await?;
    emit(app, "done", 1, 1, &format!("Java {major} installed"));
    Ok(exe)
}

/// 设置页「安装 Java」：给定大版本，缺则装，返回 java 路径。
#[tauri::command]
pub async fn install_java_runtime(app: AppHandle, major: u32) -> Result<String, String> {
    let exe = ensure_for_major(&app, major).await?;
    Ok(exe.to_string_lossy().into_owned())
}

/// 列出本地版本各自的 Java 要求与是否已满足（设置页「一键安装」用）。
/// 现有安装里是否已有该大版本的 Java（一次性扫描，供批量判断用）。
fn installed_majors() -> Vec<u32> {
    java_manager::scan_java_installations()
        .iter()
        .filter_map(|rt| java_manager::version_major(&rt.version))
        .collect()
}

#[tauri::command]
pub fn list_java_requirements() -> Result<Vec<Value>, String> {
    let root = crate::minecraft_manager::get_minecraft_dir()?;
    let versions_dir = root.join("versions");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&versions_dir) else {
        return Ok(out);
    };
    // 只扫一次已安装的 Java（扫描本身会起 java --version）
    let majors = installed_majors();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let json_path = entry.path().join(format!("{name}.json"));
        let Ok(text) = std::fs::read_to_string(&json_path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(major) = json["javaVersion"]["majorVersion"].as_u64() else {
            continue;
        };
        let major = major as u32;
        out.push(serde_json::json!({
            "version": name,
            "major": major,
            "satisfied": majors.contains(&major),
        }));
    }
    out.sort_by_key(|v| v["major"].as_u64().unwrap_or(0));
    Ok(out)
}
