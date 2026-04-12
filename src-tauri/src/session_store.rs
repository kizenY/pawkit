use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_RECORDS: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionSource {
    Terminal,
    Slack,
    Resume,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub title: String,
    pub working_dir: String,
    pub created_at: i64,
    pub last_active: i64,
    pub source: SessionSource,
    #[serde(default)]
    pub slack_thread_ts: Option<String>,
    #[serde(default)]
    pub total_cost_usd: f64,
}

pub struct SessionStore {
    records: Vec<SessionRecord>,
    path: PathBuf,
}

impl SessionStore {
    /// Load session store from disk, or create empty if missing/corrupt.
    pub fn load() -> Self {
        let path = store_file_path();

        // Try migration from old .last_terminal_session.json
        let old_path = crate::config::get_config_dir().join(".last_terminal_session.json");
        if !path.exists() && old_path.exists() {
            if let Some(store) = migrate_from_old_format(&old_path, &path) {
                return store;
            }
        }

        let records = match std::fs::read_to_string(&path) {
            Ok(s) if !s.trim().is_empty() => {
                serde_json::from_str::<Vec<SessionRecord>>(s.trim()).unwrap_or_else(|e| {
                    plog!("[SessionStore] Failed to parse {}: {}", path.display(), e);
                    Vec::new()
                })
            }
            _ => Vec::new(),
        };

        plog!("[SessionStore] Loaded {} records", records.len());
        SessionStore { records, path }
    }

    /// Write current records to disk.
    pub fn save(&self) {
        match serde_json::to_string_pretty(&self.records) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, &json) {
                    plog!("[SessionStore] Failed to write: {}", e);
                }
            }
            Err(e) => plog!("[SessionStore] Failed to serialize: {}", e),
        }
    }

    /// Insert a new record or update an existing one (matched by session_id).
    /// Enforces MAX_RECORDS by evicting the oldest (by last_active).
    pub fn upsert(&mut self, record: SessionRecord) {
        if let Some(existing) = self.records.iter_mut().find(|r| r.session_id == record.session_id) {
            existing.last_active = record.last_active;
            if !record.title.is_empty() && existing.title != record.title {
                existing.title = record.title;
            }
            if !record.working_dir.is_empty() && existing.working_dir.is_empty() {
                existing.working_dir = record.working_dir;
            }
            if record.slack_thread_ts.is_some() {
                existing.slack_thread_ts = record.slack_thread_ts;
            }
            existing.total_cost_usd += record.total_cost_usd;
        } else {
            self.records.push(record);
            // Evict oldest if over capacity
            if self.records.len() > MAX_RECORDS {
                self.records.sort_by(|a, b| b.last_active.cmp(&a.last_active));
                self.records.truncate(MAX_RECORDS);
            }
        }
        self.save();
    }

    /// Update last_active timestamp for a session.
    pub fn touch(&mut self, session_id: &str) {
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(r) = self.records.iter_mut().find(|r| r.session_id == session_id) {
            r.last_active = now;
        }
        // Don't save on every touch — caller can batch saves
    }

    /// Save only if called explicitly (for batched touch operations).
    pub fn touch_and_save(&mut self, session_id: &str) {
        self.touch(session_id);
        self.save();
    }

    /// Get recent sessions sorted by last_active descending.
    pub fn recent(&self, limit: usize) -> Vec<&SessionRecord> {
        let mut sorted: Vec<&SessionRecord> = self.records.iter().collect();
        sorted.sort_by(|a, b| b.last_active.cmp(&a.last_active));
        sorted.truncate(limit);
        sorted
    }

    /// Find a session by ID.
    pub fn by_id(&self, session_id: &str) -> Option<&SessionRecord> {
        self.records.iter().find(|r| r.session_id == session_id)
    }

    /// Remove a session by ID.
    pub fn remove(&mut self, session_id: &str) {
        self.records.retain(|r| r.session_id != session_id);
        self.save();
    }

    /// Update the title for a session.
    pub fn set_title(&mut self, session_id: &str, title: &str) {
        if let Some(r) = self.records.iter_mut().find(|r| r.session_id == session_id) {
            r.title = title.to_string();
            self.save();
        }
    }

    /// Update the working directory for a session.
    pub fn set_working_dir(&mut self, session_id: &str, working_dir: &str) {
        if let Some(r) = self.records.iter_mut().find(|r| r.session_id == session_id) {
            r.working_dir = working_dir.to_string();
            self.save();
        }
    }

    /// Add cost to a session's running total.
    pub fn add_cost(&mut self, session_id: &str, cost: f64) {
        if let Some(r) = self.records.iter_mut().find(|r| r.session_id == session_id) {
            r.total_cost_usd += cost;
        }
    }
}

/// Canonical store file path.
fn store_file_path() -> PathBuf {
    crate::config::get_config_dir().join(".sessions.json")
}

