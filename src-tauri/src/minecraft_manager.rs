//! minecraft_manager —— 本地版本扫描、远程版本列表、版本安装（vanilla / fabric / forge / neoforge）
//!
//! 进度事件 install-progress（前端 listen）：
//!   { name, stage, done, total, bytesDone, bytesTotal, speed, label }
//!   - name: 正在安装的版本 id（forge/neoforge 以安装器实际创建的目录名为准）
//!   - stage: json / client / libraries / natives / assets / installer / done
//!     client 阶段 done/total 为字节数；libraries/natives/assets 为文件数；其余 0..1
//!   - bytesDone/bytesTotal: 已下载/总字节数（0=未知），批量下载阶段提供平滑进度条
//!   - speed: 估算速度（字节/秒）；批量下载 16 路并发，120ms 节流广播
//! 取消：cancel_install() 置位，安装流程在每个阶段/分片检查；同时间只允许一个安装任务。

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use zip::ZipArchive;

#[derive(Serialize, Clone)]
pub struct MinecraftVersion {
    pub name: String,
    /// 官方名称：fabric/forge/neoforge = 所基于的原版 MC 版本（inheritsFrom），原版 = 自身 id
    pub official_name: String,
    pub loader: u8, // 0->vanilla, 1->fabric, 2->forge, 3->neoforge
    /// 版本类型（Mojang type 字段）：0->release, 1->snapshot, 2->old_alpha, 3->old_beta, 4->其他/未知
    #[serde(rename = "type")]
    pub version_type: u8,
}

/// Mojang type 字符串 -> u8：0->release, 1->snapshot, 2->old_alpha, 3->old_beta, 4->其他/未知
fn mc_type_u8(t: Option<&str>) -> u8 {
    match t {
        Some("release") => 0,
        Some("snapshot") => 1,
        Some("old_alpha") => 2,
        Some("old_beta") => 3,
        _ => 4,
    }
}

/// 安装取消标志
static CANCEL: AtomicBool = AtomicBool::new(false);
/// 互斥：同一时间只允许一个安装任务
static INSTALLING: AtomicBool = AtomicBool::new(false);

fn check_cancel() -> Result<(), String> {
    if CANCEL.load(Ordering::Relaxed) {
        Err("已取消安装".into())
    } else {
        Ok(())
    }
}

fn io_err(e: std::io::Error) -> String {
    e.to_string()
}

fn default_mc_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    { dirs::data_dir().unwrap().join(".minecraft") }

    #[cfg(target_os = "macos")]
    { dirs::data_dir().unwrap().join("minecraft") }

    #[cfg(all(unix, not(target_os = "macos")))]
    { dirs::home_dir().unwrap().join(".minecraft") }
}

/// 游戏根目录（TODO: 之后可在设置里支持自定义，simpleValues 存路径）
fn get_minecraft_dir() -> Result<PathBuf, String> {
    Ok(default_mc_dir())
}

pub(crate) fn http_client() -> reqwest::Client {
    static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("build reqwest client")
    });
    CLIENT.clone()
}

/// 大文件（客户端 jar / 官方安装器）专用：无总超时，只有连接超时
static BIG_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .expect("build big-file reqwest client")
});

fn emit_install(app: &AppHandle, name: &str, stage: &str, done: u64, total: u64, label: &str) {
    emit_install_full(app, name, stage, done, total, 0, 0, 0, label);
}

/// 完整进度负载。done/total：文件数（client/installer 阶段为字节数）；
/// bytesDone/bytesTotal：字节级进度（0=未知）；speed：估算速度（字节/秒）。
/// 前端按需取用，缺失字段当 0 处理，向后兼容旧格式。
fn emit_install_full(
    app: &AppHandle,
    name: &str,
    stage: &str,
    done: u64,
    total: u64,
    bytes_done: u64,
    bytes_total: u64,
    speed: u64,
    label: &str,
) {
    let payload = serde_json::json!({
        "name": name, "stage": stage, "done": done, "total": total,
        "bytesDone": bytes_done, "bytesTotal": bytes_total, "speed": speed,
        "label": label,
    });
    if let Err(e) = app.emit("install-progress", payload) {
        eprintln!("[mc-install] 进度事件发送失败: {e}");
    }
}

/// 字节数人性化显示（进度 label 用）：0 B / 12.3 KB / 4.5 MB / 1.2 GB
fn fmt_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

// ==================== HTTP / 文件下载基础 ====================

async fn fetch_json(url: &str) -> Result<Value, String> {
    let response = http_client().get(url).send().await
        .map_err(|e| format!("请求失败 {url}: {e}"))?;
    let resp = response.error_for_status().map_err(|e| format!("{url}: {e}"))?;
    resp.json::<Value>().await
        .map_err(|e| format!("解析 JSON 失败 {url}: {e}"))
}

async fn fetch_text(url: &str) -> Result<String, String> {
    let response = http_client().get(url).send().await
        .map_err(|e| format!("请求失败 {url}: {e}"))?;
    response.error_for_status()
        .map_err(|e| format!("{url}: {e}"))?
        .text().await
        .map_err(|e| format!("读取响应失败 {url}: {e}"))
}

