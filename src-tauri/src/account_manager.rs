use std::collections::HashMap;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use md5::Md5;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;
use url::Url;

use crate::db_handler::{Db, AccountRow};
use crate::deeplink_handler;
use crate::skin_manager;

// ── Azure 应用信息（公开客户端：无密钥、PKCE，这些值本来就公开，直接编译进二进制）──
// 不再从 npm_package_config_* 环境变量读取：
// 1) 环境变量只在 pnpm/npm 脚本进程里存在，release 双击启动时读不到；
// 2) dev 进程启动后改 package.json 不生效，容易带着旧值调试。
const AZURE_CLIENT_ID: &str = "c2dad5eb-7f74-4fb3-afde-9c29a6f60988";
// tenants 取值：consumers = 仅个人 Microsoft 账户（Minecraft 账号都是绑 Xbox 的个人 MSA）。
// 不能用组织租户 GUID / organizations：那里没有 XboxLive.signin scope，会报 AADSTS650053。
const AZURE_TENANT: &str = "consumers";

// MS OAuth 回调 deeplink（tauri.conf.json 里已注册 "glow-squid" scheme）
const REDIRECT_URI: &str = "glow-squid://ms-auth";
const DEEPLINK_SCHEME: &str = "glow-squid";
const DEEPLINK_HOST: &str = "ms-auth";
// 等待浏览器回调的超时
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

// 共享 HTTP 客户端：复用连接池（Keep-Alive + TLS 会话复用）。
// 之前每次调用都 new 一个 Client，导致披风/皮肤批量下载时每个请求
// 都要重新做 TCP+TLS 握手，这是首次加载慢的根源。
pub(crate) fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("build reqwest client")
        })
        .clone()
}

// ── 登录进度广播 ─────────────────────────────────────────────────────────

/// 登录步骤总数（Rust 广播的 step 与前端进度条换算共用）
const LOGIN_STEPS_TOTAL: u8 = 8;

/// 广播微软登录进度：home.js 监听 ms-login-progress 事件，
/// 用 Material 3 线性进度条展示当前进行到哪一步。
/// payload: { step, total, label }
fn emit_progress(app: &AppHandle, step: u8, label: &str) {
    let payload = serde_json::json!({ "step": step, "total": LOGIN_STEPS_TOTAL, "label": label });
    if let Err(e) = app.emit("ms-login-progress", payload) {
        eprintln!("[ms-auth] 进度事件发送失败: {e}");
    }
}

/// 发送带/不带 JSON body 的 HTTP 请求（任意 Method）并返回 (状态码, 解析后的 JSON)。
/// body 为 None 时不携带请求体（如 DELETE 卸下披风）。
/// 空响应体（DELETE 成功返回空负载）解析为 JSON null。
/// 网络失败或响应体不是合法 JSON 时，错误信息带上状态码与响应体（截断），
/// 避免 "error decoding response body" 这类看不出原因的报错。
async fn send_json(
    method: reqwest::Method,
    url: &str,
    extra_headers: &[(&str, &str)],
    body: Option<&serde_json::Value>,
) -> Result<(reqwest::StatusCode, serde_json::Value), String> {
    let mut req = http_client()
        .request(method, url)
        .header("Accept", "application/json");
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }
    if let Some(body) = body {
        req = req.json(body);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取 {url} 响应失败: {e}"))?;
    let json = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text).map_err(|e| {
            format!("解析 {url} 响应失败 ({status}): {e}；响应体: {}", truncate(&text, 400))
        })?
    };
    Ok((status, json))
}

/// 发送 JSON POST 并返回 (状态码, 解析后的 JSON)。
async fn post_json(
    url: &str,
    extra_headers: &[(&str, &str)],
    body: &serde_json::Value,
) -> Result<(reqwest::StatusCode, serde_json::Value), String> {
    send_json(reqwest::Method::POST, url, extra_headers, Some(body)).await
}

/// 截断文本用于错误信息（按字符截，避免切在 UTF-8 边界上 panic）。
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max_chars).collect();
        format!("{cut}…")
    }
}

/// 把 JSON 值压成单行文本并截断，用于错误信息。
fn describe_json(value: &serde_json::Value, max_chars: usize) -> String {
    truncate(&value.to_string(), max_chars)
}

fn unix_now() -> Result<i64, String> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| e.to_string())
}

