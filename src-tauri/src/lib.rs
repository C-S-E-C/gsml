// src-tauri/src/lib.rs
#![allow(unused_imports)]
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;
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

#[tauri::command]
fn list_minecraft_versions(mc_path: String) -> Result<Vec<String>, String> {
    let versions_dir = Path::new(&mc_path).join("versions");
    let entries = fs::read_dir(&versions_dir)
        .map_err(|e| format!("Unable to read versions folder '{}': {}", versions_dir.display(), e))?;

    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Unable to read versions entry: {}", e))?;
        let metadata = entry
            .metadata()
            .map_err(|e| format!("Unable to read versions metadata: {}", e))?;

        if !metadata.is_dir() {
            continue;
        }

        let version = entry.file_name().to_string_lossy().to_string();
        let version_json = entry.path().join(format!("{}.json", &version));
        if version_json.exists() {
            versions.push(version);
        }
    }

    versions.sort();
    versions.reverse();
    Ok(versions)
}

fn sanitize_version(version: &str) -> Result<&str, String> {
    if version.is_empty() {
        return Err("Version cannot be empty".to_string());
    }
    if version.contains("..") || version.contains('/') || version.contains('\\') {
        return Err("Invalid version value".to_string());
    }
    Ok(version)
}

fn resolve_java_binary() -> String {
    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        let binary = if cfg!(target_os = "windows") {
            Path::new(&java_home).join("bin").join("javaw.exe")
        } else {
            Path::new(&java_home).join("bin").join("java")
        };

        if binary.exists() {
            return binary.to_string_lossy().to_string();
        }
    }

    if cfg!(target_os = "windows") {
        "javaw".to_string()
    } else {
        "java".to_string()
    }
}

#[tauri::command]
fn start_minecraft(mc_path: String, version: String, player_name: Option<String>) -> Result<String, String> {
    let version = sanitize_version(version.trim())?;
    let game_dir = Path::new(&mc_path);
    let jar_path = game_dir
        .join("versions")
        .join(version)
        .join(format!("{}.jar", version));

    if !jar_path.exists() {
        return Err(format!("Minecraft jar not found: {}", jar_path.display()));
    }

    let player = player_name
        .unwrap_or_else(|| "Player".to_string())
        .trim()
        .chars()
        .take(16)
        .collect::<String>();

    let player = if player.is_empty() {
        "Player".to_string()
    } else {
        player
    };

    let child = Command::new(resolve_java_binary())
        .arg("-jar")
        .arg(&jar_path)
        .arg("--username")
        .arg(player)
        .current_dir(game_dir)
        .spawn()
        .map_err(|e| format!("Unable to start Minecraft process: {}", e))?;

    Ok(format!("Minecraft started (PID: {})", child.id()))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_env_var,
            list_directories,
            list_minecraft_versions,
            start_minecraft
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
