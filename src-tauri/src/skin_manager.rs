// skin_manager.rs — 皮肤渲染与磁盘缓存
//
// 目录结构（data_root() 下，与 app.db 同级；Windows 上即
// %LOCALAPPDATA%\Syntropica\GlowSquidMinecraftLauncher\skin\）：
//   skin/microsoft/<uuid>/head.png    8×8    头像（脸 + 帽子层叠加）
//   skin/microsoft/<uuid>/body.png    16×32  正面全身像
//   skin/offline/<uuid>/head.png / body.png   同上
//   skin/default.head.png / default.body.png  内置默认皮肤（Steve 配色）
//   capes/{alias}.png  披风 pill 的展示图（64×32 材质裁出的正面躯干 10×15）
//     文件名 = 别名小写 + 空格换下划线（如 "Aurora" → aurora.png）
//
// 皮肤来源：
//   · microsoft → 登录时 profile.skins[ACTIVE].url（textures.minecraft.net）；
//     缓存缺失时经 sessionserver.mojang.com 公开接口按 uuid 补拉（无需令牌）
//   · offline   → 同名微软账号的缓存 → minotar / mc-heads 按玩家名查询 → 默认皮肤
//
// 前端没有配置 assetProtocol，图片一律由命令返回 base64 data URL。

use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageFormat, Rgba, RgbaImage};
use serde::Serialize;
use tauri::State;

use crate::db_handler::Db;

/// 全局串行锁：同一时刻只做一次「解析 → 渲染 → 写缓存」，避免同一账号被
/// 前端头像/预览并发请求时重复下载与写入。锁要跨 await，必须用 tokio 的
/// Mutex（std MutexGuard 不是 Send，跨 await 会让 async command 编译失败）。
static SKIN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Serialize)]
pub struct SkinData {
    /// 8×8 头像 PNG 的 base64
    pub head_base64: String,
    /// 16×32 正面全身 PNG 的 base64
    pub body_base64: String,
}

// ── 目录与编码 ───────────────────────────────────────────────────────────

fn skin_root() -> Result<PathBuf, String> {
    Ok(crate::db_handler::data_root()?.join("skin"))
}

/// 某类账号的皮肤缓存目录：skin/{microsoft|offline}/<uuid>/
fn kind_dir(account_type: i64, uuid: &str) -> Result<PathBuf, String> {
    let kind = if account_type == 0 { "microsoft" } else { "offline" };
    Ok(skin_root()?.join(kind).join(uuid))
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png)
        .map_err(|e| format!("PNG 编码失败: {e}"))?;
    Ok(buf.into_inner())
}

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

// ── 像素级渲染 ───────────────────────────────────────────────────────────

