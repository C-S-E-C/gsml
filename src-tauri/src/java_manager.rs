//! java_manager —— Java 运行时探测 / 扫描 / 校验（设置页「Java」分类后端）。
//!
//! 命令：
//!   list_java_installations() -> Vec<JavaRuntime>   扫描本机常见 Java 目录
//!   check_java_path(path)     -> JavaRuntime        校验自定义路径并取版本
//!
//! 持久化的「首选 Java 路径」不用专门命令：前端直接走现有
//! get_svar / set_svar（key = "java-path"）。launch_game / install_version
//! 收到 javaPath 后仍按原 find_java 规则解析（显式 > JAVA_HOME > PATH）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// 一个可用的 Java 运行时。
#[derive(Debug, Clone, Serialize)]
pub struct JavaRuntime {
    /// java 可执行文件绝对路径
    pub path: String,
    /// Java 主目录（可执行文件上两级； macOS 为 Contents/Home）
    pub home: String,
    /// 解析出的版本号（如 "17.0.2"；解析失败为空串）
    pub version: String,
    /// 粗略厂商/发行名（从目录名与 --version 输出推断，未知为 "Java"）
    pub vendor: String,
    /// 类型：JDK（java 同目录下有 javac）或 JRE
    pub kind: String,
    /// 来源说明：JAVA_HOME / PATH / 目录扫描 / 自定义
    pub source: String,
    /// 是否是启动器当前默认会用到的那一个（与 find_java(None) 一致）
    pub is_default: bool,
}

// ── 探测单个 java 可执行文件 ─────────────────────────────────────────────

/// 运行 `<exe> --version` 查版本号（Java 9+ 输出走 stdout，如
/// `openjdk 21.0.4 2024-07-16`）；不支持 --version 的老 Java 8 回退
/// `-version`（输出固定走 stderr，如 `openjdk version "1.8.0_391" ...`）。
/// 返回 (版本号, 输出首行)；首行同时用于推断发行名。
fn probe_java_line(exe: &Path) -> Option<(String, String)> {
    // Java 9+：--version 输出走 stdout。
    // 隐藏控制台窗口：否则每次探测都会闪一下 cmd（GUI 进程 spawn 控制台程序的默认行为）
    let mut probe = Command::new(exe);
    probe.arg("--version");
    crate::proc_util::hide_console(&mut probe);
    if let Ok(out) = probe.output() {
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let first = stdout.lines().next().unwrap_or("").trim().to_string();
            let version = version_token(&first);
            if !version.is_empty() {
                return Some((version, first));
            }
        }
    }
    // 兜底：-version 输出走 stderr。
    let mut probe = Command::new(exe);
    probe.arg("-version");
    crate::proc_util::hide_console(&mut probe);
    let out = probe.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let first = stderr.lines().next().unwrap_or("").trim().to_string();
    // 版本号：取 "..." 里那段（同时兼容 "1.8.0_391" 这类老式版本）。
    let version = first
        .split('"')
        .nth(1)
        .unwrap_or("")
        .trim()
        .to_string();
    if version.is_empty() {
        return None;
    }
    Some((version, first))
}

/// 从 --version 输出首行取版本号：优先取 "..." 引号段，否则取第一个数字
/// 开头的空白分隔 token（`openjdk 21.0.4 2024-07-16` → `21.0.4`）。
fn version_token(line: &str) -> String {
    let quoted = line.split('"').nth(1).unwrap_or("").trim();
    if !quoted.is_empty() {
        return quoted.to_string();
    }
    line.split_whitespace()
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or("")
        .to_string()
}

/// 运行 `<exe> --version` 解析版本号与发行名（老 Java 8 自动回退 -version）。
pub(crate) fn probe_java(exe: &Path) -> Option<(String, String)> {
    let (version, first) = probe_java_line(exe)?;
    // 发行名：先用路径片段匹配常见发行版；退而用输出首行首词。
    let vendor = vendor_from_path(exe).unwrap_or_else(|| {
        let word = first.split_whitespace().next().unwrap_or("java");
        match word.to_ascii_lowercase().as_str() {
            "openjdk" => "OpenJDK".to_string(),
            _ => {
                let mut w = word.to_string();
                if let Some(r0) = w.get_mut(0..1) {
                    let head = r0.to_uppercase();
                    w.replace_range(0..1, &head);
                }
                w
            }
        }
    });
    Some((version, vendor))
}

