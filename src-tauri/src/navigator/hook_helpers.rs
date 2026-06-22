use std::{fs, path::PathBuf};

use serde_json::{json, Value};

use crate::platform;

pub const SOURCE_MARKER: &str = "copiwaifu";

// ── Path helpers ──────────────────────────────────────────────────────────────

pub fn home_dir() -> Result<PathBuf, String> {
    platform::home_dir_result()
}

pub fn runtime_dir() -> Result<PathBuf, String> {
    platform::runtime_dir()
}

pub fn hook_dir() -> Result<PathBuf, String> {
    Ok(runtime_dir()?.join("hooks"))
}

pub fn claude_settings_path() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".claude").join("settings.json"))
}

pub fn copilot_settings_path() -> Result<PathBuf, String> {
    Ok(home_dir()?
        .join(".config")
        .join("github-copilot")
        .join("config.json"))
}

pub fn codex_config_path() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".codex").join("config.toml"))
}

pub fn gemini_settings_path() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".gemini").join("settings.json"))
}

pub fn opencode_plugin_dir() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".config").join("opencode").join("plugins"))
}

pub fn opencode_plugin_path() -> Result<PathBuf, String> {
    Ok(opencode_plugin_dir()?.join("copiwaifu.js"))
}

pub fn opencode_config_path() -> Result<PathBuf, String> {
    Ok(home_dir()?
        .join(".config")
        .join("opencode")
        .join("config.json"))
}

pub fn opencode_config_path_new() -> Result<PathBuf, String> {
    Ok(home_dir()?
        .join(".config")
        .join("opencode")
        .join("opencode.json"))
}

pub fn backup_path() -> Result<PathBuf, String> {
    Ok(hook_dir()?.join("original-hooks.json"))
}

// ── JSON helpers ──────────────────────────────────────────────────────────────

pub fn read_json_or_default(path: &std::path::Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({}));
    }
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

pub fn write_json(path: &std::path::Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    fs::write(path, body).map_err(|e| e.to_string())
}

// ── OpenCode plugin registration ──────────────────────────────────────────────

/// Upsert the copiwaifu plugin path in an opencode config's `plugin` array.
/// Inverse of `cleanup_opencode_plugin_registration` (hook_installer.rs):
/// inserts or refreshes the absolute plugin path, which carries SOURCE_MARKER
/// so cleanup can find and remove it later.
pub fn register_opencode_plugin(
    config_path: &std::path::Path,
    plugin_abs_path: &str,
) -> Result<(), String> {
    let mut root = read_json_or_default(config_path)?;
    if !root.is_object() {
        root = json!({});
    }
    if root
        .get_mut("plugin")
        .and_then(Value::as_array_mut)
        .is_none()
    {
        root["plugin"] = json!([]);
    }
    let arr = root["plugin"]
        .as_array_mut()
        .ok_or("plugin is not an array")?;

    let mut found = false;
    for entry in arr.iter_mut() {
        if entry
            .as_str()
            .map(|v| v.contains(SOURCE_MARKER))
            .unwrap_or(false)
        {
            *entry = json!(plugin_abs_path);
            found = true;
            break;
        }
    }
    if !found {
        arr.push(json!(plugin_abs_path));
    }
    write_json(config_path, &root)
}

// ── Command builders ──────────────────────────────────────────────────────────

pub fn hook_command(script: &std::path::Path, agent: &str, event: &str) -> String {
    format!(
        "node \"{}\" --agent {} --event {}",
        script.display(),
        agent,
        event
    )
}

// ── Claude-specific helpers ───────────────────────────────────────────────────

pub fn claude_hook_obj(command: &str) -> Value {
    json!({ "type": "command", "command": command })
}

pub fn cmd_has_marker(v: &Value) -> bool {
    v.get("command")
        .and_then(Value::as_str)
        .map(|c| c.contains(SOURCE_MARKER))
        .unwrap_or(false)
}

// ── TOML helpers (no toml crate) ──────────────────────────────────────────────

