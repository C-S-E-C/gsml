// src-tauri/src/lib.rs
#![allow(unused_imports)]
use std::fs;
use std::io;
// 定义单个环境变量获取函数
#[tauri::command]
fn get_env_var(name: String) -> Result<String, String> {
    println!("[Rust] Requesting env var: {}", name);
    
    match std::env::var(&name) {
        Ok(value) => {
            println!("[Rust] Found env var: {} = {}", name, value);
            Ok(value)
        }
        Err(e) => {
            let error_msg = format!("Env var '{}' not found: {}", name, e);
            println!("[Rust] {}", error_msg);
            Err(error_msg)
        }
    }
}

#[tauri::command]
fn list_directories(path: String) -> Result<Vec<String>, String> {
    println!("列出目录: {}", path);
    
    let mut dirs = Vec::new();
    
    match fs::read_dir(&path) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        if let Ok(metadata) = entry.metadata() {
                            if metadata.is_dir() {
                                if let Some(dir_name) = entry.file_name().to_str() {
                                    dirs.push(dir_name.to_string());
                                }
                            }
                        }
                    }
                    Err(e) => {
                        return Err(format!("无法读取条目: {}", e));
                    }
                }
            }
            // 排序结果
            dirs.sort();
            Ok(dirs)
        }
        Err(e) => {
            Err(format!("无法读取目录 '{}': {}", path, e))
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_env_var,
            list_directories
        ])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}