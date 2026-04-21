#![allow(unused_imports)]
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_PLAYER_NAME_LENGTH: usize = 16;

#[tauri::command]
fn get_env_var(name: String) -> Result<String, String> {
    std::env::var(&name).map_err(|e| format!("Env var '{}' not found: {}", name, e))
}

#[tauri::command]
fn list_directories(path: String) -> Result<Vec<String>, String> {
    let mut dirs = Vec::new();
    let entries = fs::read_dir(&path).map_err(|e| format!("无法读取目录 '{}': {}", path, e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("无法读取条目: {}", e))?;
        if entry
            .metadata()
            .map_err(|e| format!("无法读取元数据: {}", e))?
            .is_dir()
        {
            if let Some(dir_name) = entry.file_name().to_str() {
                dirs.push(dir_name.to_string());
            }
        }
    }

    dirs.sort();
    Ok(dirs)
}

#[tauri::command]
fn list_minecraft_versions(mc_path: String) -> Result<Vec<String>, String> {
    let versions_dir = Path::new(&mc_path).join("versions");
    let entries = fs::read_dir(&versions_dir).map_err(|e| {
        format!(
            "Unable to read versions folder '{}': {}",
            versions_dir.display(),
            e
        )
    })?;

    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Unable to read versions entry: {}", e))?;
        if !entry
            .metadata()
            .map_err(|e| format!("Unable to read versions metadata: {}", e))?
            .is_dir()
        {
            continue;
        }

        let version = entry.file_name().to_string_lossy().to_string();
        if entry.path().join(format!("{}.json", &version)).exists() {
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
        let candidates = if cfg!(target_os = "windows") {
            vec![
                Path::new(&java_home).join("bin").join("java.exe"),
                Path::new(&java_home).join("bin").join("javaw.exe"),
            ]
        } else {
            vec![Path::new(&java_home).join("bin").join("java")]
        };

        for binary in candidates {
            if binary.exists() {
                return binary.to_string_lossy().to_string();
            }
        }
    }

    "java".to_string()
}

fn current_os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn feature_enabled(_feature: &str) -> bool {
    false
}

