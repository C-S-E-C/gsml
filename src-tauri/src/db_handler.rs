// ms_refresh token 存于 keyring，key = "ms_refresh:<msa_id>"
// db 用法：先 init 注册为 State，command 里 `let conn = db.lock()?; conn.execute(...)?;`
// account type：0->microsoft, 1->offline
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use dirs;
use tauri::State;

/// 数据根目录：%LOCALAPPDATA%\Syntropica\GlowSquidMinecraftLauncher
/// 所有模块（db、皮肤缓存、披风缓存）共用这一个入口，不再各自拼路径。
pub fn data_root() -> Result<PathBuf, String> {
    Ok(dirs::data_local_dir()
        .ok_or("无法定位本地数据目录")?
        .join("Syntropica")
        .join("GlowSquidMinecraftLauncher"))
}

pub struct Db(pub Arc<Mutex<Connection>>);

impl Db {
    pub fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.0.lock().map_err(|e| format!("db lock poisoned: {e}"))
    }
}

pub fn init() -> Result<Db, String> {
    let dir = data_root()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let db_path = dir.join("app.db");

    let conn = Connection::open(&db_path).map_err(|e| format!("open db: {e}"))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 5000;

        CREATE TABLE IF NOT EXISTS accounts(
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid         TEXT    NOT NULL UNIQUE,
            name         TEXT    NOT NULL,
            account_type INTEGER NOT NULL,
            msa_id       TEXT    UNIQUE,
            last_used    INTEGER,
            created_at   INTEGER NOT NULL DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_accounts_last_used
            ON accounts(last_used DESC);
        
        CREATE TABLE IF NOT EXISTS simpleValues(
            key   TEXT PRIMARY KEY,
            -- value 存 JSON 文本，任意类型（数字/布尔/字符串/数组/对象）
            value TEXT NOT NULL
        );
        ",
    )
    .map_err(|e| format!("migrate: {e}"))?;

    Ok(Db(Arc::new(Mutex::new(conn))))
}

#[derive(serde::Serialize)]
pub struct AccountRow {
    pub id: u16,
    pub uuid: String,
    pub name: String,
    pub account_type: u8, // 0 = microsoft, 1 = offline（见文件头注释）
    pub msa_id: Option<String>,
    pub last_used: Option<u32>, //2106-02-07溢出！
    pub created_at: u32,
}

/// simpleValues 的 value 存 JSON 文本，可承载任意类型（数字/布尔/字符串/数组/对象）。
/// 读取时解析 JSON；历史遗留的非 JSON 字符串原样按字符串返回。
#[tauri::command]
pub fn get_svar(db: State<'_, Db>, key: &str) -> Result<Option<serde_json::Value>, String> {
    let conn = db.lock()?;
    match conn.query_row(
        "SELECT value FROM simpleValues WHERE key = ?1",
        rusqlite::params![key],
        |r| r.get::<_, String>(0),
    ) {
        Ok(text) => Ok(Some(
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)),
        )),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub fn set_svar(db: State<'_, Db>, key: &str, value: serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    let conn = db.lock()?;
    conn.execute(
        "INSERT INTO simpleValues(key, value) VALUES(?1, ?2)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, text],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}