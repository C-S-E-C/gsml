//! download_source —— 下载源（镜像）列表
//!
//! 源列表本身由前端维护（存 svar，可增删改），每次刷新时交给
//! `probe_download_sources`：
//!   1. 并发测速（读 TTFB），若设置为「游戏运行时不测速」且游戏在跑则直接跳过；
//!   2. 结果按延迟从低到高、**流式**尝试「解码」版本清单（拉 manifest + 解析）；
//!   3. 某个源解码成功 → 立刻把它设为活动源并返回；
//!      此时还没回来的 ping 直接丢弃（接收端已关闭，任务自行结束）。
//!
//! 活动源的 manifest URL / 域名重写给 minecraft_manager 的下载流程复用，
//! 因此安装时的版本 JSON、客户端、依赖库、资源文件也会走当前最快的镜像。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::game_launcher;
use crate::minecraft_manager::http_client;

const PING_TIMEOUT: Duration = Duration::from_secs(4);
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const OFFICIAL_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// 一个下载源。`rewrites` 把官方域名映射到镜像域名，清单里已经是镜像地址
/// 的源可以留空。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSource {
    pub id: String,
    pub name: String,
    /// 版本清单 JSON 的完整 URL
    pub manifest_url: String,
    /// 测速探针 URL（留空则用 manifest_url）
    #[serde(default)]
    pub ping_url: String,
    #[serde(default)]
    pub rewrites: Vec<(String, String)>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl DownloadSource {
    fn probe_url(&self) -> &str {
        if self.ping_url.trim().is_empty() {
            &self.manifest_url
        } else {
            &self.ping_url
        }
    }

    fn rewrite(&self, url: &str) -> String {
        for (from, to) in &self.rewrites {
            if let Some(rest) = url.strip_prefix(from.as_str()) {
                return format!("{to}{rest}");
            }
        }
        url.to_string()
    }
}

/// 内置的默认源（前端首次使用时会拿这份做初始化）。
pub fn default_sources() -> Vec<DownloadSource> {
    vec![
        DownloadSource {
            id: "official".to_string(),
            name: "Mojang Official".to_string(),
            manifest_url: OFFICIAL_MANIFEST.to_string(),
            ping_url: String::new(),
            rewrites: Vec::new(),
            enabled: true,
        },
        DownloadSource {
            id: "bmclapi".to_string(),
            name: "BMCLAPI Mirror".to_string(),
            manifest_url: "https://bmclapi2.bangbang93.com/mc/game/version_manifest_v2.json"
                .to_string(),
            ping_url: String::new(),
            rewrites: vec![
                (
                    "https://libraries.minecraft.net/".to_string(),
                    "https://bmclapi2.bangbang93.com/maven/".to_string(),
                ),
                (
                    "https://resources.download.minecraft.net/".to_string(),
                    "https://bmclapi2.bangbang93.com/assets/".to_string(),
                ),
            ],
            enabled: true,
        },
        DownloadSource {
            id: "minguncle".to_string(),
            name: "Minguncle MCMirror".to_string(),
            manifest_url: "https://mirror.minguncle.cn/mc/game/version_manifest_v2.json"
                .to_string(),
            ping_url: String::new(),
            rewrites: vec![
                (
                    "https://libraries.minecraft.net/".to_string(),
                    "https://mirror.minguncle.cn/maven/".to_string(),
                ),
                (
                    "https://resources.download.minecraft.net/".to_string(),
                    "https://mirror.minguncle.cn/assets/".to_string(),
                ),
            ],
            enabled: true,
        },
    ]
}

// ── 活动源（进程内缓存；刷新时由 probe 写入）─────────────────────────────

static ACTIVE: OnceLock<Mutex<Option<DownloadSource>>> = OnceLock::new();

fn active_slot() -> &'static Mutex<Option<DownloadSource>> {
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// 设为当前活动源（None 表示回落到官方）。
pub fn set_active(source: Option<DownloadSource>) {
    if let Ok(mut guard) = active_slot().lock() {
        *guard = source;
    }
}

pub fn active() -> Option<DownloadSource> {
    active_slot().lock().ok().and_then(|guard| guard.clone())
}

/// 当前活动源的版本清单 URL（无活动源时用官方）。
pub fn manifest_url() -> String {
    active()
        .map(|s| s.manifest_url)
        .unwrap_or_else(|| OFFICIAL_MANIFEST.to_string())
}

/// 按活动源的 rewrites 重写下载地址；无活动源或无匹配前缀时原样返回。
pub fn rewrite(url: &str) -> String {
    active()
        .map(|s| s.rewrite(url))
        .unwrap_or_else(|| url.to_string())
}

// ── 测速 / 解码 ────────────────────────────────────────────────────────

/// 测一次延迟：取响应头到达的时间（TTFB），body 不读即丢弃。
async fn ping(client: &reqwest::Client, url: &str) -> Result<u64, String> {
    let start = Instant::now();
    let resp = tokio::time::timeout(PING_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| "timeout".to_string())?
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    Ok(start.elapsed().as_millis() as u64)
}

