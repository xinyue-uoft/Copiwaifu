// Pet-side PASSIVE notification window + completion badge system.
//
// PERMISSION NOTIFICATIONS
// Shows a card per Claude Code session that has been waiting for a permission
// decision for more than DEBOUNCE_MS. The debounce is the crux: every tool
// call briefly flips a session into `needs_attention` and back (especially
// auto-mode agent sessions), so without it the pet would flash a card — and
// churn its summary — on every command. Only a session that *stays* waiting
// (a genuine prompt the user must act on) survives the debounce.
//
// Makes NO decision — the user resolves in their terminal / Claude. Cards
// auto-dissolve when the reducer clears `needs_attention`; a card can be
// locally Dismissed (muted) without touching CC.
//
// COMPLETION BADGES
// Shows a small "完工啦！" chip at the bottom of the main pet window for up
// to COMPLETION_BADGE_DISPLAY_MS after a session settles into Complete. The
// same 3-second debounce filters spurious Complete spikes from auto-mode
// sub-turns — only a session that stays Complete (genuinely waiting for the
// user's next input) gets a badge. Chips stack per-session and can be
// dismissed early. Each badge carries the CC completion message so the user
// can glance what just finished.
//
// Both features share NotifState (single mutex) and the reconcile() path
// driven by the ~1s polling loop in reconcile.rs.

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
/// A session must remain in Complete state for this long before we show a
/// badge — prevents spurious flashes from rapid auto-mode sub-turn cycling.
const COMPLETE_DEBOUNCE_MS: u64 = 3_000;
/// How long a completion badge stays visible before auto-dissolving.
const COMPLETION_BADGE_DISPLAY_MS: u64 = 300_000; // 5 min

// ── Shared state ──────────────────────────────────────────────────────────────

pub struct NotificationStore(pub Mutex<NotifState>);

#[derive(Default)]
pub struct NotifState {
    // -- permission notification maps --
    /// session_id → dismissed pending-signature (Dismiss mute).
    muted: HashMap<String, String>,
    /// session_id → epoch-ms when first observed pending (notification debounce).
    first_pending_ms: HashMap<String, u64>,

    // -- completion badge maps --
    /// session_id → epoch-ms when first observed Complete (completion debounce).
    complete_first_seen: HashMap<String, u64>,
    /// session_id → entry for sessions promoted to badge-visible.
    complete_promoted: HashMap<String, CompletionEntry>,
    /// session_ids whose badge was manually dismissed (cleared when session exits Complete).
    complete_dismissed: HashSet<String>,
}

struct CompletionEntry {
    promoted_at_ms: u64,
    session_title: Option<String>,
    summary: Option<String>,
}

// ── Wire types (serialised to frontend) ──────────────────────────────────────

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

