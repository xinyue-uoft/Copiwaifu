use std::{fs, path::Path};

use serde_json::{json, Value};

use super::hook_helpers::{
    backup_path, claude_hook_obj, claude_settings_path, cmd_has_marker, codex_config_path,
    codex_hook_obj, codex_hooks_path, copilot_settings_path, gemini_settings_path, hook_command,
    hook_dir, opencode_config_path, opencode_config_path_new, opencode_plugin_path,
    read_json_or_default, runtime_dir, toml_build_notify, toml_remove_notify, toml_upsert_notify,
    write_json, SOURCE_MARKER,
};
use crate::platform;

const COPIWAIFU_HOOK: &str = include_str!("../../../hooks/copiwaifu-hook.js");

// ── Public API ────────────────────────────────────────────────────────────────
//
// Install targets Claude Code and Codex. The remove_* paths for other agents
// are kept so uninstall still scrubs traces left by older multi-agent builds.

pub fn install_hooks() -> Result<(), String> {
    let dir = hook_dir()?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let script = dir.join("copiwaifu-hook.js");
    fs::write(&script, COPIWAIFU_HOOK).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    install_claude_hooks(&script)?;
    install_codex_hooks(&script)?;
    log::info!(
        "[hooks] installed (claude={} events, codex={} events)",
        CLAUDE_EVENTS.len(),
        CODEX_EVENTS.len(),
    );
    Ok(())
}

pub fn uninstall_hooks() -> Result<(), String> {
    remove_claude_hooks()?;
    strip_stale_permission_hook()?;
    remove_copilot_hooks()?;
    remove_codex_hooks()?;
    remove_gemini_hooks()?;
    remove_opencode_plugin()?;

    let dir = hook_dir()?;
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    let _ = fs::remove_file(runtime_dir()?.join("port"));
    let _ = fs::remove_file(platform::fallback_port_file());
    log::info!("[hooks] uninstalled (claude + legacy agent traces scrubbed)");
    Ok(())
}

// ── Claude ────────────────────────────────────────────────────────────────────

const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "Notification",
    // PermissionRequest as a fire-and-forget COMMAND hook: it notifies the pet a
    // permission is pending (→ needs_attention → notification card) and exits 0
    // with NO decision, so CC's own prompt — terminal, or Claude Desktop's
    // --permission-prompt-tool — still handles the actual approval. Claude Desktop
    // local-agent sessions fire PermissionRequest (not Notification) for
    // permissions, so this is the signal the notification needs. NOT a blocking
    // http hook (copiwaifu-hook.js always exits 0 with no decision → no fail-open).
    "PermissionRequest",
];

// ── Migration: strip the OLD blocking PermissionRequest *http* hook ─────────────
// A previous build installed a blocking `PermissionRequest` http hook that parked
// the socket and made allow/deny decisions. This build keeps only the
// fire-and-forget command hook (installed via CLAUDE_EVENTS above). Any leftover
// *http* hook must be removed, else CC POSTs to a route we no longer serve
// (404 → fail-open). The command observe hook is the signal we want — preserve it.

const CLAUDE_PERMISSION_EVENT: &str = "PermissionRequest";
const PERMISSION_PATH: &str = "/permission";

/// Matches ONLY the old blocking http permission hook (localhost `/permission`),
/// never the command observe hook we now install (which must be kept).
fn is_blocking_permission_http_hook(inner_hook: &Value) -> bool {
    inner_hook.get("type").and_then(Value::as_str) == Some("http")
        && inner_hook
            .get("url")
            .and_then(Value::as_str)
            .map(|url| url.contains("127.0.0.1") && url.ends_with(PERMISSION_PATH))
            .unwrap_or(false)
}

pub fn strip_stale_permission_hook() -> Result<(), String> {
    let config = claude_settings_path()?;
    if !config.exists() {
        return Ok(());
    }
    let mut root = read_json_or_default(&config)?;
    let Some(hooks_obj) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    if let Some(arr) = hooks_obj
        .get_mut(CLAUDE_PERMISSION_EVENT)
        .and_then(Value::as_array_mut)
    {
        arr.retain(|outer| {
            outer
                .get("hooks")
                .and_then(Value::as_array)
                .map(|inner| !inner.iter().any(is_blocking_permission_http_hook))
                .unwrap_or(true)
        });
    }
    hooks_obj.retain(|_, value| value.as_array().map(|arr| !arr.is_empty()).unwrap_or(true));
    write_json(&config, &root)
}

