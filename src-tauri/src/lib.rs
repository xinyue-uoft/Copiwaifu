#[cfg(target_os = "macos")]
use tauri::ActivationPolicy;
use tauri::Manager;

#[cfg(target_os = "macos")]
use std::process::Command;

mod ai_talk;
mod navigator;
mod platform;
mod shell;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg(target_os = "macos")]
fn fix_path_env() {
    if std::env::var_os("PATH")
        .map(|p| p.to_string_lossy().contains("/usr/local"))
        .unwrap_or(false)
    {
        return;
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    if let Ok(output) = Command::new(&shell).args(["-ilc", "echo $PATH"]).output() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            std::env::set_var("PATH", &path);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "macos")]
    fix_path_env();

    // Unified log file: ~/.copiwaifu/logs/copiwaifu.log (+ stdout in dev).
    // Frontend webviews log into the same file via @tauri-apps/plugin-log.
    let log_dir = platform::runtime_dir()
        .map(|dir| dir.join("logs"))
        .unwrap_or_else(|_| std::path::PathBuf::from("logs"));

    let builder = tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                        path: log_dir,
                        file_name: Some("copiwaifu".into()),
                    }),
                ])
                .level(log::LevelFilter::Info)
                .max_file_size(5_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .timezone_strategy(tauri_plugin_log::TimezoneStrategy::UseLocal)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init());

    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_nspanel::init());

    builder
        .setup(|app| {
            navigator::init(app);
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            app.handle().plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                None::<Vec<&str>>,
            ))?;
            shell::init(app)?;

            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(ActivationPolicy::Accessory);
                app.set_dock_visibility(false);
            }

            let window = app
                .get_webview_window("main")
                .or_else(|| app.webview_windows().into_values().next())
                .expect("failed to find the primary webview window");

            platform::elevate_panel(&window)?;

            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            navigator::commands::get_agent_status,
            navigator::commands::get_navigator_sessions,
            navigator::notification::get_notifications,
            navigator::notification::dismiss_notification,
            navigator::notification::get_completions,
            navigator::notification::dismiss_completion,
            ai_talk::generate_ai_talk,
            navigator::commands::uninstall_hooks,
            shell::commands::get_app_bootstrap,
            shell::commands::scan_model_directory,
            shell::commands::import_model_directory,
            shell::commands::scan_default_model,
            shell::commands::save_settings,
            shell::commands::open_settings_window,
            shell::commands::toggle_main_window_visibility,
            shell::commands::exit_app
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                if let Err(err) = navigator::hook_installer::uninstall_hooks() {
                    log::error!("[hooks] shutdown cleanup failed: {err}");
                }
            }
        });
}
