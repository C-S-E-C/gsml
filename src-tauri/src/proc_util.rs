//! 子进程辅助：隐藏控制台窗口。
//!
//! Windows 上从 GUI 进程（Tauri 主进程）拉起控制台程序时，系统默认会给它新建
//! 一个控制台窗口 —— 在启动器里表现为「cmd 一闪而过」。凡是 spawn `java.exe`
//! 一类控制台程序的地方都应当调用 [`hide_console`]（`CREATE_NO_WINDOW`）。
//!
//! 注意：`Stdio::piped()` / `output()` 只是接管标准输入输出，并不能阻止窗口创建。

use std::process::Command;

/// 让该子进程不显示控制台窗口（仅 Windows 生效，其它平台空操作）。
pub(crate) fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW
        cmd.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    {
        // 避免 unused 警告；非 Windows 无需处理
        let _ = cmd;
    }
}
