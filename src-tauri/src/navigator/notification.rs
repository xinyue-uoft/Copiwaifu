// Pet-side PASSIVE notification window + completion badge system.
//
// ATTENTION NOTIFICATIONS — the (session_id, epoch) instance model
//
// A "notification" is one concrete request for the user's attention: a
// permission dialog (PermissionRequest / Notification hook) or a choice
// (AskUserQuestion / ExitPlanMode via PreToolUse). The reducer bumps
// `attention_epoch` on every false→true edge of `needs_attention`, so each
// request has a stable identity (session_id, epoch):
//
//   • One popup per instance — a card appears once per epoch, after a short
//     debounce that coalesces Notification + PermissionRequest double-fires.
//   • Dismiss kills exactly that instance, forever. The window hides when no
//     cards remain. A *new* request on the same session gets a new epoch and
//     passes through normally.
//   • Resolve — the user acts inside CC, the next event clears
//     needs_attention, and the card dissolves on its own.
//
// COMPLETION BADGES
// A "完工啦！" chip at the bottom of the pet window for up to 5 minutes after
// a session settles into Complete. Promoted once per completion-run (anchored
// to the debounce stamp), badge lifetime is wall-clock only — it survives
// session TTL eviction and new turns.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use super::events::{
    AgentState, AttentionKind, NavigatorSessionPayload, NavigatorSessionsPayload,
};
use super::short_id;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const NOTIFICATION_WINDOW_LABEL: &str = "notification";
const NOTIFICATION_CHANGED_EVENT: &str = "notification:changed";
/// Coalesces near-simultaneous double-fires (Notification + PermissionRequest
/// for the same dialog) and skips requests resolved within a second.
const ATTN_DEBOUNCE_MS: u64 = 1_000;

pub const COMPLETION_CHANGED_EVENT: &str = "completion:changed";
const COMPLETE_DEBOUNCE_MS: u64 = 3_000;
const COMPLETION_BADGE_DISPLAY_MS: u64 = 300_000; // 5 min

// ── Shared state ──────────────────────────────────────────────────────────────

pub struct NotificationStore(pub Mutex<NotifState>);

#[derive(Default)]
pub struct NotifState {
    /// One track per session currently waiting for attention.
    attention: HashMap<String, AttnTrack>,

    // -- completion badge state --
    complete_first_seen: HashMap<String, u64>,
    complete_promoted: HashMap<String, CompletionEntry>,
    complete_dismissed: std::collections::HashSet<String>,
}

struct AttnTrack {
    epoch: u64,
    first_seen_ms: u64,
    promoted: bool,
    dismissed: bool,
}