/// 从皮肤贴图裁剪 (x, y, w, h) 区域
fn crop(img: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> RgbaImage {
    let mut out = RgbaImage::new(w, h);
    for py in 0..h {
        for px in 0..w {
            let sx = (x + px).min(img.width().saturating_sub(1));
            let sy = (y + py).min(img.height().saturating_sub(1));
            out.put_pixel(px, py, *img.get_pixel(sx, sy));
        }
    }
    out
}

/// 把 layer 以 alpha 混合叠到 base 的 (dx, dy) 处；layer 的透明像素跳过
fn blend(base: &mut RgbaImage, layer: &RgbaImage, dx: u32, dy: u32) {
    for py in 0..layer.height() {
        for px in 0..layer.width() {
            let o = *layer.get_pixel(px, py);
            if o.0[3] == 0 {
                continue; // overlay 的透明像素不遮盖 base
            }
            let (tx, ty) = (dx + px, dy + py);
            if tx >= base.width() || ty >= base.height() {
                continue;
            }
            if o.0[3] == 255 {
                base.put_pixel(tx, ty, o);
                continue;
            }
            let b = *base.get_pixel(tx, ty);
            let a = o.0[3] as u32;
            let inv = 255 - a;
            let mix = |ov: u8, bv: u8| ((ov as u32 * a + bv as u32 * inv) / 255) as u8;
            let alpha = (a + b.0[3] as u32 * inv / 255).min(255) as u8;
            base.put_pixel(
                tx,
                ty,
                Rgba([
                    mix(o.0[0], b.0[0]),
                    mix(o.0[1], b.0[1]),
                    mix(o.0[2], b.0[2]),
                    alpha,
                ]),
            );
        }
    }
}

// ── 默认皮肤───────────────

const BORDER: Rgba<u8> = Rgba([124, 184, 231, 255]);
const FILLING: Rgba<u8> = Rgba([148, 206, 243, 255]);
const EYES: Rgba<u8> = Rgba([235, 246, 252, 255]);
const TRANSPARENT: Rgba<u8> = Rgba([0, 0, 0, 0]);

/// 8×8 默认脸
fn default_head() -> RgbaImage {
    let mut head = RgbaImage::new(8, 8);
    for x in 0..8 {
        for y in 0..8 {
            head.put_pixel(x, y, if x == 0 || x == 7 || y == 0 || y == 7 {
                BORDER
            } else {
                FILLING
            });
        }
    }
    head.put_pixel(2, 3, EYES);
    head.put_pixel(5, 3, EYES);
    head
}

/// 16×32 默认正面全身（classic 模型：裸臂）
fn default_body() -> RgbaImage {
    let mut body = RgbaImage::new(16, 32);
    for x in 0..16 {
        for y in 0..32 {
            body.put_pixel(x, y, if y == 0 || y == 8 || y == 19 || y == 31 || x == 0 || x == 4 || x == 11 || x == 15 {
                BORDER
            } else {
                FILLING
            });
        }
    }
    for x in 0..4 {
        for y in 0..8 {body.put_pixel(x, y, TRANSPARENT);}
        for y in 20..32 {body.put_pixel(x, y, TRANSPARENT);}
    }
    for x in 12..16 {
        for y in 0..8 {body.put_pixel(x, y, TRANSPARENT);}
        for y in 20..32 {body.put_pixel(x, y, TRANSPARENT);}
    }
    body.put_pixel(6, 3, EYES);
    body.put_pixel(9, 3, EYES);
    body
}

/// 确保 skin/default.{head,body}.png 存在，返回 skin 根目录
fn ensure_default_skin() -> Result<PathBuf, String> {
    let root = skin_root()?;
    std::fs::create_dir_all(&root).map_err(|e| format!("创建 skin 目录失败: {e}"))?;
    let head_path = root.join("default.head.png");
    let body_path = root.join("default.body.png");
    if !head_path.exists() {
        std::fs::write(&head_path, encode_png(&default_head())?)
            .map_err(|e| format!("写入 default.head.png 失败: {e}"))?;
    }
    if !body_path.exists() {
        std::fs::write(&body_path, encode_png(&default_body())?)
            .map_err(|e| format!("写入 default.body.png 失败: {e}"))?;
    }
    Ok(root)
}

/// 默认渲染结果（优先读磁盘，读不到就现算）
fn default_pair(root: &Path) -> (Vec<u8>, Vec<u8>) {
    let head = std::fs::read(root.join("default.head.png"))
        .unwrap_or_else(|_| encode_png(&default_head()).unwrap_or_default());
    let body = std::fs::read(root.join("default.body.png"))
        .unwrap_or_else(|_| encode_png(&default_body()).unwrap_or_default());
    (head, body)
}

// ── 皮肤贴图 → 渲染 ─────────────────────────────────────────────────────

/// 把一张 Minecraft 皮肤贴图渲染成 (head 8×8, body 16×32)。
/// 支持多层：hat 帽子层两种格式坐标相同；64×64 的第二层 overlay
/// （jacket/sleeve/pants）会叠加到第一层之后再写缓存；
/// legacy 64×32 没有第二层 overlay，左侧由右侧水平镜像。
fn render_skin_bytes(png: &[u8], slim: bool) -> Result<(Vec<u8>, Vec<u8>), String> {
    let skin = image::load_from_memory(png)
        .map_err(|e| format!("皮肤 PNG 解析失败: {e}"))?
        .to_rgba8();
    let (w, h) = (skin.width(), skin.height());
    if w != 64 || (h != 64 && h != 32) {
        return Err(format!("不支持的皮肤尺寸 {w}×{h}（仅支持 64×64 / 64×32）"));
    }

    // 头像：脸 (8,8) + 帽子层 (40,8)
    let mut head = crop(&skin, 8, 8, 8, 8);
    blend(&mut head, &crop(&skin, 40, 8, 8, 8), 0, 0);

    // 正面全身 16×32
    let arm_w = if slim { 3 } else { 4 };
    let mut body = RgbaImage::new(16, 32);
    blend(&mut body, &head, 4, 0);                            // 头（含帽子层）
    blend(&mut body, &crop(&skin, 20, 20, 8, 12), 4, 8);      // 躯干
    blend(&mut body, &crop(&skin, 44, 20, arm_w, 12), 0, 8);  // 右臂
    blend(&mut body, &crop(&skin, 4, 20, 4, 12), 4, 20);      // 右腿

    if h == 32 {
        // legacy 64×32：左侧 = 右侧水平镜像，没有第二层 overlay
        let arm = image::imageops::flip_horizontal(&crop(&skin, 44, 20, arm_w, 12));
        let leg = image::imageops::flip_horizontal(&crop(&skin, 4, 20, 4, 12));
        blend(&mut body, &arm, 16 - arm_w, 8);
        blend(&mut body, &leg, 8, 20);
    } else {
        // 64×64 第二层 overlay：jacket/sleeve/pants 叠加到第一层之后
        blend(&mut body, &crop(&skin, 20, 36, 8, 12), 4, 8);      // 外套层
        blend(&mut body, &crop(&skin, 44, 36, arm_w, 12), 0, 8);  // 右袖
        blend(&mut body, &crop(&skin, 4, 36, 4, 12), 4, 20);      // 右裤
        // 64×64：左臂 (36,52) / 左袖 (52|51,52) / 左腿 (20,52) / 左裤 (4,52)
        let left_sleeve_x = if slim { 51 } else { 52 };
        blend(&mut body, &crop(&skin, 36, 52, arm_w, 12), 16 - arm_w, 8); // 左臂
        blend(&mut body, &crop(&skin, left_sleeve_x, 52, arm_w, 12), 16 - arm_w, 8); // 左袖
        blend(&mut body, &crop(&skin, 20, 52, 4, 12), 8, 20); // 左腿
        blend(&mut body, &crop(&skin, 4, 52, 4, 12), 8, 20);  // 左裤
    }

    Ok((encode_png(&head)?, encode_png(&body)?))
}

// ── 皮肤获取 ─────────────────────────────────────────────────────────────

/// GET 一个 URL，返回字节（皮肤贴图下载）
async fn http_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = crate::account_manager::http_client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("请求 {url} 失败 ({status})"));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取 {url} 失败: {e}"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(format!("{url} 响应过大（{} 字节）", bytes.len()));
    }
    Ok(bytes.to_vec())
}