/// Migrate from old single-session format to new store.
fn migrate_from_old_format(old_path: &PathBuf, new_path: &PathBuf) -> Option<SessionStore> {
    #[derive(Deserialize)]
    struct OldSession {
        session_id: String,
        working_dir: String,
    }

    let content = std::fs::read_to_string(old_path).ok()?;
    let old: OldSession = serde_json::from_str(content.trim()).ok()?;

    let now = chrono::Utc::now().timestamp_millis();
    let title = generate_title(&old.session_id);
    let record = SessionRecord {
        session_id: old.session_id,
        title,
        working_dir: old.working_dir,
        created_at: now,
        last_active: now,
        source: SessionSource::Terminal,
        slack_thread_ts: None,
        total_cost_usd: 0.0,
    };

    let store = SessionStore {
        records: vec![record],
        path: new_path.clone(),
    };
    store.save();

    // Remove old file
    let _ = std::fs::remove_file(old_path);
    plog!("[SessionStore] Migrated from old .last_terminal_session.json");

    Some(store)
}

/// Generate a human-readable title from a Claude Code session's first user prompt.
/// Scans ~/.claude/projects/{slug}/{session_id}.jsonl for the first user message.
pub fn generate_title(session_id: &str) -> String {
    if let Some(prompt) = read_first_user_prompt(session_id) {
        return clean_title(&prompt);
    }
    // Fallback: short session ID
    format!("Session {}", &session_id[..session_id.len().min(8)])
}

/// Read the first user prompt text from a session's JSONL file.
pub(crate) fn read_first_user_prompt(session_id: &str) -> Option<String> {
    let claude_dir = dirs::home_dir()?.join(".claude").join("projects");
    if !claude_dir.exists() {
        return None;
    }

    let filename = format!("{}.jsonl", session_id);
    for entry in std::fs::read_dir(&claude_dir).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_dir() {
            continue;
        }
        let session_file = entry.path().join(&filename);
        if !session_file.exists() {
            continue;
        }

        // Read line by line looking for first user message
        let content = std::fs::read_to_string(&session_file).ok()?;
        for line in content.lines().take(50) {
            let json: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if json.get("type").and_then(|v| v.as_str()) != Some("user") {
                continue;
            }

            // Extract text from message.content
            if let Some(content) = json.get("message")
                .and_then(|m| m.get("content"))
            {
                // content can be a string or an array of content blocks
                if let Some(text) = content.as_str() {
                    return Some(text.to_string());
                }
                if let Some(arr) = content.as_array() {
                    for block in arr {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Clean a raw prompt into a short title.
fn clean_title(prompt: &str) -> String {
    let text = prompt.trim();

    // Handle <command-name>...</command-name> patterns (slash commands)
    if text.starts_with("<command-name>") {
        if let Some(end) = text.find("</command-name>") {
            let cmd = &text[14..end];
            // Also check for command-args
            if let Some(args_start) = text.find("<command-args>") {
                if let Some(args_end) = text.find("</command-args>") {
                    let args = text[args_start + 14..args_end].trim();
                    if !args.is_empty() {
                        let combined = format!("/{} {}", cmd, args);
                        return truncate_title(&combined, 50);
                    }
                }
            }
            return format!("/{}", cmd);
        }
    }

    // Try each line to find a meaningful (non-path) title
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip lines that look like file paths
        if looks_like_path(line) {
            continue;
        }
        // Skip lines that look like XML/HTML tags
        if line.starts_with('<') && line.contains('>') {
            continue;
        }
        // Found a good line
        return truncate_title(line, 50);
    }

    // All lines were paths/tags — try to extract meaningful name from first path
    let first_line = text.lines().next().unwrap_or(text).trim();
    if looks_like_path(first_line) {
        return truncate_title(&extract_name_from_path(first_line), 50);
    }

    truncate_title(first_line, 50)
}

/// Check if a string looks like a file path.
fn looks_like_path(s: &str) -> bool {
    let s = s.trim();
    // Windows absolute path: C:\ D:\
    if s.len() >= 3 && s.as_bytes()[1] == b':' && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/') {
        return true;
    }
    // Unix absolute path
    if s.starts_with('/') && !s.starts_with("//") && s.contains('/') && s.len() > 3 {
        return true;
    }
    // Relative paths with many separators (e.g., src/foo/bar/baz.rs)
    let sep_count = s.chars().filter(|&c| c == '/' || c == '\\').count();
    if sep_count >= 3 && !s.contains(' ') {
        return true;
    }
    false
}

/// Extract a human-readable name from a file path.
fn extract_name_from_path(path: &str) -> String {
    let path = path.trim().trim_end_matches(['/', '\\']);
    // Get the last component
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    // Remove common extensions
    let name = name.strip_suffix(".jsonl")
        .or_else(|| name.strip_suffix(".json"))
        .or_else(|| name.strip_suffix(".txt"))
        .or_else(|| name.strip_suffix(".md"))
        .unwrap_or(name);
    name.to_string()
}

fn truncate_title(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{}...", truncated)
    }
}

/// Resolve working directory for a session from Claude Code's JSONL files.
/// Reused from the old hook_server logic.
pub fn resolve_session_working_dir(session_id: &str) -> Option<String> {
    let claude_dir = dirs::home_dir()?.join(".claude").join("projects");
    if !claude_dir.exists() {
        return None;
    }

    let filename = format!("{}.jsonl", session_id);
    for entry in std::fs::read_dir(&claude_dir).ok()? {
        let entry = entry.ok()?;
        if entry.file_type().ok()?.is_dir() {
            let session_file = entry.path().join(&filename);
            if session_file.exists() {
                if let Ok(content) = std::fs::read_to_string(&session_file) {
                    for line in content.lines().take(10) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                            if let Some(cwd) = json.get("cwd").and_then(|v| v.as_str()) {
                                return Some(cwd.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}