/// 「解码」= 拉取版本清单并解析出 versions 数组。
async fn decode(client: &reqwest::Client, url: &str) -> Result<Vec<Value>, String> {
    let resp = tokio::time::timeout(FETCH_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| "timeout".to_string())?
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let body = tokio::time::timeout(FETCH_TIMEOUT, resp.text())
        .await
        .map_err(|_| "read timeout".to_string())?
        .map_err(|e| e.to_string())?;
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("manifest parse failed: {e}"))?;
    json["versions"]
        .as_array()
        .cloned()
        .ok_or_else(|| "unexpected manifest format (no versions)".to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeEntry {
    pub id: String,
    pub name: String,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    pub chosen: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    /// 游戏在跑且启用了「运行时不测速」→ 本轮跳过
    pub skipped: bool,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub latency_ms: Option<u64>,
    pub entries: Vec<ProbeEntry>,
    /// 清单里的 versions 原始数组（前端只取 id / type）
    pub versions: Vec<Value>,
    /// 最后一次整体失败的原因（供前端提示并回落到普通加载）
    pub error: Option<String>,
}

impl ProbeResult {
    fn empty(skipped: bool) -> Self {
        Self {
            skipped,
            source_id: None,
            source_name: None,
            latency_ms: None,
            entries: Vec::new(),
            versions: Vec::new(),
            error: None,
        }
    }
}

struct Outcome {
    src: DownloadSource,
    latency: Option<u64>,
    error: Option<String>,
    /// 解析成功时的版本清单（竞速的胜者）
    versions: Option<Vec<Value>>,
}

fn to_entries(outcomes: &[Outcome], chosen: Option<&str>) -> Vec<ProbeEntry> {
    let mut entries: Vec<ProbeEntry> = outcomes
        .iter()
        .map(|o| ProbeEntry {
            id: o.src.id.clone(),
            name: o.src.name.clone(),
            latency_ms: o.latency,
            error: o.error.clone(),
            chosen: chosen.is_some_and(|c| c == o.src.id),
        })
        .collect();
    // 有延迟的按升序在前，失败/未测的在后
    entries.sort_by_key(|e| (e.latency_ms.is_none(), e.latency_ms.unwrap_or(u64::MAX)));
    entries
}

/// 竞速：所有启用的源**同时**开始「测速 → 解码」，谁先返回且解析成功就用谁，
/// 其余任务在接收端关闭后自动作废（即「抛弃还没完成的」）。
#[tauri::command]
pub async fn probe_download_sources(
    sources: Vec<DownloadSource>,
    pause_while_running: bool,
) -> Result<ProbeResult, String> {
    if pause_while_running && game_launcher::is_running() {
        let mut res = ProbeResult::empty(true);
        let act = active();
        res.source_id = act.as_ref().map(|s| s.id.clone());
        res.source_name = act.map(|s| s.name);
        return Ok(res);
    }

    let enabled: Vec<DownloadSource> = sources.into_iter().filter(|s| s.enabled).collect();
    if enabled.is_empty() {
        return Err("No download source is enabled".to_string());
    }
    let total = enabled.len();
    let client = http_client();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Outcome>(total.max(1));

    for src in enabled {
        let tx = tx.clone();
        let client = client.clone();
        let url = src.probe_url().to_string();
        let manifest_url = src.manifest_url.clone();
        tauri::async_runtime::spawn(async move {
            // 单个源的流程：先测速，再解析清单；任一环节失败都带上原因回报。
            let latency = ping(&client, &url).await;
            let mut outcome = Outcome {
                src,
                latency: latency.as_ref().ok().copied(),
                error: latency.as_ref().err().cloned(),
                versions: None,
            };
            if latency.is_ok() {
                match decode(&client, &manifest_url).await {
                    Ok(versions) => outcome.versions = Some(versions),
                    Err(e) => outcome.error = Some(format!("decode failed: {e}")),
                }
            }
            // 胜出后接收端关闭：这里的 send 失败即「被抛弃」
            let _ = tx.send(outcome).await;
        });
    }
    drop(tx);

    let mut outcomes: Vec<Outcome> = Vec::new();
    while let Some(outcome) = rx.recv().await {
        if let Some(versions) = outcome.versions.clone() {
            // 第一个解析成功的源胜出；此时其它源的 ping/解码任务被丢弃
            set_active(Some(outcome.src.clone()));
            let mut res = ProbeResult::empty(false);
            res.source_id = Some(outcome.src.id.clone());
            res.source_name = Some(outcome.src.name.clone());
            res.latency_ms = outcome.latency;
            outcomes.push(Outcome { versions: None, ..outcome });
            res.entries = to_entries(&outcomes, res.source_id.as_deref());
            res.versions = versions;
            return Ok(res);
        }
        outcomes.push(outcome);
    }

    // 全部失败：不报 Err，把条目和原因交给前端提示（前端会回落到普通加载）
    let entries = to_entries(&outcomes, None);
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
    let mut res = ProbeResult::empty(false);
    res.entries = entries;
    res.error = Some(format!("All download sources are unavailable: {detail}"));
    Ok(res)
}

/// 前端首屏读取内置默认源（用户没有自定义过时展示它们）。
#[tauri::command]
pub fn default_download_sources() -> Vec<DownloadSource> {
    default_sources()
}
