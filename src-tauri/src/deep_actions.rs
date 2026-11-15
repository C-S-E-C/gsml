//! 自定义 URL scheme 动作：装版本 / 启动游戏并自动进服。
//!
//!   glow-squid://install-version?version=1.20.1&loader=forge&loaderVersion=47.4.0&name=MyInstance
//!   glow-squid://start-game?version=1.20.1&server=play.example.com:25565
//!
//! 解析后通过 `deeplink-action` 事件发给前端（顶层 shell 弹确认框）：
//!   · install-version → 切到 Download 页并**预填**版本/loader，由用户确认后
//!     自己点 Install（不自动安装）；
//!   · start-game      → 选中实例后启动（可选自动进服）。
//!
//! 参数：
//!   install-version: version=必需；loader=vanilla|fabric|forge|neoforge（缺省 vanilla）；
//!                    loaderVersion=非 vanilla 时必需；name=可选实例名
//!   start-game:      version=可选（省略=列出所有本地实例）；
//!                    server=可选 host[:port]（缺省端口 25565）

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
use url::Url;

use crate::deeplink_handler;

/// 前端是否已经装好 `deeplink-action` 监听。
/// 浏览器唤起启动器（冷启动）时，Rust 分发 URL 的时刻页面还没加载完，
/// 直接 emit 会丢事件 —— 所以未就绪时先排队，前端就绪后自己来取。
static FRONTEND_READY: AtomicBool = AtomicBool::new(false);
static PENDING: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();

fn pending() -> &'static Mutex<Vec<Value>> {
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

fn query(url: &Url) -> HashMap<String, String> {
    url.query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn loader_code(name: &str) -> Option<u8> {
    match name.trim().to_ascii_lowercase().as_str() {
        "" | "vanilla" => Some(0),
        "fabric" => Some(1),
        "forge" => Some(2),
        "neoforge" => Some(3),
        _ => None,
    }
}

fn parse_install(url: &Url) -> Option<Value> {
    let q = query(url);
    let version = q.get("version")?.trim().to_string();
    if version.is_empty() {
        return None;
    }
    let loader = loader_code(q.get("loader").map(String::as_str).unwrap_or(""))?;
    let loader_version = q
        .get("loaderVersion")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if loader != 0 && loader_version.is_empty() {
        return None;
    }
    Some(json!({
        "kind": "install",
        "version": version,
        "loader": loader,
        "loaderVersion": loader_version,
        "name": q.get("name").cloned().unwrap_or_default().trim().to_string(),
    }))
}

fn parse_launch(url: &Url) -> Option<Value> {
    let q = query(url);
    // version 可选：省略时前端会列出所有本地实例让用户挑
    let version = q
        .get("version")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    Some(json!({
        "kind": "launch",
        "version": version,
        "server": q.get("server").cloned().unwrap_or_default().trim().to_string(),
    }))
}

fn emit(app: &AppHandle, payload: Value) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_focus();
    }
    if FRONTEND_READY.load(Ordering::SeqCst) {
        let _ = app.emit("deeplink-action", payload);
    } else if let Ok(mut queue) = pending().lock() {
        queue.push(payload);
    }
}

/// 前端监听装好后调用：返回「前端就绪前」攒下的动作（取走即清空）。
#[tauri::command]
pub fn deeplink_ready() -> Vec<Value> {
    FRONTEND_READY.store(true, Ordering::SeqCst);
    pending()
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

/// 注册两个 host 的 handler。必须在 `deeplink_handler::init` **之前**调用，
/// 否则启动参数里带来的 URL 会被提前分发、没人接。
pub fn init(app: &AppHandle) {
    let app_install = app.clone();
    let install = deeplink_handler::register("glow-squid", Some("install-version"), move |url| {
        match parse_install(url) {
            Some(payload) => emit(&app_install, payload),
            None => eprintln!(
                "[deeplink] install-version 参数不完整（需要 version；非 vanilla 还需 loaderVersion）: {url}"
            ),
        }
    });

    let app_launch = app.clone();
    let launch = deeplink_handler::register("glow-squid", Some("start-game"), move |url| {
        match parse_launch(url) {
            Some(payload) => emit(&app_launch, payload),
            None => eprintln!("[deeplink] start-game 链接无法解析: {url}"),
        }
    });

    // Subscription 一旦 drop 就会注销 handler：这两个要活到进程结束
    std::mem::forget((install, launch));
}
