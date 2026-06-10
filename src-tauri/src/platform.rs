use std::path::PathBuf;

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            let mut home = PathBuf::from(drive);
            home.push(path);
            Some(home)
        })
}

pub fn home_dir_result() -> Result<PathBuf, String> {
    home_dir().ok_or_else(|| "Could not resolve the user home directory".to_string())
}

pub fn runtime_dir() -> Result<PathBuf, String> {
    Ok(home_dir_result()?.join(".copiwaifu"))
}

pub fn primary_port_file() -> Result<PathBuf, String> {
    Ok(runtime_dir()?.join("port"))
}

pub fn fallback_port_file() -> PathBuf {
    std::env::temp_dir().join("copiwaifu-port")
}

// ── Window elevation (shared by the pet window and the notification window) ──────
// NSWindowStyleMaskNonActivatingPanel
#[cfg(target_os = "macos")]
#[allow(non_upper_case_globals)]
const NS_NON_ACTIVATING_PANEL: i32 = 1 << 7;

/// Turn a window into a non-activating floating panel: it floats above other
/// apps, joins all Spaces, and — crucially — receives clicks WITHOUT stealing
/// focus from the terminal / AI tool. Used for both the desktop pet and the
/// notification window (the latter's Dismiss button needs the clicks).
#[cfg(target_os = "macos")]
#[allow(deprecated)]
pub fn elevate_panel(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    use tauri_nspanel::{cocoa::appkit::NSWindowCollectionBehavior, WebviewWindowExt};

    let panel = window.to_panel().unwrap();

    panel.set_style_mask(NS_NON_ACTIVATING_PANEL);
    panel.set_collection_behaviour(
        NSWindowCollectionBehavior::NSWindowCollectionBehaviorCanJoinAllSpaces
            | NSWindowCollectionBehavior::NSWindowCollectionBehaviorStationary
            | NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary,
    );
    panel.set_level(1000); // NSScreenSaverWindowLevel
    panel.order_front_regardless();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn elevate_panel(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    window.set_always_on_top(true)
}
