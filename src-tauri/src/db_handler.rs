// ms_refresh token 存于 keyring，key = "ms_refresh:<msa_id>"
// db 用法：先 init 注册为 State，command 里 `let conn = db.lock()?; conn.execute(...)?;`
// account type：0->microsoft, 1->offline
use rusqlite::Connection;
use std::sync::{Arc, Mutex};
use tauri::Manager;

pub struct Db(pub Arc<Mutex<Connection>>);

impl Db {
    pub fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
        self.0.lock().map_err(|e| format!("db lock poisoned: {e}"))
    }
}

pub fn init(app: &tauri::AppHandle) -> Result<Db, String> {
    let dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
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
           ON accounts(last_used DESC);",
    )
    .map_err(|e| format!("migrate: {e}"))?;

    Ok(Db(Arc::new(Mutex::new(conn))))
}

#[derive(serde::Serialize)]
pub struct AccountRow {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub account_type: i64,
    pub msa_id: Option<String>,
    pub last_used: Option<i64>,
    pub created_at: i64,
}