/// 玩家名只允许 [A-Za-z0-9_] 且 ≤16 字符，防止拼 URL 时注入
fn sanitize_name(name: &str) -> Option<&str> {
    let name = name.trim();
    if name.is_empty()
        || name.len() > 16
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    Some(name)
}

/// sessionserver textures 属性（base64 JSON）→ (皮肤 URL, 是否 slim)
fn parse_session_textures(value_b64: &str) -> Option<(String, bool)> {
    let raw = STANDARD.decode(value_b64).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let skin = json.get("textures")?.get("SKIN")?;
    let url = skin.get("url")?.as_str()?.to_string();
    let slim = skin
        .get("metadata")
        .and_then(|m| m.get("model"))
        .and_then(|m| m.as_str())
        .map(|m| m.eq_ignore_ascii_case("slim"))
        .unwrap_or(false);
    Some((url, slim))
}

/// 微软账号：sessionserver 公开接口按 uuid 补拉皮肤（无需登录令牌）
async fn resolve_microsoft_skin(uuid: &str) -> Option<(Vec<u8>, bool)> {
    let url = format!("https://sessionserver.mojang.com/session/minecraft/profile/{uuid}");
    let resp = crate::account_manager::http_client().get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        eprintln!("[skin] sessionserver 拉取 {uuid} 失败 ({})", resp.status());
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let prop = json
        .get("properties")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(|n| n.as_str()) == Some("textures"))
        .and_then(|p| p.get("value"))
        .and_then(|v| v.as_str())?;
    let (skin_url, slim) = parse_session_textures(prop)?;
    match http_bytes(&skin_url).await {
        Ok(png) => Some((png, slim)),
        Err(err) => {
            eprintln!("[skin] 下载皮肤 {skin_url} 失败: {err}");
            None
        }
    }
}