/// 下载到指定路径；dest 已存在且尺寸匹配（尺寸未知时存在即算）则跳过。
/// 传入 prog 时把每个写入的字节块累加进共享进度并节流广播。
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_size: Option<u64>,
    prog: Option<&PoolProgress>,
) -> Result<(), String> {
    if let Ok(meta) = fs::metadata(dest) {
        if meta.is_file() && expected_size.map_or(true, |s| meta.len() == s) {
            return Ok(());
        }
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    let response = client.get(url).send().await
        .map_err(|e| format!("下载失败 {url}: {e}"))?;
    let mut resp = response.error_for_status().map_err(|e| format!("{url}: {e}"))?;
    let mut file = fs::File::create(dest).map_err(io_err)?;
    let mut written: u64 = 0;
    while let Some(chunk) = resp.chunk().await
        .map_err(|e| format!("下载中断 {url}: {e}"))?
    {
        check_cancel()?;
        file.write_all(&chunk).map_err(io_err)?;
        written += chunk.len() as u64;
        if let Some(p) = prog {
            p.done_bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            p.emit(false, "下载中…");
        }
    }
    if let Some(size) = expected_size {
        if size > 0 && written != size {
            let _ = fs::remove_file(dest);
            return Err(format!("下载不完整 {url}（{written}/{size} 字节）"));
        }
    }
    Ok(())
}

/// 批量下载共享进度：文件数 + 字节数，120ms 节流广播 + 速度估算
struct PoolProgress {
    app: AppHandle,
    pname: String,
    stage: String,
    total_files: u64,
    total_bytes: u64,
    done_files: AtomicU64,
    done_bytes: AtomicU64,
    /// (上次广播时刻, 已写字节, 已完成文件) —— 估算速度用
    last: Mutex<(Instant, u64, u64)>,
}

impl PoolProgress {
    /// forced=true 立即广播（阶段开始/结束）；false 时 120ms 节流
    fn emit(&self, forced: bool, label: &str) {
        let mut last = self.last.lock().unwrap();
        let now = Instant::now();
        let dt = now.duration_since(last.0);
        if !forced && dt < Duration::from_millis(120) {
            return;
        }
        let files = self.done_files.load(Ordering::Relaxed);
        let bytes = self.done_bytes.load(Ordering::Relaxed);
        let speed = bytes.saturating_sub(last.1) as f64 / dt.as_secs_f64().max(0.001);
        *last = (now, bytes, files);
        drop(last);
        emit_install_full(
            &self.app, &self.pname, &self.stage,
            files, self.total_files, bytes, self.total_bytes, speed as u64, label,
        );
    }
}

/// 并行（16 路常驻 worker 池，任务由原子下标分发，无需等整批）下载一批文件；
/// 广播文件数 + 字节数 + 速度进度；任一失败汇总报错
async fn download_all(
    app: &AppHandle,
    items: Vec<(String, PathBuf, Option<u64>)>,
    pname: &str,
    stage: &str,
) -> Result<(), String> {
    let total = items.len() as u64;
    if total == 0 {
        return Ok(());
    }
    let bytes_total: u64 = items.iter().map(|(_, _, s)| s.unwrap_or(0)).sum();
    let progress = Arc::new(PoolProgress {
        app: app.clone(),
        pname: pname.to_string(),
        stage: stage.to_string(),
        total_files: total,
        total_bytes: bytes_total,
        done_files: AtomicU64::new(0),
        done_bytes: AtomicU64::new(0),
        last: Mutex::new((Instant::now(), 0, 0)),
    });
    let head = if bytes_total > 0 {
        format!("共 {total} 个文件 · 约 {}", fmt_bytes(bytes_total))
    } else {
        format!("共 {total} 个文件…")
    };
    progress.emit(true, &head);

    const WORKERS: usize = 16;
    let items = Arc::new(items);
    let next = Arc::new(AtomicUsize::new(0));
    let failures: Arc<Mutex<Vec<String>>> = Default::default();
    let mut handles = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let items = Arc::clone(&items);
        let next = Arc::clone(&next);
        let progress = Arc::clone(&progress);
        let failures = Arc::clone(&failures);
        let client = http_client();
        handles.push(tauri::async_runtime::spawn(async move {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                if check_cancel().is_err() {
                    break; // 已取消：worker 直接退出，外面统一报“已取消安装”
                }
                let (url, dest, size) = &items[i];
                if let Err(e) = download_file(&client, url, dest, *size, Some(&progress)).await {
                    failures.lock().unwrap().push(format!("{url}: {e}"));
                }
                progress.done_files.fetch_add(1, Ordering::Relaxed);
                progress.emit(false, "下载中…");
            }
        }));
    }
    for h in handles {
        h.await.map_err(|e| format!("下载任务异常退出: {e}"))?;
    }
    check_cancel()?;
    let fails = failures.lock().unwrap();
    if !fails.is_empty() {
        return Err(format!("{} 个文件下载失败，首个错误：{}", fails.len(), fails[0]));
    }
    progress.emit(true, "下载完成");
    Ok(())
}