/// java 可执行文件同目录（bin）下有 javac 即 JDK，否则 JRE。
fn is_jdk(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    dir.join("javac.exe").is_file() || dir.join("javac").is_file()
}

/// 从路径片段猜测发行版名称（不够准确，但比空白好）。
fn vendor_from_path(exe: &Path) -> Option<String> {
    let hay = exe.to_string_lossy().to_ascii_lowercase();
    const MAP: &[(&str, &str)] = &[
        ("temurin", "Eclipse Temurin"),
        ("adoptium", "Eclipse Temurin"),
        ("corretto", "Amazon Corretto"),
        ("zulu", "Azul Zulu"),
        ("liberica", "BellSoft Liberica"),
        ("graal", "GraalVM"),
        ("microsoft", "Microsoft OpenJDK"),
        ("sapmachine", "SapMachine"),
        ("dragonwell", "Alibaba Dragonwell"),
        ("oracle", "Oracle Java"),
        ("jetbrains", "JetBrains Runtime"),
        ("jbr", "JetBrains Runtime"),
        ("redhat", "Red Hat OpenJDK"),
    ];
    MAP.iter()
        .find(|(kw, _)| hay.contains(kw))
        .map(|(_, name)| name.to_string())
}

// ── 候选目录收集 ─────────────────────────────────────────────────────────

/// 把一个「Java 主目录候选」变成 java 可执行文件路径（不存在则 None）。
/// 兼容三种形态：目录本身 / 目录+bin / macOS Contents/Home。
fn exe_in_home(dir: &Path) -> Option<PathBuf> {
    let exe = java_exe_name();
    let mut tries = vec![dir.join("bin").join(&exe)];
    if cfg!(target_os = "macos") {
        tries.push(
            dir.join("Contents")
                .join("Home")
                .join("bin")
                .join(&exe),
        );
    }
    tries.into_iter().find(|p| p.is_file())
}

/// 已知发行版/通用安装根目录（存在才扫）。
fn candidate_roots() -> Vec<(PathBuf, &'static str)> {
    let mut roots: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Ok(home) = std::env::var("JAVA_HOME") {
        roots.push((PathBuf::from(home), "JAVA_HOME"));
    }
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let pf86 = std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
    let local = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| String::new());
    let user_home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();

    if cfg!(windows) {
        let pf = PathBuf::from(pf);
        for (sub, src) in [
            ("Java", "Scan"),
            ("Eclipse Adoptium", "Scan"),
            ("Microsoft", "Scan"),
            ("Zulu", "Scan"),
            ("Amazon Corretto", "Scan"),
            ("BellSoft", "Scan"),
            ("SapMachine", "Scan"),
            ("Semeru", "Scan"),
            ("RedHat", "Scan"),
        ] {
            roots.push((pf.join(sub), src));
        }
        let pf86 = PathBuf::from(pf86);
        roots.push((pf86.join("Java"), "Scan"));
        if !local.is_empty() {
            let local = PathBuf::from(local);
            roots.push((local.join("Programs").join("Eclipse Adoptium"), "Scan"));
            roots.push((local.join("Programs").join("Java"), "Scan"));
        }
    } else {
        roots.push((PathBuf::from("/usr/lib/jvm"), "Scan"));
        roots.push((PathBuf::from("/opt/java"), "Scan"));
        roots.push((PathBuf::from("/opt/homebrew/opt"), "Scan")); // macOS Homebrew openjdk
        roots.push((PathBuf::from("/Library/Java/JavaVirtualMachines"), "Scan"));
    }
    // IntelliJ 管理的 JDK（跨平台都在用户目录 .jdks）
    if !user_home.is_empty() {
        roots.push((PathBuf::from(&user_home).join(".jdks"), "Scan"));
    }
    // 启动器私有目录（旧位置，兼容已装好的运行时）
    if let Ok(managed) = crate::java_installer::managed_root() {
        roots.push((managed, "Managed"));
    }
    roots.retain(|(p, _)| p.is_dir());
    roots
}