fn generate_pkce() -> (String, String) {
    let bytes: [u8; 32] = rand::random();
    let verifier = URL_SAFE_NO_PAD.encode(bytes); // 43 字符左右
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn generate_state() -> String {
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn offline_uuid(name: &str) -> String {
    let digest = Md5::digest(format!("OfflinePlayer:{name}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest);
    bytes[6] = (bytes[6] & 0x0f) | 0x30; // version 3
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 2
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── 离线登录 ─────────────────────────────────────────────────────────────

#[tauri::command]
pub fn login_offline(db: State<'_, Db>, name: String) -> Result<AccountRow, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("用户名不能为空".into());
    }

    let uuid = offline_uuid(&name);
    let now = unix_now()?;

    let conn = db.lock()?;
    // 同名离线账号重复登录时视为同一账号，只更新 last_used
    conn.execute(
        "INSERT INTO accounts(uuid, name, account_type, last_used)
         VALUES(?1, ?2, 1, ?3)
         ON CONFLICT(uuid) DO UPDATE SET name = excluded.name, last_used = excluded.last_used",
        rusqlite::params![uuid, name, now],
    )
    .map_err(|e| e.to_string())?;

    let (id, created_at): (i64, i64) = conn
        .query_row(
            "SELECT id, created_at FROM accounts WHERE uuid = ?1",
            rusqlite::params![uuid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    Ok(AccountRow {
        id,
        uuid,
        name,
        account_type: 1, // 1 = offline（见 db_handler.rs 顶部注释）
        msa_id: None,
        last_used: Some(now),
        created_at,
    })
}

// ── 列出账号 ─────────────────────────────────────────────────────────────

#[tauri::command]
pub fn list_accounts(db: State<'_, Db>) -> Result<Vec<AccountRow>, String> {
    let conn = db.lock()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, uuid, name, account_type, msa_id, last_used, created_at
             FROM accounts
             ORDER BY COALESCE(last_used, created_at) DESC",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map([], |r| {
            Ok(AccountRow {
                id: r.get(0)?,
                uuid: r.get(1)?,
                name: r.get(2)?,
                account_type: r.get(3)?,
                msa_id: r.get(4)?,
                last_used: r.get(5)?,
                created_at: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

// ── 微软登录 ─────────────────────────────────────────────────────────────

/// MSA token 接口返回的令牌
#[derive(Debug)]
struct MsaTokens {
    access_token: String,
    refresh_token: Option<String>,
}

/// profile.skins 里的一项
#[derive(Debug, Deserialize)]
struct McSkin {
    state: Option<String>,   // "ACTIVE" 才是当前佩戴的皮肤
    url: Option<String>,     // textures.minecraft.net 上的贴图地址
    variant: Option<String>, // "SLIM"（Alex 型 3px 臂）/ "CLASSIC"（Steve 型 4px 臂）
}

impl McProfile {
    /// 当前佩戴（ACTIVE）皮肤的 URL 与是否 slim；skins 缺失时返回 None
    fn active_skin(&self) -> Option<(&str, bool)> {
        let skin = self
            .skins
            .iter()
            .find(|s| s.state.as_deref() == Some("ACTIVE"))
            .or_else(|| self.skins.first())?;
        let url = skin.url.as_deref()?;
        let slim = skin
            .variant
            .as_deref()
            .map(|v| v.eq_ignore_ascii_case("slim"))
            .unwrap_or(false);
        Some((url, slim))
    }
}

/// profile.capes 里的一项（披风）
#[derive(Debug, Deserialize)]
struct McCape {
    id: String,              // 披风的 UUID，切换披风时用
    #[serde(default)]
    state: Option<String>,   // "ACTIVE" = 当前装备的披风
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// Minecraft profile 接口返回的玩家资料
#[derive(Debug, Deserialize)]
struct McProfile {
    id: String,
    name: String,
    /// 皮肤列表；万一接口没带则空 Vec，回退默认皮肤
    #[serde(default)]
    skins: Vec<McSkin>,
    /// 披风列表；同样可能缺失
    #[serde(default)]
    capes: Vec<McCape>,
}

#[tauri::command]
pub async fn login_microsoft(app: AppHandle, db: State<'_, Db>) -> Result<AccountRow, String> {
    let client_id = AZURE_CLIENT_ID;
    let tenant_id = AZURE_TENANT;

    // 1) PKCE + state + 回调 channel
    let (code_verifier, code_challenge) = generate_pkce();
    let state = generate_state();
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
    // oneshot::Sender 不是 Sync，而 deeplink handler 要求 Send + Sync；
    // 包一层 Mutex<Option<_>>，发送时用 take() 把 Sender 从 guard 里移出来
    let tx = std::sync::Mutex::new(Some(tx));

    // 2) 注册 deeplink 回调，等浏览器回调 glow-squid://ms-auth
    let expected_state = state.clone();
    let sub = deeplink_handler::register(DEEPLINK_SCHEME, Some(DEEPLINK_HOST), move |url| {
        let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if pairs.get("state").map(String::as_str) != Some(expected_state.as_str()) {
            eprintln!("[ms-auth] state 不匹配，忽略: {url}");
            return;
        }
        if let Some(desc) = pairs.get("error_description").or(pairs.get("error")) {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(Err(format!("微软登录失败: {desc}")));
            }
        } else if let Some(code) = pairs.get("code") {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(Ok(code.clone()));
            }
        }
    });

    // 3) 拼授权 URL 并打开系统浏览器
    let mut auth_url = Url::parse(&format!(
        "https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/authorize"
    ))
    .map_err(|e| format!("拼授权 URL 失败: {e}"))?;
    auth_url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("response_mode", "query")
        .append_pair("prompt", "consent")
        // XboxLive.signin scope 必需：没有它 MSA token 无权访问 Xbox Live
        .append_pair("scope", "openid profile offline_access XboxLive.signin")
        .append_pair("state", &state)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256");

    emit_progress(&app, 1, "正在打开系统浏览器…");
    app.opener()
        .open_url(auth_url.as_str(), None::<&str>)
        .map_err(|e| format!("打开浏览器失败: {e}"))?;

    emit_progress(&app, 2, "等待微软授权回调（最多 5 分钟）…");

    // 4) 等浏览器回调（deeplink_handler 分发到上面注册的 handler）
    let code = match tokio::time::timeout(LOGIN_TIMEOUT, rx).await {
        Ok(Ok(Ok(code))) => code,
        Ok(Ok(Err(e))) => return Err(e),
        Ok(Err(_)) => return Err("登录已取消".into()),
        Err(_) => return Err("登录超时（5 分钟）".into()),
    };
    drop(sub); // 注销本次登录的 deeplink 回调

    // 5) code 换 MSA token
    emit_progress(&app, 3, "正在获取微软令牌…");
    let tokens = exchange_code(&code, &code_verifier, &client_id, &tenant_id).await?;

    // 6) MSA -> XBL -> XSTS -> Minecraft
    emit_progress(&app, 4, "正在认证 Xbox Live…");
    let (xbl_token, _) = xbl_authenticate(&tokens.access_token).await?;
    emit_progress(&app, 5, "正在获取 XSTS 授权…");
    let (xsts_token, uhs) = xsts_authorize(&xbl_token).await?;
    emit_progress(&app, 6, "正在登录 Minecraft 服务…");
    let mc_token = login_with_xbox(&uhs, &xsts_token).await?;
    emit_progress(&app, 7, "正在获取玩家资料…");
    let profile = fetch_minecraft_profile(&mc_token).await?;

    // 7) 令牌存 keyring（msa_id = XBL uhs，即 xuid，账号唯一标识）
    emit_progress(&app, 8, "正在保存账号信息…");
    store_refresh_token(&uhs, tokens.refresh_token.as_deref())?;
    store_access_token(&uhs, &mc_token)?;

    // 8) 写库（同 uuid 视为同一账号，只更新）
    let now = unix_now()?;
    //    显式作用域：std MutexGuard 必须在 await 之前释放（NLL 有时对
    //    drop(conn) 的写法仍报 "future cannot be sent"，用块作用域最稳）。
    let (id, created_at): (i64, i64) = {
        let conn = db.lock()?;
        conn.execute(
            "INSERT INTO accounts(uuid, name, account_type, msa_id, last_used)
             VALUES(?1, ?2, 0, ?3, ?4)
             ON CONFLICT(uuid) DO UPDATE SET
                 name = excluded.name,
                 msa_id = excluded.msa_id,
                 last_used = excluded.last_used",
            rusqlite::params![profile.id, profile.name, uhs, now],
        )
        .map_err(|e| e.to_string())?;

        conn.query_row(
            "SELECT id, created_at FROM accounts WHERE uuid = ?1",
            rusqlite::params![profile.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?
    }; // conn（MutexGuard）在这里随块结束而释放，之后才允许 await

    // 9) 皮肤缓存（best effort：失败只打 stderr，绝不影响登录成功）
    if let Some((skin_url, slim)) = profile.active_skin() {
        skin_manager::cache_microsoft_skin(&app, &profile.id, skin_url, slim).await;
    }

    Ok(AccountRow {
        id,
        uuid: profile.id,
        name: profile.name,
        account_type: 0, // 0 = microsoft
        msa_id: Some(uhs),
        last_used: Some(now),
        created_at,
    })
}

/// 授权 code 换 MSA token（access_token + refresh_token）
async fn exchange_code(
    code: &str,
    verifier: &str,
    client_id: &str,
    tenant_id: &str,
) -> Result<MsaTokens, String> {
    let resp = http_client()
        .post(format!(
            "https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token"
        ))
        .form(&[
            ("client_id", client_id),
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|e| format!("请求微软 token 接口失败: {e}"))?;

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 token 响应失败: {e}"))?;
    if let Some(desc) = json.get("error_description").and_then(|v| v.as_str()) {
        return Err(format!("微软登录失败: {desc}"));
    }
    let access_token = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("token 响应缺少 access_token")?
        .to_string();
    let refresh_token = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok(MsaTokens {
        access_token,
        refresh_token,
    })
}

/// MSA access_token 换 XBL user token
async fn xbl_authenticate(msa_token: &str) -> Result<(String, String), String> {
    // user.auth.xboxlive.com 的固定请求体：顶层字段必须是
    // RelyingParty + TokenType，令牌放在 Properties.RpsTicket 且带 "d=" 前缀。
    // （此前顶层的 "RpsTicket"/"SandboxId"/"TitleId" 不是该端点的字段，
    //   端点会直接拒绝并返回非 JSON 内容，报 "error decoding response body"。）
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={msa_token}")
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });
    let (status, json) =
        post_json("https://user.auth.xboxlive.com/user/authenticate", &[], &body).await?;
    if !status.is_success() {
        return Err(format!("XBL 认证失败 ({status}): {}", describe_json(&json, 400)));
    }
    let token = json
        .get("Token")
        .and_then(|v| v.as_str())
        .ok_or("XBL 响应缺少 Token")?
        .to_string();
    let uhs = json
        .pointer("/DisplayClaims/xui/0/uhs")
        .and_then(|v| v.as_str())
        .ok_or("XBL 响应缺少 uhs")?
        .to_string();
    Ok((token, uhs))
}

/// XBL token 换 XSTS token（换取最终的 XBL3.0 凭据）
async fn xsts_authorize(xbl_token: &str) -> Result<(String, String), String> {
    // xsts.auth.xboxlive.com 的固定请求体：顶层 RelyingParty 指向
    // rp://api.minecraftservices.com/（Minecraft 专属 RP；Mojang API 只接受
    // 为此 RP 签发的 XSTS token——用 http://xboxlive.com 会在第 6 步被拒
    // 401 UNAUTHORIZED），XBL token 放在 Properties.UserTokens。
    let body = serde_json::json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbl_token]
        },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });
    let (status, json) =
        post_json("https://xsts.auth.xboxlive.com/xsts/authorize", &[], &body).await?;
    if let Some(xerr) = json.get("XErr").and_then(|v| v.as_i64()) {
        return Err(match xerr {
            2148916233 => "该微软账号没有 Xbox 账号，请先注册 Xbox 账号".into(),
            2148916234 => "该账号需要成人验证".into(),
            2148916235 => "该账号所在地区不提供 Xbox 服务".into(),
            2148916236 | 2148916237 => "该账号需要年龄验证".into(),
            2148916238 => "该账号是儿童账号，需加入家庭组才能登录".into(),
            other => format!("XSTS 授权失败 (XErr: {other})"),
        });
    }
    if !status.is_success() {
        return Err(format!("XSTS 授权失败 ({status}): {}", describe_json(&json, 400)));
    }
    let token = json
        .get("Token")
        .and_then(|v| v.as_str())
        .ok_or("XSTS 响应缺少 Token")?
        .to_string();
    let uhs = json
        .pointer("/DisplayClaims/xui/0/uhs")
        .and_then(|v| v.as_str())
        .ok_or("XSTS 响应缺少 uhs")?
        .to_string();
    Ok((token, uhs))
}

/// XSTS token 换 Minecraft access_token
///
/// 2026 年起官方启动器改用 POST /launcher/login（xtoken = XBL3.0 凭据，
/// platform = PC_LAUNCHER）：旧端点 /authentication/login_with_xbox 对新账户
/// 会拒绝（401 UNAUTHORIZED），其签发的 token 也可能不带玩家资料（进服务器
/// 报 "Profile not found"）。因此优先新端点，失败时回退旧端点（旧账户仍可用）。
async fn login_with_xbox(uhs: &str, xsts_token: &str) -> Result<String, String> {
    let xbl_credential = format!("XBL3.0 x={uhs};{xsts_token}");

    // 新端点：launcher/login，成功响应含 access_token / expires_in
    let body = serde_json::json!({
        "xtoken": xbl_credential,
        "platform": "PC_LAUNCHER"
    });
    match post_json("https://api.minecraftservices.com/launcher/login", &[], &body).await {
        Ok((status, json)) => {
            if status.is_success() {
                if let Some(token) = json.get("access_token").and_then(|v| v.as_str()) {
                    return Ok(token.to_string());
                }
            }
            eprintln!(
                "[ms-auth] launcher/login 未成功: {status} {}",
                describe_json(&json, 300)
            );
        }
        Err(err) => eprintln!("[ms-auth] launcher/login 请求失败: {err}"),
    }

    // 回退旧端点：identityToken = XBL3.0 凭据
    let body = serde_json::json!({
        "identityToken": xbl_credential
    });
    let (status, json) = post_json(
        "https://api.minecraftservices.com/authentication/login_with_xbox",
        &[],
        &body,
    )
    .await?;
    if !status.is_success() {
        return Err(format!("Minecraft 登录失败 ({status}): {}", describe_json(&json, 400)));
    }
    if let Some(desc) = json.get("error").and_then(|v| v.as_str()) {
        return Err(format!("Minecraft 登录失败: {desc}"));
    }
    json.get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or("Minecraft 登录响应缺少 access_token".into())
}

/// 用 Minecraft token 拉取玩家资料。
/// Ok(Ok(profile)) = 2xx 且解析成功；Ok(Err(msg)) = 接口返回非 2xx（错误串含
/// "(401" 这类状态码，调用方据此识别 token 失效）；Err = 网络层错误。
async fn fetch_minecraft_profile_raw(mc_token: &str) -> Result<Result<McProfile, String>, String> {
    let resp = http_client()
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(mc_token)
        .send()
        .await
        .map_err(|e| format!("获取 Minecraft 资料失败: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Ok(Err(format!("获取 Minecraft 资料失败 ({status}): {text}")));
    }
    serde_json::from_str(&text)
        .map(Ok)
        .map_err(|e| format!("解析 Minecraft 资料失败: {e}"))
}

/// 便捷封装：成功返回 profile，非 2xx / 网络错误都返回 Err
async fn fetch_minecraft_profile(mc_token: &str) -> Result<McProfile, String> {
    fetch_minecraft_profile_raw(mc_token).await?
}

/// 判断 profile 接口的非 2xx 错误是否意味着 token 失效（需要刷新）
fn is_token_error(err: &str) -> bool {
    err.contains("(401") || err.contains("(403")
}

// ── keyring 存取 ─────────────────────────────────────────────────────────

/// 存 refresh token，key = "ms_refresh:<msa_id>"（见 db_handler.rs 顶部注释）
fn store_refresh_token(msa_id: &str, token: Option<&str>) -> Result<(), String> {
    let Some(token) = token else {
        return Ok(()); // 本次没拿到 refresh_token（scope 没勾 offline_access），跳过
    };
    let entry = keyring::Entry::new("gsml", &format!("ms_refresh:{msa_id}"))
        .map_err(|e| format!("keyring: {e}"))?;
    entry.set_password(token).map_err(|e| format!("keyring: {e}"))
}

/// 存 Minecraft access_token，key = "ms_access:<msa_id>"，之后启动游戏时用
fn store_access_token(msa_id: &str, token: &str) -> Result<(), String> {
    let entry = keyring::Entry::new("gsml", &format!("ms_access:{msa_id}"))
        .map_err(|e| format!("keyring: {e}"))?;
    entry.set_password(token).map_err(|e| format!("keyring: {e}"))
}

/// 读 keyring 里的字符串（不存在时返回 None）
fn keyring_get(key: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new("gsml", key).map_err(|e| format!("keyring: {e}"))?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keyring: {e}")),
    }
}

/// 按 account_id 查微软账号的 msa_id（uuid, msa_id）
fn lookup_msa_id(
    conn: &rusqlite::Connection,
    account_id: i64,
) -> Result<(String, String), String> {
    let (uuid, msa_id): (String, Option<String>) = conn
        .query_row(
            "SELECT uuid, msa_id FROM accounts WHERE id = ?1 AND account_type = 0",
            rusqlite::params![account_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .map_err(|_| format!("微软账号不存在 (id={account_id})"))?;
    let msa_id = msa_id.ok_or_else(|| format!("账号缺少微软标识 (id={account_id})"))?;
    Ok((uuid, msa_id))
}

/// 用 MSA refresh_token 走一遍刷新链：MSA → XBL → XSTS → Minecraft token。
/// 返回 (可能轮换的 refresh_token, 新的 Minecraft token)。
async fn refresh_minecraft_token(refresh_token: &str) -> Result<(Option<String>, String), String> {
    // 1) MSA refresh_token 换新的 access_token（refresh_token 可能轮换）
    let resp = http_client()
        .post(format!(
            "https://login.microsoftonline.com/{AZURE_TENANT}/oauth2/v2.0/token"
        ))
        .form(&[
            ("client_id", AZURE_CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|e| format!("请求微软 token 接口失败: {e}"))?;
    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("解析 token 响应失败: {e}"))?;
    if let Some(desc) = json.get("error_description").and_then(|v| v.as_str()) {
        return Err(format!("微软登录失败: {desc}"));
    }
    let msa_access = json
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or("token 响应缺少 access_token")?
        .to_string();
    let new_refresh = json
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // 2) MSA access_token → XBL token（顺带拿 uhs）
    let (xbl_token, _xbl_uhs) = xbl_authenticate(&msa_access).await?;
    // 3) XBL token → XSTS token（拿到 uhs）
    let (xsts_token, uhs) = xsts_authorize(&xbl_token).await?;
    // 4) XSTS → Minecraft token（launcher/login 已在 login_with_xbox 内部处理）
    let mc_token = login_with_xbox(&uhs, &xsts_token).await?;
    Ok((new_refresh, mc_token))
}

/// 确保拿到可用的 Minecraft token：
/// - keyring 里的 ms_access 有效 → 返回 (token, Some(profile))（profile 顺带取回，省一次请求）
/// - token 失效/不存在 → 用 ms_refresh 刷新，重存两把 key，返回 (token, None)（调用方需再拉 profile）
async fn ensure_mc_token(msa_id: &str) -> Result<(String, Option<McProfile>), String> {
    let cached = keyring_get(&format!("ms_access:{msa_id}"))?;
    if let Some(token) = cached {
        match fetch_minecraft_profile_raw(&token).await? {
            Ok(profile) => return Ok((token, Some(profile))),
            // 401/403 → token 失效，走刷新；其他错误（网络/解析）直接报
            Err(msg) if is_token_error(&msg) => {}
            Err(msg) => return Err(msg),
        }
    }

    // 走刷新链
    let refresh = keyring_get(&format!("ms_refresh:{msa_id}"))?
        .ok_or("缺少微软刷新凭据，请重新登录微软账号")?;
    let (new_refresh, mc_token) = refresh_minecraft_token(&refresh).await?;
    // 刷新后的凭据回存（refresh token 可能轮换，必须更新）
    store_refresh_token(msa_id, new_refresh.as_deref())?;
    store_access_token(msa_id, &mc_token)?;
    Ok((mc_token, None))
}

/// 把 McProfile 转成给前端的 JSON（uuid/name/skins/capes）
fn profile_to_json(profile: &McProfile, uuid: &str) -> serde_json::Value {
    serde_json::json!({
        "uuid": profile.id,
        "name": profile.name,
        "skins": profile.skins.iter().map(|s| serde_json::json!({
            "state": s.state,
            "url": s.url,
            "variant": s.variant,
        })).collect::<Vec<_>>(),
        "capes": profile.capes.iter().map(|c| serde_json::json!({
            "id": c.id,
            "state": c.state,
            "alias": c.alias,
            "url": c.url,
        })).collect::<Vec<_>>(),
        "dbUuid": uuid,
    })
}

/// 拿到账号的 Minecraft token 与最新 profile（带 401 自动刷新），
/// 是 ms_account_details / ms_change_skin / ms_change_cape 的公共入口。
async fn account_profile(
    db: &State<'_, Db>,
    account_id: i64,
) -> Result<McProfile, String> {
    let (_uuid, msa_id) = {
        let conn = db.lock()?;
        lookup_msa_id(&conn, account_id)?
    };
    let (mc_token, profile) = ensure_mc_token(&msa_id).await?;
    match profile {
        Some(p) => Ok(p),
        // 刷新后的 token 是新的，直接再拉一次 profile
        None => fetch_minecraft_profile(&mc_token).await,
    }
}

/// JS 调用：直接 GET /minecraft/profile（自动处理 token 刷新），
/// 返回 { uuid, name, skins[], capes[], dbUuid }
#[tauri::command]
pub async fn ms_account_details(
    db: State<'_, Db>,
    account_id: i64,
) -> Result<serde_json::Value, String> {
    let uuid = {
        let conn = db.lock()?;
        lookup_msa_id(&conn, account_id)?.0
    };
    let profile = account_profile(&db, account_id).await?;
    Ok(profile_to_json(&profile, &uuid))
}

/// JS 调用：更换皮肤（上传 64×64 PNG，base64 编码）。
/// variant = "slim"（Alex 3px 臂）或 "classic"（Steve 4px 臂）。
/// 成功后返回最新 profile JSON（同 ms_account_details）。
#[tauri::command]
pub async fn ms_change_skin(
    app: AppHandle,
    db: State<'_, Db>,
    account_id: i64,
    skin_base64: String,
    variant: String,
) -> Result<serde_json::Value, String> {
    // 1) base64 解码（前端用标准带 padding 的 base64）
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(skin_base64.trim())
        .map_err(|e| format!("皮肤图片 base64 解码失败: {e}"))?;

    // 2) 校验是 64×64 PNG（Mojang 只接受这个规格）
    {
        let img = image::load_from_memory(&bytes)
            .map_err(|e| format!("皮肤图片不是有效的 PNG: {e}"))?;
        let (w, h) = (img.width(), img.height());
        if w != 64 || h != 64 {
            return Err(format!("皮肤图片必须是 64×64 像素，当前为 {w}×{h}"));
        }
    }

    // 3) 确保可用 token（401 自动刷新）
    let uuid = {
        let conn = db.lock()?;
        lookup_msa_id(&conn, account_id)?.0
    };
    let (mc_token, profile) = {
        let (_u, msa_id) = {
            let conn = db.lock()?;
            lookup_msa_id(&conn, account_id)?
        };
        ensure_mc_token(&msa_id).await?
    };
    // token 是新刷新的 → 先拉一次 profile 校验（结果不使用，下面上传后重拉）
    if profile.is_none() {
        fetch_minecraft_profile(&mc_token).await?;
    }

    let variant_normalized = if variant.eq_ignore_ascii_case("slim") {
        "slim"
    } else {
        "classic"
    };

    // 4) multipart 上传皮肤
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("skin.png")
        .mime_str("image/png")
        .map_err(|e| format!("构造皮肤上传请求失败: {e}"))?;
    let form = reqwest::multipart::Form::new()
        .text("variant", variant_normalized)
        .part("file", part);
    let resp = http_client()
        .post("https://api.minecraftservices.com/minecraft/profile/skins")
        .bearer_auth(&mc_token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("上传皮肤失败: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("上传皮肤失败 ({status}): {text}"));
    }
    // 接口 2xx 时仍可能带 error 字段
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(desc) = json.get("error").and_then(|v| v.as_str()) {
            return Err(format!("上传皮肤失败: {desc}"));
        }
    }

    // 5) 成功后重新拉 profile（拿到新的 ACTIVE 皮肤地址）并刷新皮肤缓存
    let profile = fetch_minecraft_profile(&mc_token).await?;
    if let Some((skin_url, slim)) = profile.active_skin() {
        skin_manager::cache_microsoft_skin(&app, &profile.id, skin_url, slim).await;
    }
    Ok(profile_to_json(&profile, &uuid))
}

/// JS 调用：装备 / 卸下披风。cape_id = Some(id) 装备，None 卸下当前披风。
/// 返回最新 capes 数组。
#[tauri::command]
pub async fn ms_change_cape(
    db: State<'_, Db>,
    account_id: i64,
    cape_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let uuid = {
        let conn = db.lock()?;
        lookup_msa_id(&conn, account_id)?.0
    };
    let (mc_token, profile) = {
        let (_u, msa_id) = {
            let conn = db.lock()?;
            lookup_msa_id(&conn, account_id)?
        };
        ensure_mc_token(&msa_id).await?
    };
    // token 是新刷新的 → 先拉一次 profile 校验（结果不使用，下面换披风后重拉）
    if profile.is_none() {
        fetch_minecraft_profile(&mc_token).await?;
    }

    // 装备 / 卸下披风，按官方文档分流：
    //   装备：PUT /minecraft/profile/capes/active，body {"capeId": uuid}，返回完整 profile；
    //   卸下：DELETE /minecraft/profile/capes/active，无 body，成功返回空负载。
    let (status, json) = match &cape_id {
        Some(id) => send_json(
            reqwest::Method::PUT,
            "https://api.minecraftservices.com/minecraft/profile/capes/active",
            &[("Authorization", &format!("Bearer {mc_token}"))],
            Some(&serde_json::json!({ "capeId": id })),
        )
        .await?,
        None => send_json(
            reqwest::Method::DELETE,
            "https://api.minecraftservices.com/minecraft/profile/capes/active",
            &[("Authorization", &format!("Bearer {mc_token}"))],
            None,
        )
        .await?,
    };
    if !status.is_success() {
        return Err(format!(
            "切换披风失败 ({status}): {}",
            describe_json(&json, 400)
        ));
    }
    if let Some(desc) = json.get("error").and_then(|v| v.as_str()) {
        return Err(format!("切换披风失败: {desc}"));
    }

    // 成功后重拉 profile 拿最新 capes
    let profile = fetch_minecraft_profile(&mc_token).await?;
    Ok(profile_to_json(&profile, &uuid))
}

// ── 删除账号 ─────────────────────────────────────────────────────────────

/// 删除账号：accounts 行 + keyring 凭据（微软账号）+ 皮肤缓存目录。
/// DB 删除成功即视为成功；keyring / 皮肤目录清理失败只打 stderr（best effort）。
#[tauri::command]
pub fn delete_account(app: AppHandle, db: State<'_, Db>, account_id: i64) -> Result<(), String> {
    let (uuid, msa_id, account_type) = {
        let conn = db.lock()?;
        let row = conn
            .query_row(
                "SELECT uuid, msa_id, account_type FROM accounts WHERE id = ?1",
                rusqlite::params![account_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(|_| format!("账号不存在 (id={account_id})"))?;
        conn.execute(
            "DELETE FROM accounts WHERE id = ?1",
            rusqlite::params![account_id],
        )
        .map_err(|e| format!("删除账号失败: {e}"))?;
        row
    };

    // 微软账号：清掉 keyring 里的两把凭据（离线账号没有 msa_id，跳过）
    if let Some(msa_id) = &msa_id {
        for key in [format!("ms_refresh:{msa_id}"), format!("ms_access:{msa_id}")] {
            match keyring::Entry::new("gsml", &key) {
                Ok(entry) => {
                    if let Err(err) = entry.delete_credential() {
                        eprintln!("[account] 删除 keyring {key} 失败: {err}");
                    }
                }
                Err(err) => eprintln!("[account] 打开 keyring {key} 失败: {err}"),
            }
        }
    }

    // 对应类型的皮肤缓存目录
    let kind = if account_type == 0 { "microsoft" } else { "offline" };
    match app.path().app_local_data_dir() {
        Ok(root) => {
            let dir = root.join("skin").join(kind).join(&uuid);
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => eprintln!("[account] 删除皮肤缓存 {} 失败: {err}", dir.display()),
            }
        }
        Err(err) => eprintln!("[account] 获取应用数据目录失败: {err}"),
    }

    Ok(())
}
