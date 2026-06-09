// Pet-side PASSIVE notification window.
//
// Shows a card per Claude Code session that has been waiting for a permission
// decision for more than DEBOUNCE_MS. The debounce is the crux: every tool call
// briefly flips a session into `needs_attention` and back (especially auto-mode
// agent sessions), so without it the pet would flash a card — and churn its
// summary — on every command. Only a session that *stays* waiting (a genuine
// prompt the user must act on) survives the debounce.
//
// It makes NO decision — the user resolves permissions in their terminal /
// Claude. A card auto-dissolves when the reducer clears `needs_attention`, and a
// card can be locally Dismissed (muted) without touching CC. Driven entirely by
// the existing observe event stream — no blocking hook, no fail-open.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use super::events::{NavigatorSessionPayload, NavigatorSessionsPayload};

pub const NOTIFICATION_WINDOW_LABEL: &str = "notification";
const NOTIFICATION_CHANGED_EVENT: &str = "notification:changed";
// A session must stay continuously `needs_attention` for at least this long
// before its card appears — filters out the brief spikes of auto-handled tool
// calls, leaving only genuine prompts that actually wait for the user.
const DEBOUNCE_MS: u64 = 2500;

pub struct NotificationStore(pub Mutex<NotifState>);

#[derive(Default)]
pub struct NotifState {
    /// session_id -> dismissed pending-signature (Dismiss mute).
    muted: HashMap<String, String>,
    /// session_id -> epoch-ms when it was first observed pending (debounce timer).
    first_pending_ms: HashMap<String, u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationCard {
    pub session_id: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationPayload {
    pub cards: Vec<NotificationCard>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Distinguishes one pending prompt from the next on the same session, so a
/// Dismiss only mutes the specific request, not all future ones.
fn signature(session: &NavigatorSessionPayload) -> String {
    format!(
        "{}|{}|{}",
        session.session_title.as_deref().unwrap_or(""),
        session.tool_name.as_deref().unwrap_or(""),
        session.summary.as_deref().unwrap_or(""),
    )
}

fn build_cards(sessions: &NavigatorSessionsPayload, state: &mut NotifState) -> Vec<NotificationCard> {
    let now = now_ms();
    let pending: Vec<&NavigatorSessionPayload> = sessions
        .sessions
        .iter()
        .filter(|s| s.needs_attention == Some(true))
        .collect();

    // Reset both timers and mutes for sessions that are no longer pending — so a
    // new pending restarts the debounce, and the next pending re-shows.
    let pending_ids: std::collections::HashSet<&str> =
        pending.iter().map(|s| s.session_id.as_str()).collect();
    state
        .first_pending_ms
        .retain(|sid, _| pending_ids.contains(sid.as_str()));
    state.muted.retain(|sid, _| pending_ids.contains(sid.as_str()));

    pending
        .into_iter()
        .filter_map(|s| {
            // Debounce: stamp first-seen, and hide until it has waited DEBOUNCE_MS.
            let since = *state
                .first_pending_ms
                .entry(s.session_id.clone())
                .or_insert(now);
            if now.saturating_sub(since) < DEBOUNCE_MS {
                return None; // still settling — likely an auto-handled spike
            }

            let sig = signature(s);
            if state.muted.get(&s.session_id) == Some(&sig) {
                return None; // dismissed and still the same pending
            }

            Some(NotificationCard {
                session_id: s.session_id.clone(),
                agent: s.agent.as_str().to_string(),
                tool_name: s.tool_name.clone(),
                summary: s.summary.clone(),
                working_directory: s.working_directory.clone(),
                session_title: s.session_title.clone(),
                signature: sig,
            })
        })
        .collect()
}

/// Recompute visible cards from current session state + mutes + debounce, emit
/// them to the window, and show/hide the OS window. Called from the emit path on
/// every state change (incl. the ~1s polling loops, which drive the debounce),
/// from the dismiss command, and from settings save.
pub fn reconcile(app_handle: &AppHandle) {
    let Some(nav) = app_handle.try_state::<super::NavigatorStore>() else {
        return;
    };
    let Some(notif) = app_handle.try_state::<NotificationStore>() else {
        return;
    };

    let enabled = app_handle
        .try_state::<crate::shell::ShellStore>()
        .and_then(|store| {
            store
                .0
                .lock()
                .ok()
                .map(|state| state.settings.permission_approval_enabled)
        })
        .unwrap_or(true);

    let sessions = match nav.0.lock() {
        Ok(state) => state.sessions_snapshot(),
        Err(_) => return,
    };

    let cards = match notif.0.lock() {
        Ok(mut state) => build_cards(&sessions, &mut state),
        Err(_) => return,
    };

    let _ = app_handle.emit(
        NOTIFICATION_CHANGED_EVENT,
        NotificationPayload {
            cards: cards.clone(),
        },
    );

    if enabled && !cards.is_empty() {
        ensure_window(app_handle);
    } else {
        hide_window(app_handle);
    }
}

#[tauri::command]
pub fn get_notifications(
    app_handle: AppHandle,
    store: State<'_, NotificationStore>,
) -> Result<NotificationPayload, String> {
    let sessions = app_handle
        .try_state::<super::NavigatorStore>()
        .and_then(|nav| nav.0.lock().ok().map(|state| state.sessions_snapshot()))
        .ok_or_else(|| "navigator unavailable".to_string())?;
    let cards = {
        let mut state = store.0.lock().map_err(|err| err.to_string())?;
        build_cards(&sessions, &mut state)
    };
    Ok(NotificationPayload { cards })
}

#[tauri::command]
pub fn dismiss_notification(
    session_id: String,
    signature: String,
    app_handle: AppHandle,
    store: State<'_, NotificationStore>,
) -> Result<(), String> {
    if let Ok(mut state) = store.0.lock() {
        state.muted.insert(session_id, signature);
    }
    reconcile(&app_handle);
    Ok(())
}

fn ensure_window(app_handle: &AppHandle) {
    let app = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(window) = app.get_webview_window(NOTIFICATION_WINDOW_LABEL) {
            let _ = window.show();
            return;
        }
        match WebviewWindowBuilder::new(
            &app,
            NOTIFICATION_WINDOW_LABEL,
            WebviewUrl::App("index.html".into()),
        )
        .title("copiwaifu")
        .inner_size(360.0, 480.0)
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .build()
        {
            // Non-activating panel so the Dismiss button receives clicks without
            // stealing focus from the terminal / Claude.
            Ok(window) => {
                if let Err(err) = crate::platform::elevate_panel(&window) {
                    eprintln!("[notification] elevate failed: {err}");
                }
            }
            Err(err) => eprintln!("[notification] window build failed: {err}"),
        }
    });
}

fn hide_window(app_handle: &AppHandle) {
    let app = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(window) = app.get_webview_window(NOTIFICATION_WINDOW_LABEL) {
            let _ = window.hide();
        }
    });
}
