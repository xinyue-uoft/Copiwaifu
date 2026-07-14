//! Per-region click-through for the pet window.
//!
//! The main window is a large transparent rectangle; only the model, the
//! speech bubble, completion toasts, and the context menu should actually
//! swallow mouse events. The frontend reports those rectangles (window-local
//! logical coordinates) and a background poller toggles
//! `set_ignore_cursor_events` as the global cursor enters/leaves them.
//!
//! The poller also streams the cursor position to the webview so the model
//! can keep eye contact even while the window is click-through and the
//! webview receives no native mouse events.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::shell::MAIN_WINDOW_LABEL;

const CURSOR_EVENT: &str = "cursor:global";
const POLL_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct InteractiveRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl InteractiveRegion {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }
}

#[derive(Default)]
pub struct CursorPassthroughState {
    /// `None` until the frontend reports its first region set; the window
    /// stays fully interactive as a safe fallback (pre-feature behavior).
    regions: Mutex<Option<Vec<InteractiveRegion>>>,
    ignoring: AtomicBool,
}

#[derive(Clone, Serialize)]
struct CursorPayload {
    x: f64,
    y: f64,
}

#[tauri::command]
pub fn set_interactive_regions(
    state: tauri::State<'_, CursorPassthroughState>,
    regions: Vec<InteractiveRegion>,
) {
    if let Ok(mut guard) = state.regions.lock() {
        *guard = Some(regions);
    }
}

pub fn init(app: &tauri::App) {
    app.manage(CursorPassthroughState::default());
    let app_handle = app.handle().clone();
    std::thread::spawn(move || poll_loop(&app_handle));
}

fn poll_loop(app_handle: &AppHandle) {
    let mut last_emitted: Option<(i64, i64)> = None;

    loop {
        std::thread::sleep(POLL_INTERVAL);

        let Some(window) = app_handle.get_webview_window(MAIN_WINDOW_LABEL) else {
            continue;
        };
        if !window.is_visible().unwrap_or(false) {
            continue;
        }

        let (Ok(cursor), Ok(origin)) = (app_handle.cursor_position(), window.outer_position())
        else {
            continue;
        };
        let scale = window.scale_factor().unwrap_or(1.0);
        let x = (cursor.x - f64::from(origin.x)) / scale;
        let y = (cursor.y - f64::from(origin.y)) / scale;

        let state = app_handle.state::<CursorPassthroughState>();
        let interactive = match state.regions.lock() {
            Ok(guard) => match guard.as_ref() {
                Some(regions) => regions.iter().any(|region| region.contains(x, y)),
                None => true,
            },
            Err(_) => true,
        };

        let ignore = !interactive;
        if state.ignoring.swap(ignore, Ordering::Relaxed) != ignore {
            if let Err(err) = window.set_ignore_cursor_events(ignore) {
                log::warn!("[cursor] failed to toggle click-through: {err}");
            }
        }

        // Half-logical-pixel dedup key: skip emits while the cursor is still.
        let key = ((x * 2.0).round() as i64, (y * 2.0).round() as i64);
        if last_emitted != Some(key) {
            last_emitted = Some(key);
            let _ = app_handle.emit_to(MAIN_WINDOW_LABEL, CURSOR_EVENT, CursorPayload { x, y });
        }
    }
}