/// 离线账号：按玩家名查询公共皮肤源（都失败则回退默认皮肤）
async fn resolve_offline_skin(name: &str) -> Option<(Vec<u8>, bool)> {
    let name = sanitize_name(name)?;
    for url in [
        format!("https://minotar.net/skin/{name}"),
        format!("https://mc-heads.net/skin/{name}"),
    ] {
        match http_bytes(&url).await {
            Ok(png) => return Some((png, false)),
            Err(err) => eprintln!("[skin] {url} 查询失败: {err}"),
        }
    }
    eprintln!("[skin] 离线玩家 {name} 未查到皮肤，使用默认皮肤");
    None
}

// ── 写缓存 ───────────────────────────────────────────────────────────────

/// 微软登录成功后调用：下载 profile 里的 ACTIVE 皮肤并渲染进缓存。
/// best effort —— 失败只打 stderr，不影响登录结果。
pub async fn cache_microsoft_skin(uuid: &str, skin_url: &str, slim: bool) {
    if let Err(err) = cache_microsoft_skin_inner(uuid, skin_url, slim).await {
        eprintln!("[skin] 缓存微软账号 {uuid} 皮肤失败: {err}");
    }
}

async fn cache_microsoft_skin_inner(
    uuid: &str,
    skin_url: &str,
    slim: bool,
) -> Result<(), String> {
    let png = http_bytes(skin_url).await?;
    let (head, body) = render_skin_bytes(&png, slim)?;
    let dir = kind_dir(0, uuid)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建皮肤目录失败: {e}"))?;
    std::fs::write(dir.join("head.png"), head).map_err(|e| format!("写入 head.png 失败: {e}"))?;
    std::fs::write(dir.join("body.png"), body).map_err(|e| format!("写入 body.png 失败: {e}"))?;
    Ok(())
}

// ── 命令 ─────────────────────────────────────────────────────────────────

