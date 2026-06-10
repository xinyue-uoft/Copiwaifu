// Pet-side PASSIVE notification window + completion badge system.
//
// PERMISSION NOTIFICATIONS
// Shows a card per Claude Code session that has been waiting for a permission
// decision for more than NOTIF_DEBOUNCE_MS. Two mechanisms prevent re-showing:
//
// • Debounce (2.5s): filters sub-second auto-mode tool-call spikes that briefly
//   flip needs_attention without ever waiting for user input.
//
// • Session-scoped seen buffer (`shown_this_session`): a HashSet<signature>
//   that only grows — once a notification's content signature is inserted
//   (on Dismiss), it is NEVER shown again in this copiwaifu session. This
//   replaces the previous signature-based mute map, which was unreliable
//   because:
//     - intermediate events (PreToolUse, Notification…) can momentarily flip
//       needs_attention=false for one reconcile tick, causing retain() to drop
//       the mute;
//     - the pending_ids retain cleared mutes during any brief state transition;
//     - each clear triggered another card → window.show() → keyboard blocked.
//
//   With the seen buffer there is no retain/clear path — the dismiss is final.
//   A genuinely different prompt on the same session has a different signature
//   (different tool_name or summary) and passes through normally.
//
// COMPLETION BADGES
// Shows a "完工啦！" chip at the bottom of the pet window for up to 5 minutes
// after a session settles into Complete. Same 3s debounce. Stacks per-session.

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use super::events::{AgentState, NavigatorSessionPayload, NavigatorSessionsPayload};

// ── Permission notifications ──────────────────────────────────────────────────

pub const NOTIFICATION_WINDOW_LABEL: &str = "notification";
const NOTIFICATION_CHANGED_EVENT: &str = "notification:changed";
/// A session must stay continuously `needs_attention` for at least this long
/// before its card appears — filters out brief auto-handled tool-call spikes.
const NOTIF_DEBOUNCE_MS: u64 = 2500;

// ── Completion badges ─────────────────────────────────────────────────────────

pub const COMPLETION_CHANGED_EVENT: &str = "completion:changed";
const COMPLETE_DEBOUNCE_MS: u64 = 3_000;
const COMPLETION_BADGE_DISPLAY_MS: u64 = 300_000; // 5 min

// ── Shared state ──────────────────────────────────────────────────────────────

pub struct NotificationStore(pub Mutex<NotifState>);

#[derive(Default)]
pub struct NotifState {
    // -- permission notification state --

    /// session_id → epoch-ms when it was first observed pending (debounce timer).
    /// Cleared when the session leaves `needs_attention` so a genuine new
    /// pending on the same session re-starts the 2.5s clock.
    first_pending_ms: HashMap<String, u64>,

    /// Global seen buffer — content signatures dismissed in this copiwaifu
    /// session. Entries are NEVER removed. A dismissed notification can never
    /// re-appear even if needs_attention cycles back to true. A new prompt on
    /// the same session has a different signature and passes through normally.
    shown_this_session: HashSet<String>,

    // -- completion badge state --
    complete_first_seen: HashMap<String, u64>,
    complete_promoted: HashMap<String, CompletionEntry>,
    complete_dismissed: HashSet<String>,
}

struct CompletionEntry {
    promoted_at_ms: u64,
    session_title: Option<String>,
    summary: Option<String>,
}

