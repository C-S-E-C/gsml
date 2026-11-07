//! 机器码识别（get_code）：采集主板/BIOS/CPU/机箱/系统 UUID/硬盘/GPU/MAC 指纹，
//! clean 后按固定顺序用 '-' 连接并整体大写。
//!
//! 优化要点：
//! - Windows 端全部 CIM 字段合并为一次 PowerShell 进程（JSON 批量返回），
//!   替代原先 6-8 次独立调用（每次都是一次 PS 冷启动，串行需 4-8 秒）；
//! - 8 个字段采集器并行执行（std::thread::scope），总耗时 ≈ 最慢的一个而非全部之和；
//! - run() 用独立线程持续排空子进程管道，避免输出超过 64KB 管道缓冲导致
//!   子进程卡死到超时被杀、输出丢失；
//! - clean() 等价于只保留 [A-Za-z0-9]，不再每次编译正则；循环内正则全部改为
//!   一次性编译（OnceLock）或纯字符串处理；
//! - 硬盘只采「固定内置盘」：Windows 按 MediaType/InterfaceType 过滤、Linux 按
//!   lsblk RM 过滤、macOS 改查 NVMe/SATA 数据源，避免插拔 U 盘改变识别码。

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use md5::{Md5, Digest};

// ============================================================
// 平台判定
// ============================================================
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Os {
    Windows,
    Linux,
    Darwin,
}

fn current_os() -> Os {
    if cfg!(target_os = "windows") {
        Os::Windows
    } else if cfg!(target_os = "macos") {
        Os::Darwin
    } else {
        Os::Linux
    }
}

// ============================================================
// 清洗
// ============================================================
/// 只保留 [A-Za-z0-9]（与旧版正则行为一致）；空则返回 "0"
fn clean(v: &str) -> String {
    let out: String = v.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if out.is_empty() {
        "0".to_string()
    } else {
        out
    }
}

fn join_all<I: IntoIterator<Item = String>>(vals: I) -> String {
    // 先清洗再排序去重，保证稳定输出（BTreeSet 自带排序）
    let set: BTreeSet<String> = vals
        .into_iter()
        .filter(|v| !v.is_empty())
        .map(|v| clean(&v))
        .collect();
    set.into_iter().collect()
}

// ============================================================
// 运行命令
// ============================================================
/// 运行命令并返回 stdout（UTF-8 lossy）。带超时，超时杀进程返回空（与旧版一致）。
/// 由独立线程持续排空 stdout，避免输出写满 64KB 管道缓冲导致子进程
/// 卡死到超时被杀、输出丢失（旧版缺陷：进程结束前从不读取管道）。
fn run(cmd: &[&str], timeout_secs: u64) -> String {
    let mut builder = Command::new(cmd[0]);
    builder
        .args(&cmd[1..])
        .stdout(Stdio::piped())
        // 旧版捕获 stderr 但从不读取，语义等同丢弃；直接 null 顺带避免
        // 子进程因 stderr 写满缓冲而阻塞
        .stderr(Stdio::null());
    // Windows：隐藏子进程（PowerShell/wmic/nvidia-smi）的控制台窗口，
    // 否则正式版（无终端宿主）每次采集都会闪黑框。
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        builder.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match builder.spawn() {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return String::new();
    };

    // 排空线程：子进程存活期间持续读走 stdout，退出后读到 EOF
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let slot = Arc::clone(&buf);
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut s = stdout;
        let _ = s.read_to_end(&mut out);
        if let Ok(mut b) = slot.lock() {
            *b = out;
        }
    });

    // 带超时等待退出；超时杀掉子进程防止泄漏
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return String::new();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = reader.join();
                return String::new();
            }
        }
    }
    let _ = reader.join();
    let bytes = buf.lock().map(|b| b.clone()).unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn read_file_first_line(path: &str) -> String {
    match fs::read_to_string(path) {
        Ok(content) => content.lines().next().unwrap_or("").trim().to_string(),
        Err(_) => String::new(),
    }
}

// ============================================================
// Windows 辅助
// ============================================================
fn win_cim(class_name: &str, prop: &str) -> String {
    let ps = format!(
        "(Get-CimInstance Win32_{class_name} | Select-Object -First 1 -ExpandProperty {prop})"
    );
    let out = run(&["powershell", "-NoProfile", "-Command", &ps], 5);
    let out = out.trim();
    if !out.is_empty() {
        return out.to_string();
    }
    // 回退 wmic
    let out = run(&["wmic", class_name, "get", prop], 5);
    for line in out.lines().skip(1) {
        let line = line.trim();
        if !line.is_empty() {
            return line.to_string();
        }
    }
    String::new()
}