/// 从 root 起按深度优先找「含 java 可执行文件」的 home 目录，最多下钻
/// max_depth 层；命中即停止下钻（避免把 JDK 内部的 jre/ 再算一份）。
/// 用于 Mojang 自带运行时这类层级更深的目录：
///   runtime/<组件>/<平台>/<组件>/bin/java[.exe]
/// 返回 (home, exe)。
fn collect_homes(root: &Path, max_depth: usize) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if let Some(exe) = exe_in_home(&dir) {
            out.push((dir, exe));
            continue;
        }
        if depth >= max_depth {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let child = entry.path();
                if child.is_dir() {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    out
}

/// 扫描所有候选目录，返回去重后的可用 Java 列表（尽力而为，从不报错）。
pub(crate) fn scan_java_installations() -> Vec<JavaRuntime> {
    // 与 find_java(None) 对齐，标出「当前默认」的那一个。
    let default_path = crate::minecraft_manager::find_java(None)
        .map(|p| normalize_path(&p))
        .unwrap_or_default();

    let mut out: Vec<JavaRuntime> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let push_runtime = |runtime: JavaRuntime, seen: &mut Vec<String>, out: &mut Vec<JavaRuntime>| {
        if seen.contains(&runtime.path) {
            return;
        }
        seen.push(runtime.path.clone());
        out.push(runtime);
    };

    for (root, source) in candidate_roots() {
        // 逐个检查 root 下的子目录（也允许 root 自带 bin/java，如 JAVA_HOME）。
        let mut homes: Vec<(PathBuf, &str)> = Vec::new();
        if exe_in_home(&root).is_some() {
            homes.push((root.clone(), source));
        }
        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                let dir = entry.path();
                if dir.is_dir() {
                    homes.push((dir, source));
                }
            }
        }
        for (home, source) in homes {
            let Some(exe) = exe_in_home(&home) else { continue };
            let Some((version, vendor)) = probe_java(&exe) else { continue };
            let path = normalize_path(&exe);
            let is_default = !default_path.is_empty() && path == default_path;
            push_runtime(
                JavaRuntime {
                    path,
                    home: normalize_path(&home),
                    version,
                    vendor,
                    kind: if is_jdk(&exe) { "JDK" } else { "JRE" }.to_string(),
                    source: source.to_string(),
                    is_default,
                },
                &mut seen,
                &mut out,
            );
        }
    }

    // Mojang 自带的运行时（.minecraft/runtime/<组件>/<平台>/<组件>）：
    // 不在 PATH、也不在常见安装目录，但完全可用于启动游戏（很多机器上
    // 只有这一份 Java）。层级比普通安装深，单独按 3 层深度扫，
    // 其余候选目录仍维持「只看一层子目录」，避免在大目录上做全树递归。
    if let Ok(mc_dir) = crate::minecraft_manager::get_minecraft_dir() {
        let runtime_root = mc_dir.join("runtime");
        if runtime_root.is_dir() {
            for (home, exe) in collect_homes(&runtime_root, 3) {
                let Some((version, vendor)) = probe_java(&exe) else { continue };
                let path = normalize_path(&exe);
                let is_default = !default_path.is_empty() && path == default_path;
                push_runtime(
                    JavaRuntime {
                        path,
                        home: normalize_path(&home),
                        version,
                        vendor,
                        kind: if is_jdk(&exe) { "JDK" } else { "JRE" }.to_string(),
                        source: "Minecraft runtime".to_string(),
                        is_default,
                    },
                    &mut seen,
                    &mut out,
                );
            }
        }
    }

    // PATH 上的 java（find_java 的最后兜底）。若已被上面扫到会按路径去重。
    if let Ok(path) = std::env::var("PATH") {
        let exe = java_exe_name();
        for dir in std::env::split_paths(&path) {
            let exe_path = dir.join(&exe);
            if !exe_path.is_file() {
                continue;
            }
            let Some((version, vendor)) = probe_java(&exe_path) else { continue };
            let path_str = normalize_path(&exe_path);
            let is_default = !default_path.is_empty() && path_str == default_path;
            push_runtime(
                JavaRuntime {
                    path: path_str,
                    home: normalize_path(&dir),
                    version,
                    vendor,
                    kind: if is_jdk(&exe_path) { "JDK" } else { "JRE" }.to_string(),
                    source: "PATH".to_string(),
                    is_default,
                },
                &mut seen,
                &mut out,
            );
            break; // PATH 上通常只有一个 java，命中第一个即可
        }
    }

    // 默认的那个排最前，其余按版本号倒序（新版本在前）。
    out.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| java_version_key(&b.version).cmp(&java_version_key(&a.version)))
    });
    out
}