fn rule_matches(rule: &Value) -> bool {
    let os_name = rule
        .get("os")
        .and_then(|os| os.get("name"))
        .and_then(|name| name.as_str());

    let os_matches = match os_name {
        Some(name) => name == current_os_name(),
        None => true,
    };

    let features_match = rule
        .get("features")
        .and_then(|features| features.as_object())
        .map(|features| {
            features.iter().all(|(feature, expected)| {
                expected
                    .as_bool()
                    .map(|expected_value| feature_enabled(feature) == expected_value)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(true);

    os_matches && features_match
}

fn rules_allow(argument: &Value) -> bool {
    let Some(rules) = argument.get("rules").and_then(|v| v.as_array()) else {
        return true;
    };

    let mut allow = false;
    for rule in rules {
        if !rule_matches(rule) {
            continue;
        }

        let action = rule
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("disallow");
        allow = action == "allow";
    }

    allow
}

fn extract_argument_values(argument: &Value) -> Vec<String> {
    if !rules_allow(argument) {
        return vec![];
    }

    if let Some(text) = argument.as_str() {
        return vec![text.to_string()];
    }

    if let Some(values) = argument.as_array() {
        let mut out = Vec::new();
        for value in values {
            out.extend(extract_argument_values(value));
        }
        return out;
    }

    if let Some(value) = argument.get("value") {
        return extract_argument_values(value);
    }

    vec![]
}

fn is_unsupported_jvm_argument(argument: &str) -> bool {
    argument.starts_with("--sun-misc-unsafe-memory-access=")
}

fn current_arch() -> &'static str {
    if cfg!(target_pointer_width = "64") {
        "64"
    } else {
        "32"
    }
}

fn extract_natives(
    version_json: &Value,
    libraries_dir: &Path,
    natives_dir: &Path,
) -> Result<(), String> {
    let os = current_os_name();
    let arch = current_arch();

    let Some(libraries) = version_json.get("libraries").and_then(|v| v.as_array()) else {
        return Ok(());
    };

    for library in libraries {
        if !rules_allow(library) {
            continue;
        }

        let Some(natives_map) = library.get("natives").and_then(|v| v.as_object()) else {
            continue;
        };

        let Some(classifier_template) = natives_map.get(os).and_then(|v| v.as_str()) else {
            continue;
        };

        let classifier = classifier_template.replace("${arch}", arch);

        let Some(jar_path) = library
            .get("downloads")
            .and_then(|d| d.get("classifiers"))
            .and_then(|c| c.get(&classifier))
            .and_then(|e| e.get("path"))
            .and_then(|p| p.as_str())
        else {
            continue;
        };

        let jar_file = libraries_dir.join(jar_path);
        if !jar_file.exists() {
            continue;
        }

        let exclude_prefixes: Vec<String> = library
            .get("extract")
            .and_then(|e| e.get("exclude"))
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let file = fs::File::open(&jar_file)
            .map_err(|e| format!("Unable to open native jar '{}': {}", jar_file.display(), e))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Unable to read native jar '{}': {}", jar_file.display(), e))?;

        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| format!("Unable to read zip entry: {}", e))?;
            let entry_name = entry.name().to_string();

            if exclude_prefixes
                .iter()
                .any(|prefix| entry_name.starts_with(prefix.as_str()))
            {
                continue;
            }

            if entry.is_dir() {
                continue;
            }

            let file_name = match Path::new(&entry_name).file_name() {
                Some(n) => n.to_string_lossy().to_string(),
                None => continue,
            };

            if file_name.is_empty() || file_name == ".." {
                continue;
            }

            let dest = natives_dir.join(&file_name);
            let mut out = fs::File::create(&dest)
                .map_err(|e| format!("Unable to create native file '{}': {}", dest.display(), e))?;
            io::copy(&mut entry, &mut out).map_err(|e| {
                format!("Unable to extract native file '{}': {}", dest.display(), e)
            })?;
        }
    }

    Ok(())
}

fn replace_launch_tokens(
    text: &str,
    player_name: &str,
    version: &str,
    game_dir: &Path,
    assets_dir: &Path,
    assets_index_name: &str,
    version_type: &str,
    classpath: &str,
    classpath_separator: &str,
    natives_dir: &Path,
) -> String {
    text.replace("${auth_player_name}", player_name)
        .replace("${version_name}", version)
        .replace("${game_directory}", &game_dir.to_string_lossy())
        .replace("${assets_root}", &assets_dir.to_string_lossy())
        .replace("${assets_index_name}", assets_index_name)
        .replace("${auth_uuid}", "00000000-0000-0000-0000-000000000000")
        .replace("${auth_access_token}", "0")
        .replace("${user_type}", "legacy")
        .replace("${version_type}", version_type)
        .replace("${launcher_name}", "gsml")
        .replace("${launcher_version}", "1.0.0")
        .replace("${user_properties}", "{}")
        .replace("${user_properties_map}", "{}")
        .replace("${auth_xuid}", "0")
        .replace("${clientid}", "0")
        .replace("${quickPlayPath}", "")
        .replace("${quickPlaySingleplayer}", "")
        .replace("${quickPlayMultiplayer}", "")
        .replace("${quickPlayRealms}", "")
        .replace("${classpath}", classpath)
        .replace("${classpath_separator}", classpath_separator)
        .replace("${natives_directory}", &natives_dir.to_string_lossy())
}

#[tauri::command]
fn start_minecraft(
    mc_path: String,
    version: String,
    player_name: Option<String>,
) -> Result<String, String> {
    let version = sanitize_version(version.trim())?;
    let game_dir = Path::new(&mc_path);
    let version_dir = game_dir.join("versions").join(version);
    let version_json_path = version_dir.join(format!("{}.json", version));
    let client_jar = version_dir.join(format!("{}.jar", version));
    let assets_dir = game_dir.join("assets");
    let libraries_dir = game_dir.join("libraries");
    let natives_dir = version_dir.join("natives");
    let classpath_separator = if cfg!(target_os = "windows") {
        ";"
    } else {
        ":"
    };

    if !version_json_path.exists() {
        return Err(format!(
            "Minecraft version metadata not found: {}",
            version_json_path.display()
        ));
    }

    if !client_jar.exists() {
        return Err(format!("Minecraft jar not found: {}", client_jar.display()));
    }

    let player = player_name
        .unwrap_or_else(|| "Player".to_string())
        .trim()
        .chars()
        .take(MAX_PLAYER_NAME_LENGTH)
        .collect::<String>();
    let player = if player.is_empty() {
        "Player".to_string()
    } else {
        player
    };

    let version_json_raw = fs::read_to_string(&version_json_path).map_err(|e| {
        format!(
            "Unable to read version json '{}': {}",
            version_json_path.display(),
            e
        )
    })?;
    let version_json: Value = serde_json::from_str(&version_json_raw).map_err(|e| {
        format!(
            "Unable to parse version json '{}': {}",
            version_json_path.display(),
            e
        )
    })?;

    let main_class = version_json
        .get("mainClass")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "mainClass missing in version metadata".to_string())?;

    let assets_index_name = version_json
        .get("assets")
        .and_then(|v| v.as_str())
        .unwrap_or(version);
    let version_type = version_json
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("release");

    let mut classpath_entries = vec![client_jar.to_string_lossy().to_string()];
    if let Some(libraries) = version_json.get("libraries").and_then(|v| v.as_array()) {
        for library in libraries {
            if !rules_allow(library) {
                continue;
            }
            if let Some(path) = library
                .get("downloads")
                .and_then(|d| d.get("artifact"))
                .and_then(|a| a.get("path"))
                .and_then(|p| p.as_str())
            {
                classpath_entries.push(libraries_dir.join(path).to_string_lossy().to_string());
            }
        }
    }

    let classpath = classpath_entries.join(classpath_separator);
    let _ = fs::create_dir_all(&natives_dir);
    extract_natives(&version_json, &libraries_dir, &natives_dir)?;

    let mut command_args = Vec::new();
    let mut has_classpath_arg = false;

    if let Some(jvm_args) = version_json
        .get("arguments")
        .and_then(|v| v.get("jvm"))
        .and_then(|v| v.as_array())
    {
        for arg in jvm_args {
            for value in extract_argument_values(arg) {
                let replaced = replace_launch_tokens(
                    &value,
                    &player,
                    version,
                    game_dir,
                    &assets_dir,
                    assets_index_name,
                    version_type,
                    &classpath,
                    classpath_separator,
                    &natives_dir,
                );
                if replaced == "-cp" || replaced == "--classpath" {
                    has_classpath_arg = true;
                }
                if is_unsupported_jvm_argument(&replaced) {
                    log::debug!("Skipping unsupported JVM argument: {}", replaced);
                    continue;
                }
                command_args.push(replaced);
            }
        }
    }

    if !has_classpath_arg {
        command_args.push("-Djava.library.path=".to_string() + &natives_dir.to_string_lossy());
        command_args.push("-cp".to_string());
        command_args.push(classpath.clone());
    }

    command_args.push(main_class.to_string());

    if let Some(game_args) = version_json
        .get("arguments")
        .and_then(|v| v.get("game"))
        .and_then(|v| v.as_array())
    {
        for arg in game_args {
            for value in extract_argument_values(arg) {
                command_args.push(replace_launch_tokens(
                    &value,
                    &player,
                    version,
                    game_dir,
                    &assets_dir,
                    assets_index_name,
                    version_type,
                    &classpath,
                    classpath_separator,
                    &natives_dir,
                ));
            }
        }
    } else if let Some(legacy_args) = version_json
        .get("minecraftArguments")
        .and_then(|v| v.as_str())
    {
        for arg in legacy_args.split_whitespace() {
            command_args.push(replace_launch_tokens(
                arg,
                &player,
                version,
                game_dir,
                &assets_dir,
                assets_index_name,
                version_type,
                &classpath,
                classpath_separator,
                &natives_dir,
            ));
        }
    }

    let child = Command::new(resolve_java_binary())
        .args(command_args)
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