/// 大文件流式下载（无总超时），广播字节级进度 + 速度
async fn download_with_progress(
    app: &AppHandle,
    pname: &str,
    stage: &str,
    url: &str,
    dest: &Path,
) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    let response = BIG_CLIENT.get(url).send().await
        .map_err(|e| format!("下载失败 {url}: {e}"))?;
    let mut resp = response.error_for_status().map_err(|e| format!("{url}: {e}"))?;
    let total = resp.content_length().unwrap_or(0);
    let mut file = fs::File::create(dest).map_err(io_err)?;
    let mut written: u64 = 0;
    let mut last_bytes: u64 = 0;
    let mut last_time = Instant::now();
    while let Some(chunk) = resp.chunk().await
        .map_err(|e| format!("下载中断 {url}: {e}"))?
    {
        check_cancel()?;
        file.write_all(&chunk).map_err(io_err)?;
        written += chunk.len() as u64;
        let now = Instant::now();
        if written - last_bytes >= (total / 20).max(512 * 1024)
            && now.duration_since(last_time) >= Duration::from_millis(80)
        {
            let speed = (written - last_bytes) as f64 / now.duration_since(last_time).as_secs_f64();
            last_bytes = written;
            last_time = now;
            emit_install_full(
                app, pname, stage, written, total.max(1),
                written, total, speed as u64, "下载中…",
            );
        }
    }
    emit_install_full(app, pname, stage, written, total.max(1), written, total, 0, "下载完成");
    Ok(())
}

// ==================== 原版基底 ====================

async fn find_vanilla_json_url(game_version: &str) -> Result<String, String> {
    let manifest = fetch_json("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json").await?;
    let arr = manifest["versions"].as_array().ok_or("官方清单格式异常")?;
    for v in arr {
        if v["id"].as_str() == Some(game_version) {
            return v["url"].as_str().map(|s| s.to_string())
                .ok_or_else(|| format!("版本 {game_version} 缺少下载地址"));
        }
    }
    Err(format!("官方清单中不存在版本 {game_version}"))
}