// ── Wire types ────────────────────────────────────────────────────────────────

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
    /// Content fingerprint sent to the frontend so Dismiss can echo it back.
    pub signature: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NotificationPayload {
    pub cards: Vec<NotificationCard>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompletionBadge {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub promoted_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CompletionPayload {
    pub badges: Vec<CompletionBadge>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Content fingerprint for a pending session — captures what the user
/// actually sees, so Dismiss is scoped to this specific prompt.
fn signature(session: &NavigatorSessionPayload) -> String {
    format!(
        "{}|{}|{}",
        session.session_title.as_deref().unwrap_or(""),
        session.tool_name.as_deref().unwrap_or(""),
        session.summary.as_deref().unwrap_or(""),
    )
}

// ── Build functions ───────────────────────────────────────────────────────────

fn build_cards(sessions: &NavigatorSessionsPayload, state: &mut NotifState) -> Vec<NotificationCard> {
    let now = now_ms();
    let pending: Vec<&NavigatorSessionPayload> = sessions
        .sessions
        .iter()
        .filter(|s| s.needs_attention == Some(true))
        .collect();

    // Reset debounce timers for sessions that left pending — so a genuine new
    // pending on the same session re-waits the full 2.5s.
    let pending_ids: HashSet<&str> = pending.iter().map(|s| s.session_id.as_str()).collect();
    state.first_pending_ms.retain(|sid, _| pending_ids.contains(sid.as_str()));

    // `shown_this_session` is intentionally NOT pruned here — it is permanent
    // for the lifetime of this copiwaifu process. See module-level comment.

    pending
        .into_iter()
        .filter_map(|s| {
            // Debounce: stamp first-seen; hide until the session has stayed
            // pending for NOTIF_DEBOUNCE_MS.
            let since = *state
                .first_pending_ms
                .entry(s.session_id.clone())
                .or_insert(now);
            if now.saturating_sub(since) < NOTIF_DEBOUNCE_MS {
                return None; // still settling
            }

            let sig = signature(s);

            // Seen-buffer dedup: if this exact content has been dismissed in
            // this session, never show it again.
            if state.shown_this_session.contains(&sig) {
                return None;
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

fn build_completion_badges(
    sessions: &NavigatorSessionsPayload,
    state: &mut NotifState,
) -> Vec<CompletionBadge> {
    let now = now_ms();

    let complete_ids: HashSet<&str> = sessions
        .sessions
        .iter()
        .filter(|s| s.state == AgentState::Complete && s.needs_attention != Some(true))
        .map(|s| s.session_id.as_str())
        .collect();

    // Reset debounce timer when a session leaves Complete — so re-entering Complete
    // starts a fresh 3s wait before promoting a new badge.
    state.complete_first_seen.retain(|id, _| complete_ids.contains(id.as_str()));

    // Expire promoted entries older than the display window (plus a 1-minute grace).
    // Intentionally NOT retained against complete_ids: once promoted, a badge survives
    // session state transitions (e.g. user starts a new turn) and session TTL eviction.
    state.complete_promoted.retain(|_, entry| {
        now.saturating_sub(entry.promoted_at_ms) < COMPLETION_BADGE_DISPLAY_MS + 60_000
    });

    for session in sessions
        .sessions
        .iter()
        .filter(|s| s.state == AgentState::Complete && s.needs_attention != Some(true))
    {
        use std::collections::hash_map::Entry;
        let since = match state.complete_first_seen.entry(session.session_id.clone()) {
            Entry::Vacant(e) => {
                // Session just (re-)entered Complete — clear any prior dismiss so a
                // fresh badge can appear for this new completion.
                state.complete_dismissed.remove(&session.session_id);
                *e.insert(now)
            }
            Entry::Occupied(e) => *e.get(),
        };

        eprintln!(
            "[completion] session={} since_ms={} delta={}ms promoted={} dismissed={}",
            &session.session_id[..8.min(session.session_id.len())],
            since,
            now.saturating_sub(since),
            state.complete_promoted.contains_key(&session.session_id),
            state.complete_dismissed.contains(&session.session_id),
        );

        if now.saturating_sub(since) >= COMPLETE_DEBOUNCE_MS
            && !state.complete_dismissed.contains(&session.session_id)
        {
            // Use the best available summary: ai_talk_context carries last_meaningful_summary
            // (user's prompt or CC's result text); raw session.summary is often the fallback
            // "等待 claude-code 操作" because CC's Stop hook payload has no result field.
            let badge_summary = session
                .ai_talk_context
                .as_ref()
                .and_then(|ctx| ctx.last_meaningful_summary.as_deref())
                .map(str::to_string)
                .or_else(|| session.summary.clone());
            // Always upsert — refreshes badge content when a session completes multiple turns.
            state.complete_promoted.insert(
                session.session_id.clone(),
                CompletionEntry {
                    promoted_at_ms: now,
                    session_title: session.session_title.clone(),
                    summary: badge_summary,
                },
            );
        }
    }

    let mut badges: Vec<CompletionBadge> = state
        .complete_promoted
        .iter()
        .filter(|(id, entry)| {
            !state.complete_dismissed.contains(*id)
                && now.saturating_sub(entry.promoted_at_ms) < COMPLETION_BADGE_DISPLAY_MS
        })
        .map(|(id, entry)| CompletionBadge {
            session_id: id.clone(),
            session_title: entry.session_title.clone(),
            summary: entry.summary.clone(),
            promoted_at_ms: entry.promoted_at_ms,
        })
        .collect();

    badges.sort_by(|a, b| b.promoted_at_ms.cmp(&a.promoted_at_ms));
    badges
}

// ── Core reconcile ────────────────────────────────────────────────────────────

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

    let (cards, badges) = match notif.0.lock() {
        Ok(mut state) => {
            let cards = build_cards(&sessions, &mut state);
            let badges = build_completion_badges(&sessions, &mut state);
            (cards, badges)
        }
        Err(_) => return,
    };

    let _ = app_handle.emit(NOTIFICATION_CHANGED_EVENT, NotificationPayload { cards: cards.clone() });
    let _ = app_handle.emit(COMPLETION_CHANGED_EVENT, CompletionPayload { badges });

    if enabled && !cards.is_empty() {
        ensure_window(app_handle);
    } else {
        hide_window(app_handle);
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────────

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
    eprintln!("[notif] dismiss: session={} sig_len={}", &session_id[..8.min(session_id.len())], signature.len());
    if let Ok(mut state) = store.0.lock() {
        // Record in the permanent seen buffer — this sig will never generate a
        // card again in this copiwaifu session, regardless of needs_attention.
        state.shown_this_session.insert(signature);
    }
    reconcile(&app_handle);
    Ok(())
}

#[tauri::command]
pub fn get_completions(
    app_handle: AppHandle,
    store: State<'_, NotificationStore>,
) -> Result<CompletionPayload, String> {
    let sessions = app_handle
        .try_state::<super::NavigatorStore>()
        .and_then(|nav| nav.0.lock().ok().map(|state| state.sessions_snapshot()))
        .ok_or_else(|| "navigator unavailable".to_string())?;
    let badges = {
        let mut state = store.0.lock().map_err(|err| err.to_string())?;
        build_completion_badges(&sessions, &mut state)
    };
    Ok(CompletionPayload { badges })
}

#[tauri::command]
pub fn dismiss_completion(
    session_id: String,
    app_handle: AppHandle,
    store: State<'_, NotificationStore>,
) -> Result<(), String> {
    if let Ok(mut state) = store.0.lock() {
        state.complete_dismissed.insert(session_id.clone());
        state.complete_promoted.remove(&session_id);
    }
    reconcile(&app_handle);
    Ok(())
}

// ── Notification window management ───────────────────────────────────────────

fn ensure_window(app_handle: &AppHandle) {
    let app = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(window) = app.get_webview_window(NOTIFICATION_WINDOW_LABEL) {
            // Only call orderFront when truly hidden — repeated orderFront on an
            // NSNonActivatingPanel re-routes macOS keyboard events on each call,
            // producing the "keyboard blocked" symptom.
            if !window.is_visible().unwrap_or(false) {
                let _ = window.show();
            }
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