/// Find the top-level notify value span, handling both single-line and multi-line arrays.
/// Returns `Some((start_line_idx, end_line_idx))` (inclusive).
fn toml_notify_span(lines: &[&str]) -> Option<(usize, usize)> {
    let mut start = None;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            break;
        }
        if trimmed.starts_with("notify") {
            start = Some(idx);
            break;
        }
    }
    let start = start?;
    let first = lines[start];
    // Single-line: notify = [...] on one line
    if first.contains('[') && first.contains(']') {
        return Some((start, start));
    }
    // Multi-line: find the closing ']'
    for (i, line) in lines.iter().enumerate().skip(start) {
        if line.contains(']') {
            return Some((start, i));
        }
    }
    // Unclosed bracket — treat as single line to avoid eating the whole file
    Some((start, start))
}

/// Extract the full notify value text (may span multiple lines).
#[cfg(test)]
pub fn toml_find_notify(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let (start, end) = toml_notify_span(&lines)?;
    Some(lines[start..=end].join("\n"))
}

pub fn toml_build_notify(args: &[String]) -> String {
    let items: Vec<String> = args
        .iter()
        .map(|a| serde_json::to_string(a).unwrap_or_else(|_| format!("{a:?}")))
        .collect();
    format!("notify = [{}]", items.join(", "))
}

pub fn toml_upsert_notify(content: &str, new_line: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if let Some((start, end)) = toml_notify_span(&lines) {
        let mut result: Vec<String> = Vec::with_capacity(lines.len());
        result.extend(lines[..start].iter().map(|l| l.to_string()));
        result.push(new_line.to_string());
        result.extend(lines[end + 1..].iter().map(|l| l.to_string()));
        result.join("\n")
    } else if content.is_empty() {
        new_line.to_string()
    } else {
        let insert_at = lines
            .iter()
            .position(|line| line.trim_start().starts_with('['))
            .unwrap_or(lines.len());
        let mut result: Vec<String> = Vec::with_capacity(lines.len() + 1);
        result.extend(lines[..insert_at].iter().map(|l| l.to_string()));
        result.push(new_line.to_string());
        result.extend(lines[insert_at..].iter().map(|l| l.to_string()));
        result.join("\n")
    }
}

pub fn toml_remove_notify(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if let Some((start, end)) = toml_notify_span(&lines) {
        let mut result: Vec<String> = Vec::with_capacity(lines.len());
        result.extend(lines[..start].iter().map(|l| l.to_string()));
        result.extend(lines[end + 1..].iter().map(|l| l.to_string()));
        result.join("\n")
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{toml_build_notify, toml_find_notify, toml_remove_notify, toml_upsert_notify};

    #[test]
    fn upsert_notify_inserts_before_first_table() {
        let content = "model = \"gpt-5\"\n\n[projects.\"/tmp\"]\ntrust_level = \"trusted\"";
        let updated = toml_upsert_notify(content, "notify = [\"node\", \"hook.js\"]");

        assert_eq!(
            updated,
            "model = \"gpt-5\"\n\nnotify = [\"node\", \"hook.js\"]\n[projects.\"/tmp\"]\ntrust_level = \"trusted\""
        );
    }

    #[test]
    fn notify_helpers_ignore_nested_notify_keys() {
        let content = "[profiles.default]\nnotify = [\"nested\"]\n";

        assert!(toml_find_notify(content).is_none());
        assert_eq!(toml_remove_notify(content), content);
        assert_eq!(
            toml_upsert_notify(content, "notify = [\"node\", \"hook.js\"]"),
            "notify = [\"node\", \"hook.js\"]\n[profiles.default]\nnotify = [\"nested\"]"
        );
    }

    #[test]
    fn build_notify_escapes_windows_paths() {
        let line = toml_build_notify(&[
            "node".to_string(),
            r"C:\Users\name\.copiwaifu\hooks\copiwaifu-hook.js".to_string(),
        ]);

        assert_eq!(
            line,
            r#"notify = ["node", "C:\\Users\\name\\.copiwaifu\\hooks\\copiwaifu-hook.js"]"#
        );
    }
}