/// Mojang rules 过滤：os 缺省 = 全平台；allow/disallow 依次生效
fn lib_allowed(lib: &Value) -> bool {
    let Some(rules) = lib["rules"].as_array() else { return true };
    let mut allowed = false;
    for r in rules {
        let os_ok = r["os"]["name"].as_str().map_or(true, |n| n == "windows");
        if !os_ok {
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

/// 安装/补齐一个原版基底：版本 JSON、客户端 jar、依赖库、Windows natives、资源文件。
/// fabric / forge / neoforge 都复用它；全部幂等（已存在且尺寸匹配就跳过），返回版本 JSON。
async fn ensure_vanilla_base(app: &AppHandle, game_version: &str) -> Result<Value, String> {
    let root = get_minecraft_dir()?;
    let vdir = root.join("versions").join(game_version);

    // 1) 版本 JSON
    let json_path = vdir.join(format!("{game_version}.json"));
    let json: Value = if json_path.is_file() {
        let text = fs::read_to_string(&json_path).map_err(io_err)?;
        serde_json::from_str(&text).map_err(|e| format!("版本 JSON 解析失败 {game_version}: {e}"))?
    } else {
        emit_install(app, game_version, "json", 0, 1, "下载版本 JSON…");
        let url = find_vanilla_json_url(game_version).await?;
        let json = fetch_json(&url).await?;
        fs::create_dir_all(&vdir).map_err(io_err)?;
        let pretty = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
        fs::write(&json_path, pretty).map_err(io_err)?;
        json
    };
    check_cancel()?;

    // 2) 客户端 jar（约 25MB，字节级进度）
    let jar_path = vdir.join(format!("{game_version}.jar"));
    let client_url = json["downloads"]["client"]["url"].as_str()
        .ok_or("版本 JSON 缺少 client 下载地址")?;
    let client_size = json["downloads"]["client"]["size"].as_u64().unwrap_or(0);
    let cur = fs::metadata(&jar_path).map(|m| m.len()).unwrap_or(0);
    if cur != client_size {
        download_with_progress(app, game_version, "client", client_url, &jar_path).await?;
    }
    check_cancel()?;

    // 3) 依赖库 + natives
    let libs = json["libraries"].as_array().ok_or("版本 JSON 缺少 libraries")?;
    install_libraries(app, game_version, libs).await?;
    extract_natives(app, game_version, &vdir, libs).await?;
    check_cancel()?;

    // 4) 资源文件（量最大，已存在的一律跳过）
    install_assets(app, game_version, &json).await?;

    Ok(json)
}

/// 下载版本 JSON 里所有 artifacts 依赖库（跳过已存在且尺寸匹配的）
async fn install_libraries(app: &AppHandle, pname: &str, libs: &[Value]) -> Result<(), String> {
    let root = get_minecraft_dir()?;
    let mut items: Vec<(String, PathBuf, Option<u64>)> = Vec::new();
    for lib in libs {
        if !lib_allowed(lib) {
            continue;
        }
        let artifact = &lib["downloads"]["artifact"];
        let (Some(url), Some(path)) = (artifact["url"].as_str(), artifact["path"].as_str()) else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        let dest = root.join("libraries").join(path);
        let size = artifact["size"].as_u64().unwrap_or(0);
        if size > 0 && fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == size {
            continue;
        }
        items.push((url.to_string(), dest, (size > 0).then_some(size)));
    }
    download_all(app, items, pname, "libraries").await
}

/// 下载 natives-windows jar 并把其中的 .dll 解压到 versions/<id>/natives（已解压过则跳过）
async fn extract_natives(app: &AppHandle, pname: &str, vdir: &Path, libs: &[Value]) -> Result<(), String> {
    let natives_dir = vdir.join("natives");
    if natives_dir.read_dir().map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Ok(());
    }
    let mut jars: Vec<(String, String)> = Vec::new();
    for lib in libs {
        if !lib_allowed(lib) {
            continue;
        }
        let Some(url) = lib["downloads"]["classifiers"]["natives-windows"]["url"].as_str() else {
            continue;
        };
        let name = url.rsplit('/').next().unwrap_or("natives.jar").to_string();
        jars.push((url.to_string(), name));
    }
    if jars.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(&natives_dir).map_err(io_err)?;
    let total = jars.len() as u64;
    emit_install(app, pname, "natives", 0, total, "下载并解压 natives…");
    let client = http_client();
    for (i, (url, name)) in jars.into_iter().enumerate() {
        check_cancel()?;
        let tmp = std::env::temp_dir().join(format!("gsml-{name}"));
        download_file(&client, &url, &tmp, None, None).await?;
        extract_dlls(&tmp, &natives_dir)?;
        let _ = fs::remove_file(&tmp);
        emit_install(app, pname, "natives", (i + 1) as u64, total, "下载并解压 natives…");
    }
    Ok(())
}

fn extract_dlls(jar: &Path, out_dir: &Path) -> Result<(), String> {
    let file = fs::File::open(jar).map_err(io_err)?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("读取 natives jar 失败: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name() else { continue };
        if rel.extension().and_then(|e| e.to_str()) != Some("dll") {
            continue;
        }
        let Some(basename) = rel.file_name() else { continue };
        let out = out_dir.join(basename);
        let mut w = fs::File::create(&out).map_err(io_err)?;
        std::io::copy(&mut entry, &mut w).map_err(io_err)?;
    }
    Ok(())
}

/// 下载 assetIndex 指向的全部资源文件（objects 按 hash 去重，跳过已存在且尺寸匹配的）
async fn install_assets(app: &AppHandle, game_version: &str, json: &Value) -> Result<(), String> {
    let Some(index) = json["assetIndex"].as_object() else {
        return Ok(()); // 个别老版本没有 assetIndex
    };
    let index_url = index["url"].as_str().ok_or("assetIndex 缺少 url")?;
    let index_id = index["id"].as_str().ok_or("assetIndex 缺少 id")?;
    let root = get_minecraft_dir()?;
    let idx_path = root.join("assets").join("indexes").join(format!("{index_id}.json"));
    let index_json: Value = if idx_path.is_file() {
        let text = fs::read_to_string(&idx_path).map_err(io_err)?;
        serde_json::from_str(&text).map_err(|e| format!("资源索引解析失败: {e}"))?
    } else {
        let data = fetch_json(index_url).await?;
        fs::create_dir_all(idx_path.parent().unwrap()).map_err(io_err)?;
        let pretty = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        fs::write(&idx_path, pretty).map_err(io_err)?;
        data
    };
    let objects = index_json["objects"].as_object().ok_or("资源索引缺少 objects")?;
    let mut seen: HashSet<&str> = HashSet::new();
    let mut items: Vec<(String, PathBuf, Option<u64>)> = Vec::new();
    for meta in objects.values() {
        let Some(hash) = meta["hash"].as_str() else { continue };
        if !seen.insert(hash) {
            continue; // 多个逻辑文件共用同一 hash
        }
        let size = meta["size"].as_u64().unwrap_or(0);
        let h2 = hash.get(0..2).ok_or("资源索引 hash 非法")?;
        let dest = root.join("assets").join("objects").join(h2).join(hash);
        if size > 0 && fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == size {
            continue;
        }
        let url = format!("https://resources.download.minecraft.net/{h2}/{hash}");
        items.push((url, dest, (size > 0).then_some(size)));
    }
    download_all(app, items, game_version, "assets").await
}

// ==================== 各加载器安装 ====================

async fn install_vanilla(app: &AppHandle, game_version: &str) -> Result<MinecraftVersion, String> {
    ensure_vanilla_base(app, game_version).await?;
    emit_install(app, game_version, "done", 1, 1, "安装完成");
    Ok(MinecraftVersion {
        name: game_version.to_string(),
        official_name: game_version.to_string(),
        loader: 0,
        version_type: 0,
    })
}

/// fabric：原版基底 + meta 的 profile JSON（inheritsFrom 指向原版，无需处理器）
async fn install_fabric(
    app: &AppHandle,
    game_version: &str,
    loader_version: &str,
) -> Result<MinecraftVersion, String> {
    let id = format!("fabric-loader-{loader_version}-{game_version}");
    // 1) 原版基底（资源文件、natives 都在其中，fabric 直接复用）
    ensure_vanilla_base(app, game_version).await?;
    check_cancel()?;
    // 2) fabric profile JSON
    let url = format!("https://meta.fabricmc.net/v2/versions/loader/{game_version}/{loader_version}/profile/json");
    let profile = fetch_json(&url).await
        .map_err(|e| format!("获取 Fabric profile 失败（版本组合可能不存在）: {e}"))?;
    let root = get_minecraft_dir()?;
    let vdir = root.join("versions").join(&id);
    fs::create_dir_all(&vdir).map_err(io_err)?;
    let json_path = vdir.join(format!("{id}.json"));
    let pretty = serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?;
    fs::write(&json_path, pretty).map_err(io_err)?;
    // 3) fabric 依赖库（maven.fabricmc.net）
    let libs = profile["libraries"].as_array().cloned().unwrap_or_default();
    install_maven_libraries(app, &id, &libs).await?;
    emit_install(app, &id, "done", 1, 1, "安装完成");
    Ok(MinecraftVersion {
        name: id,
        official_name: game_version.to_string(),
        loader: 1,
        version_type: 0,
    })
}

/// fabric profile 的 libraries：优先 downloads.artifact，否则 maven 坐标（name+url）推路径
async fn install_maven_libraries(app: &AppHandle, pname: &str, libs: &[Value]) -> Result<(), String> {
    let root = get_minecraft_dir()?;
    let mut items: Vec<(String, PathBuf, Option<u64>)> = Vec::new();
    for lib in libs {
        let artifact = &lib["downloads"]["artifact"];
        if let (Some(url), Some(path)) = (artifact["url"].as_str(), artifact["path"].as_str()) {
            if !url.is_empty() {
                let dest = root.join("libraries").join(path);
                let size = artifact["size"].as_u64().unwrap_or(0);
                if size > 0 && fs::metadata(&dest).map(|m| m.len()).unwrap_or(0) == size {
                    continue;
                }
                items.push((url.to_string(), dest, (size > 0).then_some(size)));
                continue;
            }
        }
        let Some(coord) = lib["name"].as_str() else { continue };
        let parts: Vec<&str> = coord.split(':').collect();
        if parts.len() != 3 {
            continue;
        }
        let rel = format!(
            "{}/{}/{}/{}-{}.jar",
            parts[0].replace('.', "/"), parts[1], parts[2], parts[1], parts[2]
        );
        let dest = root.join("libraries").join(&rel);
        if dest.is_file() {
            continue;
        }
        let base = lib["url"].as_str().unwrap_or("https://libraries.minecraft.net/");
        let url = if base.ends_with('/') { format!("{base}{rel}") } else { format!("{base}/{rel}") };
        items.push((url, dest, None));
    }
    download_all(app, items, pname, "libraries").await
}

/// forge / neoforge 共用：原版基底 + 官方安装器 jar（--installClient，需 Java）+ 目录差异探测安装结果
async fn install_installer_based(
    app: &AppHandle,
    game_version: &str,
    version_id: &str,
    installer_url: &str,
    java_path: Option<&str>,
    loader: u8,
) -> Result<MinecraftVersion, String> {
    let root = get_minecraft_dir()?;
    let versions_dir = root.join("versions");
    let vdir = versions_dir.join(version_id);

    // 1) 原版基底（inheritsFrom 依赖它；资源文件也在这里补齐）
    ensure_vanilla_base(app, game_version).await?;
    check_cancel()?;

    // 2) 已装过 → 直接返回（只补基底资源）
    if vdir.join(format!("{version_id}.json")).is_file() {
        emit_install(app, version_id, "done", 1, 1, "已安装，资源已就绪");
        return Ok(MinecraftVersion {
            name: version_id.to_string(),
            official_name: game_version.to_string(),
            loader,
            version_type: 0,
        });
    }

    // 3) Java（官方安装器必须）
    let java = find_java(java_path)?;

    // 4) 下载官方安装器
    let installer = std::env::temp_dir().join(format!("gsml-{version_id}-installer.jar"));
    emit_install(app, version_id, "installer", 0, 1, "下载官方安装器…");
    download_with_progress(app, version_id, "installer", installer_url, &installer).await?;
    check_cancel()?;

    // 5) 运行安装器（可能要几分钟，阻塞等退出）
    emit_install(app, version_id, "installer", 0, 1, "正在运行官方安装器（可能需要几分钟）…");
    let before = dir_names(&versions_dir);
    let java_c = java.clone();
    let installer_c = installer.clone();
    let mc_dir = root.clone();
    let run = tauri::async_runtime::spawn_blocking(move || run_installer(&java_c, &installer_c, &mc_dir));
    run.await.map_err(|e| format!("安装器任务异常退出: {e}"))??;
    check_cancel()?;

    // 6) 探测安装器新建的版本目录（forge/neoforge 命名各有差异，不硬编码）
    let mut new_dirs: Vec<String> = dir_names(&versions_dir)
        .into_iter()
        .filter(|d| !before.contains(d))
        .collect();
    new_dirs.sort();
    let picked = new_dirs.into_iter().next()
        .ok_or("安装器未创建新版本目录（可能已安装或运行失败）")?;
    emit_install(app, &picked, "done", 1, 1, "安装完成");
    Ok(MinecraftVersion {
        name: picked,
        official_name: game_version.to_string(),
        loader,
        version_type: 0,
    })
}

fn dir_names(dir: &Path) -> HashSet<String> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn run_installer(java: &Path, installer: &Path, mc_dir: &Path) -> Result<(), String> {
    let out = Command::new(java)
        .args([
            "-jar",
            installer.to_string_lossy().as_ref(),
            "--installClient",
            mc_dir.to_string_lossy().as_ref(),
        ])
        .current_dir(mc_dir)
        .output()
        .map_err(|e| format!("无法启动 Java（{java:?}）: {e}"))?;
    if !out.status.success() {
        let mut msg = String::from_utf8_lossy(&out.stderr).to_string();
        if msg.len() < 200 {
            msg.push_str(&String::from_utf8_lossy(&out.stdout));
        }
        if msg.len() > 2000 {
            let mut cut = msg.len() - 2000;
            while !msg.is_char_boundary(cut) {
                cut += 1;
            }
            msg = msg[cut..].to_string();
        }
        return Err(format!(
            "官方安装器运行失败（exit {:?}）：{}",
            out.status.code(),
            msg.trim()
        ));
    }
    Ok(())
}

async fn install_forge(
    app: &AppHandle,
    game_version: &str,
    loader_version: &str,
    java_path: Option<&str>,
) -> Result<MinecraftVersion, String> {
    // loaderVersion 即清单里的完整版本字符串，如 "1.20.1-47.2.0"
    let url = format!("https://maven.minecraftforge.net/net/minecraftforge/forge/{loader_version}/forge-{loader_version}-installer.jar");
    install_installer_based(app, game_version, loader_version, &url, java_path, 2).await
}

async fn install_neoforge(
    app: &AppHandle,
    game_version: &str,
    loader_version: &str,
    java_path: Option<&str>,
) -> Result<MinecraftVersion, String> {
    // NeoForge 安装器生成的版本目录名形如 "<mc>-neoforge-<build>"，先按此探测是否已装过
    let root = get_minecraft_dir()?;
    let existing = dir_names(&root.join("versions")).into_iter().find(|d| {
        d.contains("-neoforge") && d.contains(loader_version)
    });
    if let Some(id) = existing {
        ensure_vanilla_base(app, game_version).await?;
        emit_install(app, &id, "done", 1, 1, "已安装，资源已就绪");
        return Ok(MinecraftVersion {
            name: id,
            official_name: game_version.to_string(),
            loader: 3,
            version_type: 0,
        });
    }
    let url = format!("https://maven.neoforged.net/releases/net/neoforged/neoforge/{loader_version}/neoforge-{loader_version}-installer.jar");
    let guess = format!("{game_version}-neoforge-{loader_version}");
    install_installer_based(app, game_version, &guess, &url, java_path, 3).await
}

/// 显式 javaPath > JAVA_HOME/bin/java.exe > PATH 上的 java（-version 试探）
fn find_java(explicit: Option<&str>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Ok(path);
        }
        let exe = path.join(java_exe());
        if exe.is_file() {
            return Ok(exe);
        }
        return Err(format!("指定的 Java 路径不存在: {p}"));
    }
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let exe = PathBuf::from(home).join("bin").join(java_exe());
        if exe.is_file() {
            return Ok(exe);
        }
    }
    if Command::new("java").arg("-version").output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Ok(PathBuf::from("java"));
    }
    Err("未找到 Java：Forge/NeoForge 安装需要 Java 17+，请先安装 Java 或在调用时传 javaPath".into())
}