fn install_claude_hooks(script: &Path) -> Result<(), String> {
    let config = claude_settings_path()?;
    let mut root = read_json_or_default(&config)?;
    if !root.is_object() {
        root = json!({});
    }

    let hooks_map = root["hooks"]
        .as_object_mut()
        .map(|_| ())
        .unwrap_or_else(|| {
            root["hooks"] = json!({});
        });
    let _ = hooks_map;
    let hooks_obj = root["hooks"]
        .as_object_mut()
        .ok_or("hooks is not an object")?;

    for &event in CLAUDE_EVENTS {
        let cmd = hook_command(script, "claude-code", event);
        upsert_command_hook(hooks_obj, event, claude_hook_obj(&cmd))?;
    }

    write_json(&config, &root)
}

fn upsert_command_hook(
    hooks_obj: &mut serde_json::Map<String, Value>,
    event: &str,
    hook: Value,
) -> Result<(), String> {
    let entries = hooks_obj.entry(event).or_insert_with(|| json!([]));
    if !entries.is_array() {
        *entries = json!([]);
    }
    let arr = entries.as_array_mut().ok_or("not an array")?;

    // Find existing copiwaifu outer entries and update their inner hook in-place.
    // Keep scanning to collapse older duplicate installs into the same command.
    let mut found = false;
    for outer in arr.iter_mut() {
        if let Some(inner) = outer.get_mut("hooks").and_then(Value::as_array_mut) {
            if inner.iter().any(cmd_has_marker) {
                for h in inner.iter_mut() {
                    if cmd_has_marker(h) {
                        *h = hook.clone();
                    }
                }
                found = true;
            }
        }
    }
    if !found {
        arr.push(json!({ "matcher": "", "hooks": [hook] }));
    }
    Ok(())
}

fn remove_claude_hooks() -> Result<(), String> {
    let config = claude_settings_path()?;
    if !config.exists() {
        return Ok(());
    }
    let mut root = read_json_or_default(&config)?;
    let Some(hooks_obj) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    remove_marker_hooks_from_map(hooks_obj);
    write_json(&config, &root)
}

fn remove_marker_hooks_from_map(hooks_obj: &mut serde_json::Map<String, Value>) {
    for entries in hooks_obj.values_mut() {
        let Some(arr) = entries.as_array_mut() else {
            continue;
        };
        arr.retain_mut(|outer| {
            let Some(inner) = outer.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            inner.retain(|h| !cmd_has_marker(h));
            !inner.is_empty()
        });
    }
    hooks_obj.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
}

// ── Copilot (legacy scrub only) ───────────────────────────────────────────────

fn remove_copilot_hooks() -> Result<(), String> {
    let config = copilot_settings_path()?;
    if !config.exists() {
        return Ok(());
    }
    let mut root = read_json_or_default(&config)?;
    let Some(hooks_obj) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    for entries in hooks_obj.values_mut() {
        if let Some(arr) = entries.as_array_mut() {
            arr.retain(|e| e.get("source").and_then(Value::as_str) != Some(SOURCE_MARKER));
        }
    }
    hooks_obj.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    write_json(&config, &root)
}

// ── Codex ─────────────────────────────────────────────────────────────────────

const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
];

fn install_codex_hooks(script: &Path) -> Result<(), String> {
    let config = codex_hooks_path()?;
    let mut root = read_json_or_default(&config)?;
    if !root.is_object() {
        root = json!({});
    }

    let hooks_map = root["hooks"]
        .as_object_mut()
        .map(|_| ())
        .unwrap_or_else(|| {
            root["hooks"] = json!({});
        });
    let _ = hooks_map;
    let hooks_obj = root["hooks"]
        .as_object_mut()
        .ok_or("hooks is not an object")?;

    for &event in CODEX_EVENTS {
        let cmd = hook_command(script, "codex", event);
        upsert_command_hook(hooks_obj, event, codex_hook_obj(&cmd))?;
    }

    write_json(&config, &root)
}

fn remove_codex_hooks() -> Result<(), String> {
    remove_codex_lifecycle_hooks()?;
    remove_codex_legacy_notify()
}

fn remove_codex_lifecycle_hooks() -> Result<(), String> {
    let config = codex_hooks_path()?;
    if !config.exists() {
        return Ok(());
    }
    let mut root = read_json_or_default(&config)?;
    let Some(hooks_obj) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    remove_marker_hooks_from_map(hooks_obj);
    write_json(&config, &root)
}

fn remove_codex_legacy_notify() -> Result<(), String> {
    let config = codex_config_path()?;
    if !config.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(&config).map_err(|e| e.to_string())?;

    // Restore from backup if available
    let backup = backup_path()?;
    if backup.exists() {
        if let Ok(bk) = read_json_or_default(&backup) {
            if let Some(arr) = bk["codex"]["notify"].as_array() {
                let args: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                if !args.is_empty() {
                    let restored = toml_upsert_notify(&content, &toml_build_notify(&args));
                    return fs::write(&config, restored).map_err(|e| e.to_string());
                }
            }
        }
    }

    let cleaned = toml_remove_notify(&content);
    fs::write(&config, cleaned).map_err(|e| e.to_string())
}