/// 版本排序键：把 "1.8.0_391" / "17.0.2" / "21" 切成数值段比较。
fn java_version_key(v: &str) -> Vec<u64> {
    v.split(|c: char| c == '.' || c == '_' || c == '-' || c == '+')
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect()
}

/// 版本字符串 → 主版本号（`21.0.4` → 21，`1.8.0_391` → 8）。
pub(crate) fn version_major(version: &str) -> Option<u32> {
    let mut nums = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u32>().ok());
    let first = nums.next()?;
    if first == 1 {
        nums.next()
    } else {
        Some(first)
    }
}

fn normalize_path(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\")
}

fn java_exe_name() -> &'static str {
    if cfg!(windows) {
        "java.exe"
    } else {
        "java"
    }
}

// ── Tauri 命令 ──────────────────────────────────────────────────────────

/// 扫描本机常见位置，列出可用的 Java 运行时（阻塞工作放 spawn_blocking）。
#[tauri::command]
pub async fn list_java_installations() -> Result<Vec<JavaRuntime>, String> {
    // 游戏运行中不做 Java 探测：扫描会给每个已装 Java 起一次 `java -version`，
    // 在游戏旁边既浪费 CPU/IO 又容易闪窗口。注意这里只拦「设置页/列表」这类
    // 探测，启动流程里的 select_java 仍然需要扫描结果。
    if crate::game_launcher::is_running() {
        return Ok(Vec::new());
    }
    tauri::async_runtime::spawn_blocking(scan_java_installations)
        .await
        .map_err(|e| format!("Java scan task aborted: {e}"))
}

/// 校验用户填写的自定义 Java 路径（可直接填 java.exe 或其所在目录）。
/// 成功返回解析出的运行时信息（source = "自定义"），失败给出可读原因。
#[tauri::command]
pub async fn check_java_path(path: String) -> Result<JavaRuntime, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("Path is empty".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let p = PathBuf::from(&path);
        let is_file = p.is_file();
        let exe = if is_file {
            p.clone()
        } else if p.is_dir() {
            p.join(java_exe_name())
        } else {
            return Err(format!("Path does not exist: {path}"));
        };
        if !exe.is_file() {
            return Err(format!(
                "No Java executable found under this directory ({}) — you can paste the full path to java.exe instead",
                java_exe_name()
            ));
        }
        let Some((version, vendor)) = probe_java(&exe) else {
            return Err(format!("Failed to run this Java (--version failed): {}", exe.display()));
        };
        let home = if is_file {
            exe.parent().and_then(|b| b.parent()).unwrap_or(&p).to_path_buf()
        } else {
            p.clone()
        };
        Ok(JavaRuntime {
            path: normalize_path(&exe),
            home: normalize_path(&home),
            version,
            vendor,
            kind: if is_jdk(&exe) { "JDK" } else { "JRE" }.to_string(),
            source: "Custom".to_string(),
            is_default: false,
        })
    })
    .await
    .map_err(|e| format!("Validation task aborted: {e}"))?
}