#[cfg(windows)]
fn java_exe() -> &'static str { "java.exe" }
#[cfg(not(windows))]
fn java_exe() -> &'static str { "java" }

// ==================== Tauri 命令 ====================

/// 扫描 versions/ 下所有已安装版本；信息来自 {name}.json（官方版本 JSON）
#[tauri::command]
pub fn get_local_versions() -> Result<Vec<MinecraftVersion>, String> {
    let dir = get_minecraft_dir()?.join("versions");
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().into_string().ok() else { continue };
        let json: Value = fs::read_to_string(path.join(format!("{name}.json")))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let loader = detect_loader(json.to_string().as_str(), &name);
        let official_name = json["inheritsFrom"].as_str()
            .map(|s| s.to_string())
            .or_else(|| json["id"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| name.clone());
        let mut version_type = mc_type_u8(json["type"].as_str());
        if version_type == 4 && loader != 0 {
            version_type = 0; // fabric/forge/neoforge 的 JSON 一般没写 type，视为 release
        }
        out.push(MinecraftVersion { name, official_name, loader, version_type });
    }
    Ok(out)
}

/// 加载器识别：neoforged 优先于 forge（neoforge JSON 里也可能出现 forge 字样）
fn detect_loader(json: &str, id: &str) -> u8 {
    if json.contains("neoforged") || id.contains("neoforge") {
        3
    } else if json.contains("fabricmc") || id.contains("fabric") {
        1
    } else if json.contains("minecraftforge") || id.contains("forge") {
        2
    } else {
        0
    }
}