fn win_cim_all(class_name: &str, prop: &str, filter: &str) -> Vec<String> {
    // filter 非空时附加 Where-Object 子句（如排除可移动磁盘）
    let ps = if filter.is_empty() {
        format!("Get-CimInstance Win32_{class_name} | Select-Object -ExpandProperty {prop}")
    } else {
        format!(
            "Get-CimInstance Win32_{class_name} | Where-Object {{ {filter} }} | Select-Object -ExpandProperty {prop}"
        )
    };
    let out = run(&["powershell", "-NoProfile", "-Command", &ps], 5);
    out.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// 一次 PowerShell 进程取回全部 Windows CIM 字段（JSON 批量返回）。
/// 空字段表示该次查询无数据，调用方自行走 wmic / 单独 PS 回退。
const WIN_CIM_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$r = [ordered]@{
    b   = @(Get-CimInstance Win32_BaseBoard | Select-Object -First 1 -ExpandProperty SerialNumber)
    bi  = @(Get-CimInstance Win32_BIOS | Select-Object -First 1 -ExpandProperty SerialNumber)
    cpu = @(Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty ProcessorId)
    ch  = @(Get-CimInstance Win32_SystemEnclosure | Select-Object -First 1 -ExpandProperty SerialNumber)
    u   = @(Get-CimInstance Win32_ComputerSystemProduct | Select-Object -First 1 -ExpandProperty UUID)
    d   = @(Get-CimInstance Win32_DiskDrive | Where-Object { $_.MediaType -eq 'Fixed hard disk media' -and $_.InterfaceType -ne 'USB' } | Select-Object -ExpandProperty SerialNumber)
    g   = @(Get-CimInstance Win32_VideoController | Select-Object -ExpandProperty PNPDeviceID)
    m   = @(Get-CimInstance Win32_NetworkAdapter | Where-Object { $_.PhysicalAdapter -eq $true } | Select-Object -ExpandProperty PermanentAddress)
    m2  = @(Get-CimInstance Win32_NetworkAdapter | Where-Object { $_.PhysicalAdapter -eq $true } | Select-Object -ExpandProperty MACAddress)
}
$r | ConvertTo-Json -Compress
"#;

/// 批量 CIM 结果（字段名对应 WIN_CIM_SCRIPT 里的键）
#[derive(Default)]
struct WinCim {
    board: String,
    bios: String,
    cpu: String,
    chassis: String,
    uuid: String,
    disks: Vec<String>,
    gpu_pnp: Vec<String>,
}

/// 取批量 JSON 里标量字段（脚本里统一包了 @()，取首个元素）
fn json_str(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 取批量 JSON 里列表字段
fn json_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn win_cim_batch() -> WinCim {
    let out = run(&["powershell", "-NoProfile", "-Command", WIN_CIM_SCRIPT], 8);
    let Ok(v) = serde_json::from_str::<Value>(out.trim()) else {
        return WinCim::default();
    };
    WinCim {
        board: json_str(&v, "b"),
        bios: json_str(&v, "bi"),
        cpu: json_str(&v, "cpu"),
        chassis: json_str(&v, "ch"),
        uuid: json_str(&v, "u"),
        disks: json_list(&v, "d"),
        gpu_pnp: json_list(&v, "g"),
    }
}

// ============================================================
// Linux 辅助
// ============================================================
fn linux_cpu_serial() -> String {
    if let Ok(content) = fs::read_to_string("/proc/cpuinfo") {
        for line in content.lines() {
            let lower = line.to_lowercase();
            if lower.starts_with("serial") {
                if let Some(idx) = line.find(':') {
                    return line[idx + 1..].trim().to_string();
                }
            }
        }
    }
    String::new()
}

// ============================================================
// macOS 辅助
// ============================================================
fn mac_ioreg(key: &str) -> String {
    let out = run(
        &["ioreg", "-rd1", "-c", "IOPlatformExpertDevice"],
        5,
    );
    for line in out.lines() {
        if line.contains(key) && line.contains('=') {
            let v = line.splitn(2, '=').nth(1).unwrap_or("").trim();
            let v = v.trim_matches('"').to_string();
            return v;
        }
    }
    String::new()
}

// ============================================================
// 字段采集器（Windows 端共享一次批量 CIM 结果）
// ============================================================

fn get_board_serial(win: Option<&WinCim>) -> String {
    match current_os() {
        Os::Windows => {
            if let Some(w) = win {
                if !w.board.is_empty() {
                    return w.board.clone();
                }
            }
            win_cim("BaseBoard", "SerialNumber")
        }
        Os::Linux => "0".to_string(),
        Os::Darwin => String::new(),
    }
}

fn get_bios_serial(win: Option<&WinCim>) -> String {
    match current_os() {
        Os::Windows => {
            if let Some(w) = win {
                if !w.bios.is_empty() {
                    return w.bios.clone();
                }
            }
            win_cim("BIOS", "SerialNumber")
        }
        Os::Linux => "0".to_string(),
        Os::Darwin => String::new(),
    }
}

fn get_cpu_serial(win: Option<&WinCim>) -> String {
    match current_os() {
        Os::Windows => {
            if let Some(w) = win {
                if !w.cpu.is_empty() {
                    return w.cpu.clone();
                }
            }
            win_cim("Processor", "ProcessorId")
        }
        Os::Linux => linux_cpu_serial(),
        Os::Darwin => String::new(),
    }
}

fn get_product_uuid(win: Option<&WinCim>) -> String {
    match current_os() {
        Os::Windows => {
            if let Some(w) = win {
                if !w.uuid.is_empty() {
                    return w.uuid.clone();
                }
            }
            win_cim("ComputerSystemProduct", "UUID")
        }
        Os::Linux => "0".to_string(),
        Os::Darwin => mac_ioreg("IOPlatformUUID"),
    }
}

fn get_chassis_serial(win: Option<&WinCim>) -> String {
    match current_os() {
        Os::Windows => {
            if let Some(w) = win {
                if !w.chassis.is_empty() {
                    return w.chassis.clone();
                }
            }
            win_cim("SystemEnclosure", "SerialNumber")
        }
        Os::Linux => "0".to_string(),
        Os::Darwin => String::new(),
    }
}

fn get_disk_serials(win: Option<&WinCim>) -> String {
    let mut vals: Vec<String> = Vec::new();
    match current_os() {
        Os::Windows => {
            if let Some(w) = win {
                if !w.disks.is_empty() {
                    vals = w.disks.clone();
                }
            }
            if vals.is_empty() {
                // 只取固定内置盘，排除 U 盘/移动硬盘/读卡器
                vals = win_cim_all(
                    "DiskDrive",
                    "SerialNumber",
                    "$_.MediaType -eq 'Fixed hard disk media' -and $_.InterfaceType -ne 'USB'",
                );
            }
        }
        Os::Linux => {
            // 1) /dev/disk/by-id：仅取 ata-/nvme-/scsi-/wwn- 前缀，
            //    去掉前缀与 -partN 后缀（纯字符串处理，不编译正则）
            if let Ok(entries) = fs::read_dir("/dev/disk/by-id") {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let Some(n) = ["ata-", "nvme-", "scsi-", "wwn-"]
                        .iter()
                        .find_map(|p| name.strip_prefix(p))
                    else {
                        continue;
                    };
                    // 等价于正则 -part\d+$：rfind 到 "-part" 且其后全为数字
                    let n = match n.rfind("-part") {
                        Some(i)
                            if !n[i + 5..].is_empty()
                                && n[i + 5..].bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            &n[..i]
                        }
                        _ => n,
                    };
                    vals.push(n.to_string());
                }
            }
            // 2) lsblk SERIAL + RM：RM=1 的可移动设备（U盘/读卡器）不参与指纹
            let out = run(&["lsblk", "-dno", "SERIAL,RM"], 5);
            for line in out.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                // 行格式 "<SERIAL> <RM>"；序列号为空时该行只剩 RM 值
                let Some((serial, rm)) = line.rsplit_once(' ') else {
                    continue;
                };
                if rm.trim() == "1" {
                    continue;
                }
                let serial = serial.trim();
                if !serial.is_empty() {
                    vals.push(serial.to_string());
                }
            }
        }
        Os::Darwin => {
            // 只查内置 NVMe / SATA；SPStorageDataType 会把 U 盘等 USB 存储
            // 的序列号也收进来，导致插拔 U 盘改变识别码
            let out = run(
                &["system_profiler", "SPNVMeDataType", "SPSerialATADataType"],
                10,
            );
            for line in out.lines() {
                if line.contains("Serial Number") && line.contains(':') {
                    if let Some(idx) = line.find(':') {
                        let v = line[idx + 1..].trim();
                        if !v.is_empty() {
                            vals.push(v.to_string());
                        }
                    }
                }
            }
        }
    }
    join_all(vals)
}

