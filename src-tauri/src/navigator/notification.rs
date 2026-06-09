// Pet-side PASSIVE notification window.
//
// Shows a card per Claude Code session that is waiting for a permission decision
// (the reducer's `needs_attention` / WaitingAttention). It makes NO decision —
// the user resolves permissions in their terminal / Claude as usual. Each card
// auto-dissolves the instant the reducer clears `needs_attention` (the session's
// next event), and a card can be locally Dismissed (muted) without touching CC.
//
// Driven entirely by the existing observe event stream — no blocking hook, no
// fail-open, no conflict with Claude's own permission UI. The "mute" state lives
// in the backend (NotificationStore) so window visibility and dismissal stay
// consistent (the backend is the single source of truth for what's shown).

use std::{collections::HashMap, sync::Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use super::events::{NavigatorSessionPayload, NavigatorSessionsPayload};

pub const NOTIFICATION_WINDOW_LABEL: &str = "notification";
const NOTIFICATION_CHANGED_EVENT: &str = "notification:changed";

/// session_id -> the dismissed pending-signature. A card is hidden only while the
/// SAME pending is still showing; a new pending (different signature) re-shows.
pub struct NotificationStore(pub Mutex<HashMap<String, String>>);

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

fn build_cards(
    sessions: &NavigatorSessionsPayload,
    muted: &mut HashMap<String, String>,
) -> Vec<NotificationCard> {
    let pending: Vec<&NavigatorSessionPayload> = sessions
        .sessions
        .iter()
        .filter(|s| s.needs_attention == Some(true))
        .collect();

    // Forget mutes for sessions that are no longer pending — so the next pending
    // on that session re-shows (auto-dissolve already removed the resolved one).
    let pending_ids: std::collections::HashSet<&str> =
        pending.iter().map(|s| s.session_id.as_str()).collect();
    muted.retain(|sid, _| pending_ids.contains(sid.as_str()));

    pending
        .into_iter()
        .filter_map(|s| {
            let sig = signature(s);
            if muted.get(&s.session_id) == Some(&sig) {
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

/// Recompute the visible cards from current session state + mutes, emit them to
/// the window, and show/hide the OS window accordingly. Called from the emit
/// path on every state change, from the dismiss command, and from settings save.
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
        Ok(mut muted) => build_cards(&sessions, &mut muted),
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
        let mut muted = store.0.lock().map_err(|err| err.to_string())?;
        build_cards(&sessions, &mut muted)
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
    if let Ok(mut muted) = store.0.lock() {
        muted.insert(session_id, signature);
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
            // stealing focus from the terminal / Claude (the fix the approval
            // window lacked).
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