/// 远程版本列表
/// loader=0: 全部原版版本（release + snapshot，新→旧）
/// loader=1: 指定 MC 版本可用的 fabric loader
/// loader=2: forge 构建列表（gameVersion 提供时过滤，name 形如 1.20.1-47.2.0，新→旧）
/// loader=3: neoforge 构建列表（同上，name 形如 21.1.77）
#[tauri::command]
pub async fn list_remote_versions(
    loader: u8,
    game_version: Option<String>,
) -> Result<Vec<MinecraftVersion>, String> {
    match loader {
        0 => list_vanilla().await,
        1 => {
            let gv = game_version.ok_or("loader=1 需要 gameVersion（目标 MC 版本）")?;
            list_fabric_loaders(&gv).await
        }
        2 => list_forge_builds(game_version.as_deref()).await,
        3 => {
            let gv = game_version.ok_or("loader=3 需要 gameVersion（目标 MC 版本）")?;
            list_neoforge_builds(&gv).await
        }
        _ => Err(format!("未知 loader: {loader}（0 vanilla / 1 fabric / 2 forge / 3 neoforge）")),
    }
}

async fn list_vanilla() -> Result<Vec<MinecraftVersion>, String> {
    let manifest = fetch_json("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json").await?;
    let arr = manifest["versions"].as_array().ok_or("官方清单格式异常")?;
    Ok(arr.iter().filter_map(|v| {
        let id = v["id"].as_str()?.to_string();
        let t = mc_type_u8(v["type"].as_str());
        Some(MinecraftVersion { name: id.clone(), official_name: id, loader: 0, version_type: t })
    }).collect())
}