#[derive(Clone, Debug, Serialize)]
pub struct CompletionBadge {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    /// Last meaningful summary from CC — shown as the completion message.
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

// ── Build functions ───────────────────────────────────────────────────────────

fn build_cards(sessions: &NavigatorSessionsPayload, state: &mut NotifState) -> Vec<NotificationCard> {
    let now = now_ms();
    let pending: Vec<&NavigatorSessionPayload> = sessions
        .sessions
        .iter()
        .filter(|s| s.needs_attention == Some(true))
        .collect();

    // Debounce timers: reset when a session leaves pending, so a genuinely new
    // pending re-starts the 2.5s clock.
    let pending_ids: HashSet<&str> = pending.iter().map(|s| s.session_id.as_str()).collect();
    state.first_pending_ms.retain(|sid, _| pending_ids.contains(sid.as_str()));

    // *** Mutes must survive brief needs_attention fluctuations. ***
    // Intermediate events (PreToolUse, Notification, etc.) can momentarily flip a
    // session out of `needs_attention` for one reconcile tick, then back. If we
    // retain mutes only while the session is in `pending_ids`, a dismiss is
    // silently lost during that window and the card immediately re-appears —
    // cycling every second and blocking keyboard input (repeated orderFront).
    //
    // Fix: retain mutes for any session still present in the navigator state at
    // all. The mute is tied to a content signature, so a genuinely new pending
    // with different content (different tool / summary) naturally bypasses it.
    // Only when the session fully exits the navigator (agent process closed) do
    // we release the mute.
    let all_session_ids: HashSet<&str> =
        sessions.sessions.iter().map(|s| s.session_id.as_str()).collect();
    let before = state.muted.len();
    state.muted.retain(|sid, _| all_session_ids.contains(sid.as_str()));
    let dropped = before.saturating_sub(state.muted.len());
    if dropped > 0 {
        eprintln!("[notif] released {dropped} mute(s) for fully-closed sessions");
    }

    pending
        .into_iter()
        .filter_map(|s| {
            // Debounce: stamp first-seen; hide until it has waited NOTIF_DEBOUNCE_MS.
            let since = *state
                .first_pending_ms
                .entry(s.session_id.clone())
                .or_insert(now);
            if now.saturating_sub(since) < NOTIF_DEBOUNCE_MS {
                return None; // still settling — likely an auto-handled spike
            }

            let sig = signature(s);
            if state.muted.get(&s.session_id) == Some(&sig) {
                return None; // dismissed, same pending — stay hidden
            }
            // If a mute exists but the signature changed, the user is seeing a
            // genuinely new prompt — clear the stale mute so the card shows.
            if state.muted.contains_key(&s.session_id) {
                state.muted.remove(&s.session_id);
                eprintln!("[notif] sig changed for {} — cleared stale mute", &s.session_id[..8.min(s.session_id.len())]);
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

    // Sessions currently settled into Complete (not still waiting for attention).
    let complete_ids: HashSet<&str> = sessions
        .sessions
        .iter()
        .filter(|s| s.state == AgentState::Complete && s.needs_attention != Some(true))
        .map(|s| s.session_id.as_str())
        .collect();

    // Prune all completion maps for sessions that left Complete state.
    // This also clears `complete_dismissed` — so if the user starts a new task
    // and it completes again, they get a fresh badge.
    state.complete_first_seen.retain(|id, _| complete_ids.contains(id.as_str()));
    state.complete_promoted.retain(|id, _| complete_ids.contains(id.as_str()));
    state.complete_dismissed.retain(|id| complete_ids.contains(id.as_str()));

    // Stamp and promote each Complete session.
    for session in sessions
        .sessions
        .iter()
        .filter(|s| s.state == AgentState::Complete && s.needs_attention != Some(true))
    {
        let since = *state
            .complete_first_seen
            .entry(session.session_id.clone())
            .or_insert(now);

        // Promote only once per Complete stint, after the debounce.
        if now.saturating_sub(since) >= COMPLETE_DEBOUNCE_MS
            && !state.complete_promoted.contains_key(&session.session_id)
            && !state.complete_dismissed.contains(&session.session_id)
        {
            state.complete_promoted.insert(
                session.session_id.clone(),
                CompletionEntry {
                    promoted_at_ms: now,
                    session_title: session.session_title.clone(),
                    summary: session.summary.clone(),
                },
            );
        }
    }

    // Collect visible badges: promoted, not dismissed, within the 5-min window.
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

    // Stable order: newest first.
    badges.sort_by(|a, b| b.promoted_at_ms.cmp(&a.promoted_at_ms));
    badges
}

// ── Core reconcile ────────────────────────────────────────────────────────────

/// Recompute visible notification cards and completion badges from current
/// session state. Emits both `notification:changed` and `completion:changed`,
/// then shows/hides the notification OS window. Called from the emit path on
/// every state change (incl. the ~1s polling loops, which drive both
/// debounces), from dismiss commands, and from settings save.
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

    // Acquire the notif lock once — build both outputs in the same critical section.
    let (cards, badges) = match notif.0.lock() {
        Ok(mut state) => {
            let cards = build_cards(&sessions, &mut state);
            let badges = build_completion_badges(&sessions, &mut state);
            (cards, badges)
        }
        Err(_) => return,
    };

    let _ = app_handle.emit(
        NOTIFICATION_CHANGED_EVENT,
        NotificationPayload { cards: cards.clone() },
    );

    let _ = app_handle.emit(
        COMPLETION_CHANGED_EVENT,
        CompletionPayload { badges },
    );

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
    if let Ok(mut state) = store.0.lock() {
        state.muted.insert(session_id, signature);
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
            // Guard: only call orderFront when the window is actually hidden.
            // Repeatedly calling show() / orderFront: on an already-visible
            // NSNonActivatingPanel causes macOS to re-route keyboard events on
            // every reconcile tick (~1s), producing the "keyboard blocked /
            // constantly refocusing" symptom the user observed.
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
