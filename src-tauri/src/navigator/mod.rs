use std::sync::{Arc, Mutex};

use tauri::{App, AppHandle, Emitter, Manager};

pub mod agent;
pub mod commands;
pub mod events;
mod hook_helpers;
pub mod hook_installer;
pub mod notification;
mod presentation;
mod providers;
mod reconcile;
mod reducer;
pub mod server;
pub mod session_recovery;
mod session_store;
pub mod state;

use events::NavigatorEmission;
use state::NavigatorState;

pub struct NavigatorStore(pub Arc<Mutex<NavigatorState>>);

pub fn init(app: &mut App) {
    let state = Arc::new(Mutex::new(NavigatorState::new()));

    // 启动时从 session 文件恢复状态
    if let Ok(mut navigator) = state.lock() {
        session_recovery::recover(&mut navigator);
    }

    if let Err(err) = hook_installer::install_hooks() {
        eprintln!("navigator hook installation failed: {err}");
    }
    // Migration: strip any leftover blocking PermissionRequest http hook from a
    // previous build. If left behind, CC would POST to a route this build no
    // longer serves (404 → fail-open auto-allow). The notification feature
    // installs NO PermissionRequest hook — it observes the Notification event.
    if let Err(err) = hook_installer::strip_stale_permission_hook() {
        eprintln!("navigator stale permission-hook cleanup failed: {err}");
    }

    app.manage(NavigatorStore(state.clone()));
    app.manage(notification::NotificationStore(Mutex::new(
        notification::NotifState::default(),
    )));

    server::start(app.handle().clone(), state.clone());
    reconcile::start(app.handle().clone(), state.clone());
    agent::start_cleanup_loop(app.handle().clone(), state);
}

pub fn emit_all(app_handle: &AppHandle, emissions: Vec<NavigatorEmission>) {
    for emission in emissions {
        match emission {
            NavigatorEmission::StateChange(payload) => {
                let _ = app_handle.emit("agent:state-change", payload);
            }
            NavigatorEmission::SessionsChanged(payload) => {
                let _ = app_handle.emit("navigator:sessions-changed", payload);
            }
        }
    }
    // Keep the passive notification window in sync with the latest session state:
    // show pending cards, auto-dissolve resolved ones, hide when none remain.
    notification::reconcile(app_handle);
}