// ── Gemini (legacy scrub only) ────────────────────────────────────────────────

fn remove_gemini_hooks() -> Result<(), String> {
    let config = gemini_settings_path()?;
    if !config.exists() {
        return Ok(());
    }
    let mut root = read_json_or_default(&config)?;
    let Some(hooks_obj) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(());
    };

    for entries in hooks_obj.values_mut() {
        let Some(arr) = entries.as_array_mut() else {
            continue;
        };
        arr.retain(|entry| {
            let Some(inner) = entry.get("hooks").and_then(Value::as_array) else {
                return true;
            };
            !inner.iter().any(cmd_has_marker)
        });
    }

    hooks_obj.retain(|_, value| value.as_array().map(|arr| !arr.is_empty()).unwrap_or(true));
    write_json(&config, &root)
}

// ── OpenCode (legacy scrub only) ──────────────────────────────────────────────

fn remove_opencode_plugin() -> Result<(), String> {
    let plugin_path = opencode_plugin_path()?;
    if plugin_path.exists() {
        let _ = fs::remove_file(&plugin_path);
    }

    cleanup_opencode_plugin_registration(&opencode_config_path_new()?)?;
    cleanup_opencode_plugin_registration(&opencode_config_path()?)?;
    Ok(())
}

fn cleanup_opencode_plugin_registration(config_path: &Path) -> Result<(), String> {
    if !config_path.exists() {
        return Ok(());
    }
    let mut root = read_json_or_default(config_path)?;
    let Some(plugins) = root.get_mut("plugin").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    plugins.retain(|entry| {
        !entry
            .as_str()
            .map(|value| value.contains(SOURCE_MARKER))
            .unwrap_or(false)
    });
    if plugins.is_empty() {
        root.as_object_mut().map(|obj| obj.remove("plugin"));
    }
    write_json(config_path, &root)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        codex_hook_obj, hook_command, remove_marker_hooks_from_map, upsert_command_hook,
        CODEX_EVENTS,
    };

    #[test]
    fn codex_hooks_are_merged_without_replacing_user_hooks() {
        let script = std::path::Path::new("/Users/me/.copiwaifu/hooks/copiwaifu-hook.js");
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "python3 user-policy.py" }
                        ]
                    }
                ]
            }
        });
        let hooks = root["hooks"].as_object_mut().expect("hooks object");

        for &event in CODEX_EVENTS {
            let cmd = hook_command(script, "codex", event);
            upsert_command_hook(hooks, event, codex_hook_obj(&cmd)).expect("upsert");
        }

        let pre_tool = root["hooks"]["PreToolUse"].as_array().expect("PreToolUse array");
        assert_eq!(pre_tool.len(), 2);
        assert_eq!(pre_tool[0]["hooks"][0]["command"], "python3 user-policy.py");
        assert_eq!(pre_tool[1]["hooks"][0]["timeout"], 5);
        assert!(pre_tool[1]["hooks"][0]["command"]
            .as_str()
            .expect("command")
            .contains("--agent codex --event PreToolUse"));
    }

    #[test]
    fn codex_hook_upsert_updates_existing_copiwaifu_entry() {
        let mut root = json!({
            "hooks": {
                "Stop": [
                    {
                        "matcher": "",
                        "hooks": [
                            { "type": "command", "command": "node /old/copiwaifu-hook.js --agent codex --event Stop" }
                        ]
                    }
                ]
            }
        });
        let hooks = root["hooks"].as_object_mut().expect("hooks object");

        upsert_command_hook(
            hooks,
            "Stop",
            codex_hook_obj("node /new/copiwaifu-hook.js --agent codex --event Stop"),
        )
        .expect("upsert");

        let stop = root["hooks"]["Stop"].as_array().expect("Stop array");
        assert_eq!(stop.len(), 1);
        assert_eq!(
            stop[0]["hooks"][0]["command"],
            "node /new/copiwaifu-hook.js --agent codex --event Stop"
        );
    }

    #[test]
    fn codex_hook_remove_preserves_user_hooks() {
        let mut root = json!({
            "hooks": {
                "PermissionRequest": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "python3 user-policy.py" },
                            { "type": "command", "command": "node /x/copiwaifu-hook.js --agent codex --event PermissionRequest" }
                        ]
                    }
                ],
                "Stop": [
                    {
                        "matcher": "",
                        "hooks": [
                            { "type": "command", "command": "node /x/copiwaifu-hook.js --agent codex --event Stop" }
                        ]
                    }
                ]
            }
        });
        let hooks = root["hooks"].as_object_mut().expect("hooks object");

        remove_marker_hooks_from_map(hooks);

        assert_eq!(
            root["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
            "python3 user-policy.py"
        );
        assert!(root["hooks"].get("Stop").is_none());
    }
}
