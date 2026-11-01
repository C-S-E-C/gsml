//! Deep-link 分发器，支持注册 / 注销。
//!
//! - `register(...)` 返回 `Subscription`，调用 `.unregister()` 或 drop 时注销。
//! - 其他模块各自注册自己的 handler；lib.rs 只调 `init`。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri_plugin_deep_link::DeepLinkExt;

pub type Handler = Box<dyn Fn(&url::Url) + Send + Sync>;

/// 注册时返回的句柄。drop 自动注销。
pub struct Subscription {
    id: u64,
}

impl Subscription {
    /// 显式注销。可重复调用。
    #[allow(dead_code)]
    pub fn unregister(&self) {
        remove(self.id);
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        remove(self.id);
    }
}

struct Entry {
    id: u64,
    scheme: String,
    host: Option<String>,
    handler: Handler,
}

struct Registry {
    next_id: AtomicU64,
    entries: Mutex<Vec<Entry>>,
}

fn registry() -> &'static Registry {
    static REG: OnceLock<Registry> = OnceLock::new();
    REG.get_or_init(|| Registry {
        next_id: AtomicU64::new(1),
        entries: Mutex::new(Vec::new()),
    })
}

/// 注册处理函数，返回可注销的句柄。
pub fn register<F>(scheme: &str, host: Option<&str>, handler: F) -> Subscription
where
    F: Fn(&url::Url) + Send + Sync + 'static,
{
    let reg = registry();
    let id = reg.next_id.fetch_add(1, Ordering::Relaxed);
    reg.entries.lock().expect("registry poisoned").push(Entry {
        id,
        scheme: scheme.to_ascii_lowercase(),
        host: host.map(|h| h.to_ascii_lowercase()),
        handler: Box::new(handler),
    });
    Subscription { id }
}

fn remove(id: u64) {
    registry()
        .entries
        .lock()
        .expect("registry poisoned")
        .retain(|e| e.id != id);
}

/// 安装插件 + 监听。lib.rs 只需调用这一行。
pub fn init<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            dispatch(url.as_str());
        }
    }

    app.deep_link().on_open_url(|event| {
        for url in event.urls() {
            dispatch(url.as_str());
        }
    });
}

fn dispatch(raw: &str) {
    let Ok(url) = url::Url::parse(raw) else {
        eprintln!("[deeplink] invalid url: {raw}");
        return;
    };

    let scheme = url.scheme().to_ascii_lowercase();
    let host = url.host_str().map(|h| h.to_ascii_lowercase());

    let reg = registry();
    let entries = reg.entries.lock().expect("registry poisoned");

    // 先精确 (scheme, host)，再回退 (scheme, None)
    let mut matched = false;
    for e in entries
        .iter()
        .filter(|e| e.scheme == scheme && e.host == host)
    {
        (e.handler)(&url);
        matched = true;
    }
    if !matched {
        for e in entries
            .iter()
            .filter(|e| e.scheme == scheme && e.host.is_none())
        {
            (e.handler)(&url);
            matched = true;
        }
    }
    if !matched {
        eprintln!("[deeplink] no handler for {raw}");
    }
}