async fn list_fabric_loaders(game_version: &str) -> Result<Vec<MinecraftVersion>, String> {
    let url = format!("https://meta.fabricmc.net/v2/versions/loader/{game_version}");
    let data = fetch_json(&url).await
        .map_err(|e| format!("查询 Fabric 加载器失败（该 MC 版本可能不受支持）: {e}"))?;
    let arr = data.as_array().ok_or("Fabric meta 格式异常")?;
    Ok(arr.iter().filter_map(|e| {
        let lv = e["loader"]["version"].as_str()?.to_string();
        let stable = e["loader"]["stable"].as_bool().unwrap_or(true);
        Some(MinecraftVersion {
            name: lv,
            official_name: game_version.to_string(),
            loader: 1,
            version_type: if stable { 0 } else { 1 },
        })
    }).collect())
}

async fn list_forge_builds(game_version: Option<&str>) -> Result<Vec<MinecraftVersion>, String> {
    let xml = fetch_text("https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml").await?;
    let mut out: Vec<MinecraftVersion> = xml_versions(&xml).into_iter()
        .filter(|v| game_version.map_or(true, |g| v.starts_with(&format!("{g}-"))))
        .map(|v| {
            // 未传 gameVersion 时从构建号解析官方名称（"1.20.1-47.2.0" -> "1.20.1"）
            let official = game_version
                .map(|g| g.to_string())
                .unwrap_or_else(|| v.split('-').next().unwrap_or(&v).to_string());
            MinecraftVersion { name: v, official_name: official, loader: 2, version_type: 0 }
        })
        .collect();
    out.reverse(); // maven-metadata 是升序，翻转让新的排前面
    Ok(out)
}

async fn list_neoforge_builds(game_version: &str) -> Result<Vec<MinecraftVersion>, String> {
    // NeoForge 版本号跟 MC 前缀对应：MC 1.21.1 -> neoforge 21.1.x
    let short = game_version.strip_prefix("1.")
        .ok_or_else(|| format!("无法从 MC 版本 {game_version} 推导 NeoForge 前缀"))?;
    let prefix = format!("{short}.");
    let url = "https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge";
    let data = fetch_json(url).await.map_err(|e| format!("查询 NeoForge 失败: {e}"))?;
    let arr = data["versions"].as_array().ok_or("NeoForge API 格式异常")?;
    let mut out: Vec<MinecraftVersion> = arr.iter().filter_map(|v| {
        let s = v.as_str()?;
        s.starts_with(&prefix).then(|| MinecraftVersion {
        name: s.to_string(),
        official_name: game_version.to_string(),
        loader: 3,
        version_type: 0,
    })
    }).collect();
    out.reverse(); // API 是升序，翻转让新的排前面
    Ok(out)
}