/// 返回某账号的皮肤（head 8×8 + body 16×32，base64 PNG）。
/// 缓存命中直接读盘；未命中按账号类型解析皮肤、渲染、写缓存后返回；
/// 全部失败时回退内置默认皮肤。
#[tauri::command]
pub async fn get_account_skin(
    db: State<'_, Db>,
    account_id: u16,
) -> Result<SkinData, String> {
    let root = ensure_default_skin()?;

    // 1) 查账号（uuid / name / account_type）；数据库锁只在同步段内持有
    let (uuid, name, account_type) = {
        let conn = db.lock()?;
        conn.query_row(
            "SELECT uuid, name, account_type FROM accounts WHERE id = ?1",
            rusqlite::params![account_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)),
        )
        .map_err(|_| format!("账号不存在 (id={account_id})"))?
    };

    let dir = kind_dir(account_type, &uuid)?;
    let head_path = dir.join("head.png");
    let body_path = dir.join("body.png");

    // 2) 缓存命中直接返回
    if let (Ok(head), Ok(body)) = (std::fs::read(&head_path), std::fs::read(&body_path)) {
        return Ok(SkinData {
            head_base64: b64(&head),
            body_base64: b64(&body),
        });
    }

    // 3) 未命中：全局锁 + 锁内双检，避免并发重复下载
    let _guard = SKIN_LOCK.lock().await;
    if let (Ok(head), Ok(body)) = (std::fs::read(&head_path), std::fs::read(&body_path)) {
        return Ok(SkinData {
            head_base64: b64(&head),
            body_base64: b64(&body),
        });
    }

    // 4) 解析皮肤来源
    let mut resolved: Option<(Vec<u8>, Vec<u8>)> = None;
    if account_type == 0 {
        if let Some((png, slim)) = resolve_microsoft_skin(&uuid).await {
            if let Ok(pair) = render_skin_bytes(&png, slim) {
                resolved = Some(pair);
            }
        }
    } else {
        // 离线账号优先复用同名微软账号已缓存的皮肤（零网络）
        let reuse: Option<String> = {
            let conn = db.lock()?;
            conn.query_row(
                "SELECT uuid FROM accounts WHERE account_type = 0 AND name = ?1 LIMIT 1",
                rusqlite::params![name],
                |r| Ok(r.get::<_, String>(0)?),
            )
            .ok()
        };
        if let Some(ms_uuid) = reuse {
            if let Ok(src) = kind_dir(0, &ms_uuid) {
                if let (Ok(head), Ok(body)) = (
                    std::fs::read(src.join("head.png")),
                    std::fs::read(src.join("body.png")),
                ) {
                    resolved = Some((head, body));
                }
            }
        }
        if resolved.is_none() {
            if let Some((png, slim)) = resolve_offline_skin(&name).await {
                if let Ok(pair) = render_skin_bytes(&png, slim) {
                    resolved = Some(pair);
                }
            }
        }
    }

    // 5) 兜底默认皮肤
    let (head_bytes, body_bytes) = resolved.unwrap_or_else(|| default_pair(&root));

    // 6) 写缓存并返回
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建皮肤目录失败: {e}"))?;
    std::fs::write(&head_path, &head_bytes).map_err(|e| format!("写入 head.png 失败: {e}"))?;
    std::fs::write(&body_path, &body_bytes).map_err(|e| format!("写入 body.png 失败: {e}"))?;
    Ok(SkinData {
        head_base64: b64(&head_bytes),
        body_base64: b64(&body_bytes),
    })
}

// ── 披风材质 ────────────────────────────────────────────────────────────
// 披风选择 pill 的展示图：下载 64×32 完整披风材质，裁出正面披风区
// (x=1, y=1, 10×16)，按别名缓存到 data_root()/capes/{name}.png
// （别名小写 + 空格换下划线，如 "Aurora" → aurora.png），返回 base64 data URL。
#[tauri::command]
pub async fn get_cape_texture(
    url: String,
    alias: String,
) -> Result<String, String> {
    // 缓存文件名：别名小写 + 空格换下划线；空别名回退到 URL 末段
    let mut name: String = alias.trim().to_lowercase().replace(' ', "_");
    if name.is_empty() {
        name = url
            .rsplit('/')
            .next()
            .unwrap_or("cape")
            .to_lowercase();
    }
    // 文件名只留 [A-Za-z0-9_-.] 防止路径注入
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '.')
        .collect();
    let safe = if safe.is_empty() { "cape".to_string() } else { safe };

    let dir = crate::db_handler::data_root()?.join("capes");
    let path = dir.join(format!("{safe}.png"));
    if let Ok(cached) = std::fs::read(&path) {
        return Ok(format!("data:image/png;base64,{}", b64(&cached)));
    }
    let bytes = http_bytes(&url).await?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("披风材质解码失败: {e}"))?
        .to_rgba8();
    let cropped = crop(&img, 1, 1, 10, 16);
    let png = encode_png(&cropped)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建披风目录失败: {e}"))?;
    std::fs::write(&path, &png).map_err(|e| format!("写入披风材质失败: {e}"))?;
    Ok(format!("data:image/png;base64,{}", b64(&png)))
}