fn get_gpu_uuids(win: Option<&WinCim>) -> String {
    let mut vals: Vec<String> = Vec::new();
    match current_os() {
        Os::Linux => {
            let out = run(
                &["nvidia-smi", "--query-gpu=uuid", "--format=csv,noheader"],
                5,
            );
            for line in out.lines() {
                let l = line.trim();
                if !l.is_empty() {
                    vals.push(l.to_string());
                }
            }
            if let Ok(entries) = glob_path("/sys/class/drm") {
                for entry in entries {
                    let p = format!("{entry}/device/unique_id");
                    let v = read_file_first_line(&p);
                    if !v.is_empty() {
                        vals.push(v);
                    }
                }
            }
        }
        Os::Windows => {
            if let Some(w) = win {
                if !w.gpu_pnp.is_empty() {
                    vals = w.gpu_pnp.clone();
                }
            }
            if vals.is_empty() {
                vals = win_cim_all("VideoController", "PNPDeviceID","");
            }
            // nvidia-smi 的 UUID 与 PNPDeviceID 信息不重复，保留
            let out = run(
                &["nvidia-smi", "--query-gpu=uuid", "--format=csv,noheader"],
                5,
            );
            for line in out.lines() {
                let l = line.trim();
                if !l.is_empty() {
                    vals.push(l.to_string());
                }
            }
        }
        Os::Darwin => {
            let out = run(&["system_profiler", "SPDisplaysDataType"], 10);
            for line in out.lines() {
                if line.contains("Serial Number") && line.contains(':') {
                    if let Some(idx) = line.find(':') {
                        let v = line[idx + 1..].trim();
                        if !v.is_empty() {
                            vals.push(v.to_string());
                        }
                    }
                }
            }
        }
    }
    join_all(vals)
}