struct CompletionEntry {
    /// `complete_first_seen` stamp this entry was promoted from — promotes
    /// exactly once per completion-run instead of refreshing every tick.
    since_ms: u64,
    promoted_at_ms: u64,
    session_title: Option<String>,
    summary: Option<String>,
}

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct NotificationCard {
    pub session_id: String,
    pub agent: String,
    /// Echoed back by Dismiss so it kills exactly this instance.
    pub epoch: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<AttentionKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
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

// ── Attention cards ───────────────────────────────────────────────────────────

fn build_cards(sessions: &NavigatorSessionsPayload, state: &mut NotifState) -> Vec<NotificationCard> {
    let now = now_ms();

    let pending: Vec<&NavigatorSessionPayload> = sessions
        .sessions
        .iter()
        .filter(|s| s.needs_attention == Some(true))
        .collect();

    // Sessions that stopped pending (user acted in CC, session ended/evicted):
    // their instances are over — drop the tracks.
    let pending_ids: std::collections::HashSet<&str> =
        pending.iter().map(|s| s.session_id.as_str()).collect();
    state.attention.retain(|sid, track| {
        let keep = pending_ids.contains(sid.as_str());
        if !keep && track.promoted {
            log::info!(
                "[notif] {} epoch={} {}",
                short_id(sid),
                track.epoch,
                if track.dismissed { "closed" } else { "resolved" },
            );
        }
        keep
    });

    pending
        .into_iter()
        .filter_map(|s| {
            let track = state
                .attention
                .entry(s.session_id.clone())
                .or_insert_with(|| {
                    log::info!(
                        "[notif] {} epoch={} created kind={:?} tool={}",
                        short_id(&s.session_id),
                        s.attention_epoch,
                        s.attention_kind,
                        s.tool_name.as_deref().unwrap_or("-"),
                    );
                    AttnTrack {
                        epoch: s.attention_epoch,
                        first_seen_ms: now,
                        promoted: false,
                        dismissed: false,
                    }
                });

            // A new epoch on the same session is a brand-new request: fresh
            // debounce, dismiss state cleared.
            if track.epoch != s.attention_epoch {
                log::info!(
                    "[notif] {} epoch={} created (replaces epoch={})",
                    short_id(&s.session_id),
                    s.attention_epoch,
                    track.epoch,
                );
                *track = AttnTrack {
                    epoch: s.attention_epoch,
                    first_seen_ms: now,
                    promoted: false,
                    dismissed: false,
                };
            }

            if track.dismissed {
                return None;
            }
            if now.saturating_sub(track.first_seen_ms) < ATTN_DEBOUNCE_MS {
                return None; // still settling
            }

            if !track.promoted {
                track.promoted = true;
                log::info!(
                    "[notif] {} epoch={} promoted",
                    short_id(&s.session_id),
                    track.epoch,
                );
            }

            Some(NotificationCard {
                session_id: s.session_id.clone(),
                agent: s.agent.as_str().to_string(),
                epoch: track.epoch,
                kind: s.attention_kind,
                tool_name: s.tool_name.clone(),
                summary: s.summary.clone(),
                working_directory: s.working_directory.clone(),
                session_title: s.session_title.clone(),
            })
        })
        .collect()
}

// ── Completion badges ─────────────────────────────────────────────────────────

fn build_completion_badges(
    sessions: &NavigatorSessionsPayload,
    state: &mut NotifState,
) -> Vec<CompletionBadge> {
    let now = now_ms();

    let complete_ids: std::collections::HashSet<&str> = sessions
        .sessions
        .iter()
        .filter(|s| s.state == AgentState::Complete && s.needs_attention != Some(true))
        .map(|s| s.session_id.as_str())
        .collect();

    // Reset debounce timer when a session leaves Complete — so re-entering
    // Complete starts a fresh 3s wait before promoting a new badge.
    state
        .complete_first_seen
        .retain(|id, _| complete_ids.contains(id.as_str()));

    // Expire promoted entries past the display window (plus a 1-minute grace).
    // Intentionally NOT retained against complete_ids: once promoted, a badge
    // survives state transitions (new turn) and session TTL eviction.
    state.complete_promoted.retain(|id, entry| {
        let keep = now.saturating_sub(entry.promoted_at_ms) < COMPLETION_BADGE_DISPLAY_MS + 60_000;
        if !keep {
            log::info!("[badge] {} expired", short_id(id));
        }
        keep
    });

    for session in sessions
        .sessions
        .iter()
        .filter(|s| s.state == AgentState::Complete && s.needs_attention != Some(true))
    {
        use std::collections::hash_map::Entry;
        let since = match state.complete_first_seen.entry(session.session_id.clone()) {
            Entry::Vacant(e) => {
                // Session just (re-)entered Complete — clear any prior dismiss
                // so a fresh badge can appear for this new completion.
                state.complete_dismissed.remove(&session.session_id);
                *e.insert(now)
            }
            Entry::Occupied(e) => *e.get(),
        };

        if now.saturating_sub(since) < COMPLETE_DEBOUNCE_MS
            || state.complete_dismissed.contains(&session.session_id)
        {
            continue;
        }

        // Promote once per completion-run (anchored to the debounce stamp).
        let already_promoted = state
            .complete_promoted
            .get(&session.session_id)
            .is_some_and(|entry| entry.since_ms == since);
        if already_promoted {
            continue;
        }

        // Best available summary: ai_talk_context carries the real CC result
        // text; raw session.summary is often the "等待 claude-code 操作"
        // hook fallback.
        let badge_summary = session
            .ai_talk_context
            .as_ref()
            .and_then(|ctx| ctx.last_meaningful_summary.as_deref())
            .map(str::to_string)
            .or_else(|| session.summary.clone());

        log::info!(
            "[badge] {} promoted summary_len={}",
            short_id(&session.session_id),
            badge_summary.as_deref().map(str::len).unwrap_or(0),
        );
        state.complete_promoted.insert(
            session.session_id.clone(),
            CompletionEntry {
                since_ms: since,
                promoted_at_ms: now,
                session_title: session.session_title.clone(),
                summary: badge_summary,
            },
        );
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

    let _ = app_handle.emit(
        NOTIFICATION_CHANGED_EVENT,
        NotificationPayload {
            cards: cards.clone(),
        },
    );
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
    epoch: u64,
    app_handle: AppHandle,
    store: State<'_, NotificationStore>,
) -> Result<(), String> {
    if let Ok(mut state) = store.0.lock() {
        match state.attention.get_mut(&session_id) {
            Some(track) if track.epoch == epoch => {
                track.dismissed = true;
                log::info!("[notif] {} epoch={} dismissed", short_id(&session_id), epoch);
            }
            Some(track) => {
                log::warn!(
                    "[notif] {} dismiss for epoch={} ignored (current epoch={})",
                    short_id(&session_id),
                    epoch,
                    track.epoch,
                );
            }
            None => {
                log::warn!(
                    "[notif] {} dismiss for epoch={} ignored (no active instance)",
                    short_id(&session_id),
                    epoch,
                );
            }
        }
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
        log::info!("[badge] {} dismissed", short_id(&session_id));
    }
    reconcile(&app_handle);
    Ok(())
}

// ── Notification window management ───────────────────────────────────────────

fn ensure_window(app_handle: &AppHandle) {
    let app = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(window) = app.get_webview_window(NOTIFICATION_WINDOW_LABEL) {
            // Only call orderFront when truly hidden — repeated orderFront on
            // an NSNonActivatingPanel re-routes macOS keyboard events on each
            // call, producing the "keyboard blocked" symptom.
            if !window.is_visible().unwrap_or(false) {
                log::info!("[window] notification show");
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
                log::info!("[window] notification created");
                if let Err(err) = crate::platform::elevate_panel(&window) {
                    log::warn!("[window] elevate failed: {err}");
                }
            }
            Err(err) => log::error!("[window] notification build failed: {err}"),
        }
    });
}

fn hide_window(app_handle: &AppHandle) {
    let app = app_handle.clone();
    let _ = app_handle.run_on_main_thread(move || {
        if let Some(window) = app.get_webview_window(NOTIFICATION_WINDOW_LABEL) {
            if window.is_visible().unwrap_or(false) {
                log::info!("[window] notification hide");
                let _ = window.hide();
            }
        }
    });
}