/// 手工切 <version>...</version>（工程没引 regex，不值得为这一个解析加依赖）
fn xml_versions(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(a) = rest.find("<version>") {
        let after = &rest[a + 9..];
        let Some(b) = after.find("</version>") else { break };
        out.push(after[..b].trim().to_string());
        rest = &after[b + 10..];
    }
    out
}

/// 安装一个版本（长任务，进度经 install-progress 事件广播）
/// loader: 0 vanilla（loaderVersion 忽略）
///         1 fabric（loaderVersion = fabric loader 版本，如 0.16.14）
///         2 forge（loaderVersion = 清单完整版本，如 1.20.1-47.2.0）
///         3 neoforge（loaderVersion = neoforge 版本，如 21.1.77）
/// javaPath: 仅 forge/neoforge 需要，缺省按 JAVA_HOME → PATH 探测
/// customName: 自定义安装名（None/空 = 用默认 id）。提供且不等于默认 id 时，
///             安装完成后会把版本落地为 versions/{customName}/{customName}.json
///             （JSON 里的 id 改写为自定义名，client jar 同名硬链接/复制），
///             get_local_versions 即可按自定义名扫出该版本。
#[tauri::command]
pub async fn install_version(
    app: AppHandle,
    game_version: String,
    loader: u8,
    loader_version: Option<String>,
    java_path: Option<String>,
    custom_name: Option<String>,
) -> Result<MinecraftVersion, String> {
    // 自定义名清洗：trim 后为空视同 None；其余不允许路径分隔符与 Windows 保留字符
    let custom_name = custom_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(n) = &custom_name {
        let bad = n.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) || n.contains("..");
        if bad {
            return Err(format!("自定义名不合法: {n}（不能包含 / \\ : * ? \" < > |）"));
        }
    }
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("已有安装任务正在进行".to_string());
    }
    CANCEL.store(false, Ordering::Relaxed);
    let result = match loader {
        0 => install_vanilla(&app, &game_version).await,
        1 => {
            let lv = loader_version.ok_or("loader=1 需要 loaderVersion（fabric loader 版本）")?;
            install_fabric(&app, &game_version, &lv).await
        }
        2 => {
            let lv = loader_version.ok_or("loader=2 需要 loaderVersion（forge 完整版本，如 1.20.1-47.2.0）")?;
            install_forge(&app, &game_version, &lv, java_path.as_deref()).await
        }
        3 => {
            let lv = loader_version.ok_or("loader=3 需要 loaderVersion（neoforge 版本，如 21.1.77）")?;
            install_neoforge(&app, &game_version, &lv, java_path.as_deref()).await
        }
        _ => Err(format!("未知 loader: {loader}")),
    };
    INSTALLING.store(false, Ordering::Relaxed);
    // 按自定义名落地版本目录（覆盖包括"已安装，资源已就绪"在内的提前返回路径）
    match (result, custom_name) {
        (Ok(v), Some(cn)) if cn != v.name => {
            finalize_custom_name(&v.name, &cn)?;
            Ok(MinecraftVersion { name: cn, ..v })
        }
        (r, _) => r,
    }
}

/// 把已按默认 id 安装好的版本目录整理成自定义名：
/// versions/{natural}/{natural}.json → versions/{custom}/{custom}.json
/// （id 改写为 custom，client jar 同名硬链接，跨卷/失败回退普通复制）。
/// fabric 这类纯 inheritsFrom 的版本没有 jar，跳过即可。
/// 目标已存在 {custom}.json 时不覆盖（视为装过同一个名字）。
fn finalize_custom_name(natural_id: &str, custom: &str) -> Result<(), String> {
    let versions = get_minecraft_dir()?.join("versions");
    let src = versions.join(natural_id);
    let dst = versions.join(custom);
    let dst_json = dst.join(format!("{custom}.json"));
    if dst_json.is_file() {
        return Ok(());
    }
    let src_json = src.join(format!("{natural_id}.json"));
    if !src_json.is_file() {
        return Err(format!("安装结果缺少版本 JSON: {}", src_json.display()));
    }
    fs::create_dir_all(&dst).map_err(io_err)?;
    let mut json: Value = serde_json::from_str(&fs::read_to_string(&src_json).map_err(io_err)?)
        .map_err(|e| format!("解析版本 JSON 失败: {e}"))?;
    json["id"] = Value::String(custom.to_string());
    let pretty = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    fs::write(&dst_json, pretty).map_err(io_err)?;
    let src_jar = src.join(format!("{natural_id}.jar"));
    if src_jar.is_file() {
        let dst_jar = dst.join(format!("{custom}.jar"));
        if !dst_jar.is_file() {
            fs::hard_link(&src_jar, &dst_jar)
                .or_else(|_| fs::copy(&src_jar, &dst_jar).map(|_| ()))
                .map_err(io_err)?;
        }
    }
    Ok(())
}

/// 取消当前安装（下次 install_version 开头自动复位）
#[tauri::command]
pub fn cancel_install() {
    CANCEL.store(true, Ordering::Relaxed);
}