// 简单模拟 Python 的 glob.glob("/sys/class/drm/card*/device/unique_id") 的目录枚举
fn glob_path(dir: &str) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let base = Path::new(dir);
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("card") {
            out.push(entry.path().to_string_lossy().to_string());
        }
    }
    Ok(out)
}

// ============================================================
// 主流程
// ============================================================
// 进程级缓存：机器码在本次运行内不会变化，算一次即可。之后所有调用
// （含前端重复 invoke、重开页面）直接命中缓存，零开销返回。
static IDENTIFIER_CACHE: OnceLock<String> = OnceLock::new();

/// 实际采集。8 个字段采集器并行执行（Windows 端共享一次 CIM 批量结果），
/// 输出格式与旧版一致：clean 后按固定顺序用 '-' 连接并整体大写。
/// 重 I/O：Windows 上要冷启动一个 PowerShell 进程跑 CIM 批量查询。
fn compute_device_identifier() -> String {
    let win = if current_os() == Os::Windows {
        Some(win_cim_batch())
    } else {
        None
    };
    // 注意：保持旧版字段顺序不变（机器码结果与顺序相关）
    let getters: [fn(Option<&WinCim>) -> String; 7] = [
        get_board_serial,
        get_bios_serial,
        get_cpu_serial,
        get_product_uuid,
        get_chassis_serial,
        get_disk_serials,
        get_gpu_uuids,
    ];
    let parts: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = getters
            .iter()
            .map(|f| {
                let win = win.as_ref();
                s.spawn(move || f(win))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .map(|p| clean(&p))                       // 先规范化
            .map(|p| Md5::digest(p.as_bytes())// 再 MD5
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>())
            .collect()
    });
    parts.join(" ").to_uppercase()
}

/// 机器码命令。
///
/// 必须是 async：Tauri 2 的同步命令在主线程上执行，而本命令要做硬件
/// 采集（PowerShell 冷启动 + CIM 查询，可能耗时数秒），同步版会把窗口
/// 事件循环和其它 IPC（如账号加载 list_accounts）全部卡死。async 命令
/// 脱离主线程执行，重活再丢进 spawn_blocking，避免占用异步运行时的
/// 事件循环线程。
#[tauri::command]
pub async fn get_device_identifier() -> String {
    if let Some(id) = IDENTIFIER_CACHE.get() {
        return id.clone();
    }
    tauri::async_runtime::spawn_blocking(|| {
        IDENTIFIER_CACHE
            .get_or_init(compute_device_identifier)
            .clone()
    })
    .await
    .unwrap_or_default()
}