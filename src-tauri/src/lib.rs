use tauri::Manager;
use tauri_plugin_deep_link::DeepLinkExt;

mod account_manager;
mod db_handler;
mod deeplink_handler;
mod skin_manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // single-instance 必须先于 deep-link 注册：浏览器回调 glow-squid:// 时
        // Windows 会拉起第二个实例，由它把 argv 里的 URL 转发给主实例
        // （Cargo.toml 已为该插件启用 deep-link feature）
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // 1) 数据库
            let database = db_handler::init(app.handle())?; // 注意不是 &app.handle()
            app.manage(database);

            // 2) Deeplink
            deeplink_handler::init(app.handle());

            // 3) 开发模式下没有安装器，需要手动把 glow-squid 等 scheme 写进注册表，
            //    否则浏览器报 "the scheme does not have a registered handler"。
            //    安装版（NSIS/MSI）由安装器注册，这里重复调用幂等。
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            app.deep_link().register_all()?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            account_manager::login_offline,
            account_manager::login_microsoft,
            account_manager::list_accounts,
            account_manager::delete_account,
            account_manager::ms_account_details,
            account_manager::ms_change_skin,
            account_manager::ms_change_cape,
            skin_manager::get_account_skin,
            skin_manager::get_cape_texture,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
