#[allow(unused_imports)]
use crate::plog;
use crate::claude_session::{ClaudeOutput, ClaudeSession};
use crate::config::SlackConfig;
use crate::hook_server::{ActiveSessions, AuthDecision, PendingRequests};
use crate::session_store::{SessionRecord, SessionSource, SessionStore};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Maps Claude session_id → Slack thread_ts for routing hook notifications to the correct thread.
pub type SessionThreadMap = Arc<Mutex<HashMap<String, String>>>;

/// Slack API client for DM communication via Socket Mode
#[derive(Clone)]
pub struct SlackBridge {
    client: Client,
    bot_token: String,
    app_token: String,
    dm_user_id: String,
    dm_channel_id: Arc<Mutex<String>>,
    bot_user_id: Arc<Mutex<String>>,
    /// Current active thread ts — all messages go here
    active_thread_ts: Arc<Mutex<String>>,
}

impl SlackBridge {
    pub fn new(bot_token: String, app_token: String, dm_user_id: String) -> Self {
        Self {
            client: Client::new(),
            bot_token,
            app_token,
            dm_user_id,
            dm_channel_id: Arc::new(Mutex::new(String::new())),
            bot_user_id: Arc::new(Mutex::new(String::new())),
            active_thread_ts: Arc::new(Mutex::new(String::new())),
        }
    }

    pub async fn init(&self) -> Result<(), String> {
        let data = self.api_post("auth.test", &serde_json::json!({})).await?;
        let user_id = data["user_id"].as_str().unwrap_or("").to_string();
        plog!("[Pawkit] Slack bot user ID: {}", user_id);
        *self.bot_user_id.lock().await = user_id;

        let data = self
            .api_post("conversations.open", &serde_json::json!({ "users": self.dm_user_id }))
            .await?;
        let channel_id = data["channel"]["id"].as_str().unwrap_or("").to_string();
        if channel_id.is_empty() {
            return Err("conversations.open returned empty channel ID".to_string());
        }
        plog!("[Pawkit] DM channel ID: {}", channel_id);
        *self.dm_channel_id.lock().await = channel_id;
        Ok(())
    }

    pub async fn connect_socket(&self) -> Result<String, String> {
        let resp = self
            .client
            .post("https://slack.com/api/apps.connections.open")
            .bearer_auth(&self.app_token)
            .send()
            .await
            .map_err(|e| format!("apps.connections.open failed: {}", e))?;
        let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if data["ok"].as_bool() != Some(true) {
            return Err(format!("apps.connections.open error: {}", data["error"]));
        }
        data["url"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "No WebSocket URL in response".to_string())
    }

    /// Post a top-level message (starts a new thread)
    pub async fn post_top_message(&self, text: &str) -> Result<String, String> {
        let channel = self.dm_channel_id.lock().await.clone();
        let data = self
            .api_post("chat.postMessage", &serde_json::json!({ "channel": channel, "text": text }))
            .await?;
        let ts = data["ts"].as_str().unwrap_or("").to_string();
        // This becomes the active thread
        *self.active_thread_ts.lock().await = ts.clone();
        Ok(ts)
    }

    /// Post a message as a reply in the active thread
    pub async fn reply(&self, text: &str) -> Result<String, String> {
        let channel = self.dm_channel_id.lock().await.clone();
        let thread_ts = self.active_thread_ts.lock().await.clone();
        if thread_ts.is_empty() {
            // No active thread — post as top-level
            return self.post_top_message(text).await;
        }
        let data = self
            .api_post(
                "chat.postMessage",
                &serde_json::json!({
                    "channel": channel,
                    "text": text,
                    "thread_ts": thread_ts,
                }),
            )
            .await?;
        Ok(data["ts"].as_str().unwrap_or("").to_string())
    }

    /// Post auth request with buttons in the active thread
    pub async fn post_auth_buttons(&self, tool_name: &str, summary: &str, request_id: &str) -> Result<String, String> {
        let channel = self.dm_channel_id.lock().await.clone();
        let thread_ts = self.active_thread_ts.lock().await.clone();

        let text = format!("🔒 权限请求: {} — {}", tool_name, summary);
        let blocks = serde_json::json!([
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("🔒 *权限请求: {}*\n```{}```", tool_name, summary)
                }
            },
            {
                "type": "actions",
                "block_id": format!("auth_{}", request_id),
                "elements": [
                    {
                        "type": "button",
                        "text": { "type": "plain_text", "text": "✅ Allow" },
                        "action_id": "auth_allow",
                        "style": "primary",
                        "value": request_id
                    },
                    {
                        "type": "button",
                        "text": { "type": "plain_text", "text": "❌ Deny" },
                        "action_id": "auth_deny",
                        "style": "danger",
                        "value": request_id
                    }
                ]
            }
        ]);

        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
            "blocks": blocks,
        });
        if !thread_ts.is_empty() {
            body["thread_ts"] = serde_json::Value::String(thread_ts);
        }

        let data = self.api_post("chat.postMessage", &body).await?;
        Ok(data["ts"].as_str().unwrap_or("").to_string())
    }

    /// Set typing/thinking status via Slack's Agents & Assistants API.
    /// Requires `assistant:write` scope. Best-effort — silently fails if not available.
    pub async fn set_status(&self, status: &str) -> Result<(), String> {
        let channel = self.dm_channel_id.lock().await.clone();
        let thread_ts = self.active_thread_ts.lock().await.clone();
        if thread_ts.is_empty() {
            return Ok(());
        }
        let _ = self.api_post("assistant.threads.setStatus", &serde_json::json!({
            "channel_id": channel,
            "thread_ts": thread_ts,
            "status": status,
        })).await;
        Ok(())
    }

    /// Clear the typing/thinking status
    pub async fn clear_status(&self) -> Result<(), String> {
        self.set_status("").await
    }

    /// Update a message in-place (e.g. to resolve auth buttons)
    pub async fn update_message(&self, channel: &str, ts: &str, text: &str, blocks: &serde_json::Value) -> Result<(), String> {
        self.api_post("chat.update", &serde_json::json!({
            "channel": channel,
            "ts": ts,
            "text": text,
            "blocks": blocks,
        })).await?;
        Ok(())
    }

    pub async fn set_active_thread(&self, ts: &str) {
        *self.active_thread_ts.lock().await = ts.to_string();
    }

    #[allow(dead_code)]
    pub async fn get_active_thread(&self) -> String {
        self.active_thread_ts.lock().await.clone()
    }

    /// Reply in a specific thread (bypasses active_thread_ts)
    pub async fn reply_in_thread(&self, thread_ts: &str, text: &str) -> Result<String, String> {
        let channel = self.dm_channel_id.lock().await.clone();
        if thread_ts.is_empty() {
            return self.post_top_message(text).await;
        }
        let data = self
            .api_post(
                "chat.postMessage",
                &serde_json::json!({
                    "channel": channel,
                    "text": text,
                    "thread_ts": thread_ts,
                }),
            )
            .await?;
        Ok(data["ts"].as_str().unwrap_or("").to_string())
    }

    /// Post auth buttons in a specific thread (bypasses active_thread_ts)
    pub async fn post_auth_buttons_in_thread(
        &self,
        thread_ts: &str,
        tool_name: &str,
        summary: &str,
        request_id: &str,
    ) -> Result<String, String> {
        let channel = self.dm_channel_id.lock().await.clone();
        let text = format!("🔒 权限请求: {} — {}", tool_name, summary);
        let blocks = serde_json::json!([
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("🔒 *权限请求: {}*\n```{}```", tool_name, summary)
                }
            },
            {
                "type": "actions",
                "block_id": format!("auth_{}", request_id),
                "elements": [
                    {
                        "type": "button",
                        "text": { "type": "plain_text", "text": "✅ Allow" },
                        "action_id": "auth_allow",
                        "style": "primary",
                        "value": request_id
                    },
                    {
                        "type": "button",
                        "text": { "type": "plain_text", "text": "❌ Deny" },
                        "action_id": "auth_deny",
                        "style": "danger",
                        "value": request_id
                    }
                ]
            }
        ]);
        let body = serde_json::json!({
            "channel": channel,
            "text": text,
            "blocks": blocks,
            "thread_ts": thread_ts,
        });
        let data = self.api_post("chat.postMessage", &body).await?;
        Ok(data["ts"].as_str().unwrap_or("").to_string())
    }

    /// Set typing status in a specific thread
    pub async fn set_status_in_thread(&self, thread_ts: &str, status: &str) -> Result<(), String> {
        let channel = self.dm_channel_id.lock().await.clone();
        if thread_ts.is_empty() {
            return Ok(());
        }
        let _ = self
            .api_post(
                "assistant.threads.setStatus",
                &serde_json::json!({
                    "channel_id": channel,
                    "thread_ts": thread_ts,
                    "status": status,
                }),
            )
            .await;
        Ok(())
    }

    /// Post a message to any channel (not just DM), optionally in a thread
    pub async fn post_in_channel(&self, channel: &str, thread_ts: Option<&str>, text: &str) -> Result<String, String> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }
        let data = self.api_post("chat.postMessage", &body).await?;
        Ok(data["ts"].as_str().unwrap_or("").to_string())
    }

    /// Fetch recent messages from a thread for context
    pub async fn fetch_thread_messages(&self, channel: &str, thread_ts: &str, limit: usize) -> Result<Vec<serde_json::Value>, String> {
        let data = self.api_post_get(
            &format!("conversations.replies?channel={}&ts={}&limit={}&inclusive=true", channel, thread_ts, limit),
        ).await?;
        Ok(data["messages"].as_array().cloned().unwrap_or_default())
    }

    /// Fetch recent channel messages for context (when mention is not in a thread)
    pub async fn fetch_channel_messages(&self, channel: &str, limit: usize) -> Result<Vec<serde_json::Value>, String> {
        let data = self.api_post_get(
            &format!("conversations.history?channel={}&limit={}", channel, limit),
        ).await?;
        Ok(data["messages"].as_array().cloned().unwrap_or_default())
    }

    /// Look up a user's display name by ID
    pub async fn get_user_name(&self, user_id: &str) -> Result<String, String> {
        let data = self.api_post_get(&format!("users.info?user={}", user_id)).await?;
        let name = data["user"]["profile"]["display_name"].as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| data["user"]["profile"]["real_name"].as_str())
            .or_else(|| data["user"]["name"].as_str())
            .unwrap_or(user_id)
            .to_string();
        Ok(name)
    }

    /// GET-style Slack API call (for endpoints that use query params)
    async fn api_post_get(&self, endpoint: &str) -> Result<serde_json::Value, String> {
        let url = format!("https://slack.com/api/{}", endpoint);
        let resp = self.client.get(&url).bearer_auth(&self.bot_token).send().await
            .map_err(|e| format!("Slack API GET {} failed: {}", endpoint, e))?;
        let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if data["ok"].as_bool() != Some(true) {
            return Err(format!("Slack API {} error: {}", endpoint, data["error"]));
        }
        Ok(data)
    }

    pub fn get_bot_user_id(&self) -> Arc<Mutex<String>> {
        self.bot_user_id.clone()
    }

    pub fn get_dm_channel_id(&self) -> Arc<Mutex<String>> {
        self.dm_channel_id.clone()
    }

    /// Public wrapper for api_post — used by mention_monitor
    pub async fn api_post_public(&self, method: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        self.api_post(method, body).await
    }

    async fn api_post(&self, method: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        let url = format!("https://slack.com/api/{}", method);
        let resp = self.client.post(&url).bearer_auth(&self.bot_token).json(body).send().await
            .map_err(|e| format!("Slack API {} failed: {}", method, e))?;
        let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        if data["ok"].as_bool() != Some(true) {
            return Err(format!("Slack API {} error: {}", method, data["error"]));
        }
        Ok(data)
    }
}

// ── Helpers ──

async fn post_claude_output_in_thread(slack: &SlackBridge, thread_ts: &str, output: &ClaudeOutput) {
    let text = &output.text;
    if text.is_empty() {
        let _ = slack.reply_in_thread(thread_ts, "_(无输出)_").await;
        return;
    }

    let max_len = 3000;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_len {
        let _ = slack.reply_in_thread(thread_ts, text).await;
    } else {
        let total = (chars.len() + max_len - 1) / max_len;
        for (i, chunk) in chars.chunks(max_len).enumerate() {
            let chunk_text: String = chunk.iter().collect();
            let _ = slack.reply_in_thread(thread_ts, &format!("_({}/{})_\n{}", i + 1, total, chunk_text)).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    if let Some(cost) = output.cost_usd {
        if let Some(dur) = output.duration_ms {
            let _ = slack.reply_in_thread(thread_ts, &format!("💰 ${:.4} ⏱ {:.1}s", cost, dur as f64 / 1000.0)).await;
        } else {
            let _ = slack.reply_in_thread(thread_ts, &format!("💰 ${:.4}", cost)).await;
        }
    }
}

async fn resolve_first_pending(pending: &PendingRequests, allow: bool) -> bool {
    let mut pending = pending.lock().await;
    if let Some(key) = pending.keys().next().cloned() {
        if let Some(tx) = pending.remove(&key) {
            let decision = if allow { AuthDecision::Allow } else { AuthDecision::Deny };
            let _ = tx.send(decision);
            return true;
        }
    }
    false
}

async fn has_pending_auth(pending: &PendingRequests) -> bool {
    !pending.lock().await.is_empty()
}

/// Incoming user message with thread context
struct UserMessage {
    text: String,
    /// None = top-level message (new thread), Some = reply in existing thread
    thread_ts: Option<String>,
    /// The message's own ts (used as thread parent for top-level messages)
    msg_ts: String,
}

/// Extract user message + thread info from a Socket Mode event.
/// Handles both plain text and Slack's rich_text blocks.
fn extract_user_message(envelope: &serde_json::Value, dm_user_id: &str, dm_channel_id: &str) -> Option<UserMessage> {
    let payload = envelope.get("payload")?;
    let event = payload.get("event")?;

    if event.get("type")?.as_str()? != "message" { return None; }
    if event.get("subtype").is_some() { return None; }
    if event.get("bot_id").is_some() { return None; }
    if event.get("user")?.as_str()? != dm_user_id { return None; }
    if event.get("channel")?.as_str()? != dm_channel_id { return None; }

    // Try `text` field first, then fall back to extracting from rich_text blocks
    let mut text = event.get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    // If text is empty or suspiciously short, try extracting from blocks
    if text.is_empty() {
        if let Some(blocks_text) = extract_text_from_blocks(event) {
            text = blocks_text;
        }
    }

    if text.is_empty() { return None; }

    let thread_ts = event.get("thread_ts").and_then(|v| v.as_str()).map(String::from);
    let msg_ts = event.get("ts").and_then(|v| v.as_str()).unwrap_or("").to_string();

    Some(UserMessage { text, thread_ts, msg_ts })
}

/// Extract plain text from Slack's rich_text blocks.
/// Handles the new Slack editor's block format where content is in
/// blocks[].elements[].elements[].text instead of the top-level text field.
fn extract_text_from_blocks(event: &serde_json::Value) -> Option<String> {
    let blocks = event.get("blocks")?.as_array()?;
    let mut parts: Vec<String> = Vec::new();

    for block in blocks {
        let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match block_type {
            "rich_text" => {
                if let Some(elements) = block.get("elements").and_then(|v| v.as_array()) {
                    for section in elements {
                        let section_type = section.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match section_type {
                            "rich_text_section" | "rich_text_preformatted" | "rich_text_quote" => {
                                if let Some(inners) = section.get("elements").and_then(|v| v.as_array()) {
                                    for elem in inners {
                                        let elem_type = elem.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                        match elem_type {
                                            "text" => {
                                                if let Some(t) = elem.get("text").and_then(|v| v.as_str()) {
                                                    parts.push(t.to_string());
                                                }
                                            }
                                            "emoji" => {
                                                // Convert Slack emoji to :name: format
                                                if let Some(name) = elem.get("name").and_then(|v| v.as_str()) {
                                                    parts.push(format!(":{}:", name));
                                                }
                                            }
                                            "link" => {
                                                let url = elem.get("url").and_then(|v| v.as_str()).unwrap_or("");
                                                let text = elem.get("text").and_then(|v| v.as_str()).unwrap_or(url);
                                                parts.push(text.to_string());
                                            }
                                            "user" | "usergroup" | "channel" => {
                                                if let Some(id) = elem.get("user_id")
                                                    .or_else(|| elem.get("usergroup_id"))
                                                    .or_else(|| elem.get("channel_id"))
                                                    .and_then(|v| v.as_str())
                                                {
                                                    parts.push(format!("@{}", id));
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                // Add newline between sections
                                if section_type == "rich_text_preformatted" {
                                    // Wrap code blocks
                                    let last = parts.pop().unwrap_or_default();
                                    parts.push(format!("```\n{}\n```", last));
                                }
                            }
                            "rich_text_list" => {
                                if let Some(items) = section.get("elements").and_then(|v| v.as_array()) {
                                    for item in items {
                                        if let Some(inners) = item.get("elements").and_then(|v| v.as_array()) {
                                            let mut item_text = String::new();
                                            for elem in inners {
                                                if let Some(t) = elem.get("text").and_then(|v| v.as_str()) {
                                                    item_text.push_str(t);
                                                }
                                            }
                                            parts.push(format!("- {}", item_text));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        parts.push("\n".to_string());
                    }
                }
            }
            "section" => {
                if let Some(text) = block.get("text").and_then(|t| t.get("text")).and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                    parts.push("\n".to_string());
                }
            }
            _ => {}
        }
    }

    let result = parts.join("").trim().to_string();
    if result.is_empty() { None } else { Some(result) }
}

struct ButtonAction {
    action_id: String,
    message_ts: String,
    channel_id: String,
    /// The original section text from the auth request message
    original_section: String,
    /// The button's value field (used for mention_reply payloads)
    value: String,
}

fn extract_button_action(envelope: &serde_json::Value) -> Option<ButtonAction> {
    let payload = envelope.get("payload")?;
    let actions = payload.get("actions")?.as_array()?;
    let action = actions.first()?;
    let action_id = action.get("action_id")?.as_str()?.to_string();
    let value = action.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let message_ts = payload.get("message")?.get("ts")?.as_str()?.to_string();
    let channel_id = payload.get("channel")?.get("id")?.as_str()?.to_string();

    // Extract the original section text from message blocks
    let original_section = payload.get("message")
        .and_then(|m| m.get("blocks"))
        .and_then(|b| b.as_array())
        .and_then(|blocks| blocks.first())
        .and_then(|block| block.get("text"))
        .and_then(|t| t.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    Some(ButtonAction { action_id, message_ts, channel_id, original_section, value })
}

// ── Main session loop ──

/// Prompt with thread context for multi-session routing
struct Prompt {
    text: String,
    /// Which Slack thread to reply in
    thread_ts: String,
    /// Whether this starts a new session (top-level message)
    new_session: bool,
}

/// Per-thread session context
struct ThreadSession {
    session: ClaudeSession,
    /// Claude Code's session_id (known after first run_prompt)
    claude_session_id: Option<String>,
    /// Per-thread /btw queue
    btw_queue: Vec<String>,
}

/// Known Pawkit commands that should NOT be forwarded to Claude Code
const PAWKIT_COMMANDS: &[&str] = &["!ping", "!auto", "!green", "!cd", "!stop", "!status", "!mention"];

fn is_pawkit_command(text: &str) -> bool {
    let lower = text.to_lowercase();
    PAWKIT_COMMANDS.iter().any(|cmd| lower.starts_with(cmd))
}

pub async fn run_remote_session(
    slack: Arc<SlackBridge>,
    pending: PendingRequests,
    is_away: Arc<AtomicBool>,
    config: SlackConfig,
    session_store: Arc<Mutex<SessionStore>>,
    green_light: Arc<AtomicBool>,
    active_sessions: ActiveSessions,
    session_thread_map: SessionThreadMap,
    mention_mode: crate::mention_monitor::SharedMentionMode,
) {
    if let Err(e) = slack.init().await {
        plog!("[Pawkit] Slack init failed: {}", e);
        is_away.store(false, Ordering::SeqCst);
        return;
    }

    // Post welcome as top-level message (instructions only, not tied to any session)
    let _welcome_ts = match slack.post_top_message(&format!(
        "🐱 *Pawkit 远程模式已启动*\n\
         📂 默认工作目录: `{}`\n\n\
         新消息 = 新建会话 thread\n\
         thread 回复 = 继续该会话\n\n\
         `!ping` `!auto on/off` `!green on/off` `!stop` `!status`\n\
         `!mention monitor/auto/rest` @提及监控\n\
         `/btw <msg>` 追加消息 | `!命令` → Claude Code `/命令`",
        config.working_dir
    )).await {
        Ok(ts) => ts,
        Err(e) => {
            plog!("[Pawkit] Failed to post welcome: {}", e);
            is_away.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Create Slack threads for each existing active session
    {
        let sessions = active_sessions.lock().await;
        for (sid, info) in sessions.iter() {
            let dir_name = std::path::Path::new(&info.working_dir)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| info.working_dir.clone());
            let thread_msg = format!(
                "🐱 *{}* — `{}`\n_session: `{}`_\n_在此 thread 回复即可继续该会话对话_",
                info.title, dir_name, &sid[..sid.len().min(8)]
            );
            match slack.post_top_message(&thread_msg).await {
                Ok(ts) => {
                    plog!("[Pawkit] Created Slack thread for session {}: ts={}", sid, ts);
                    session_thread_map.lock().await.insert(sid.clone(), ts);
                }
                Err(e) => {
                    plog!("[Pawkit] Failed to create thread for session {}: {}", sid, e);
                }
            }
        }
    }

    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<Prompt>(32);
    let auto_approve = Arc::new(AtomicBool::new(false));

    // --- Task 1: Socket Mode listener ---
    let ws_slack = slack.clone();
    let ws_pending = pending.clone();
    let ws_away = is_away.clone();
    let ws_auto = auto_approve.clone();
    let ws_green = green_light.clone();
    let ws_thread_map = session_thread_map.clone();
    let ws_active = active_sessions.clone();
    let ws_mention_mode = mention_mode.clone();
    let ws_config = config.clone();

    let listener = tokio::spawn(async move {
        while ws_away.load(Ordering::SeqCst) {
            plog!("[Pawkit] Connecting Socket Mode...");
            let ws_url = match ws_slack.connect_socket().await {
                Ok(url) => url,
                Err(e) => {
                    plog!("[Pawkit] Socket Mode connect failed: {}", e);
                    let _ = ws_slack.reply(&format!("⚠️ Socket Mode 连接失败: `{}`", e)).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    plog!("[Pawkit] WebSocket connect failed: {}", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            plog!("[Pawkit] Socket Mode connected!");
            let _ = ws_slack.reply("🔗 已连接").await;

            let (mut ws_tx, mut ws_rx) = ws_stream.split();
            let dm_user_id = ws_slack.dm_user_id.clone();
            let dm_channel_id = ws_slack.dm_channel_id.lock().await.clone();

            while let Some(msg) = ws_rx.next().await {
                if !ws_away.load(Ordering::SeqCst) { break; }

                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        plog!("[Pawkit] WebSocket error: {}", e);
                        break;
                    }
                };

                let text = match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => t,
                    tokio_tungstenite::tungstenite::Message::Ping(data) => {
                        let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await;
                        continue;
                    }
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                let envelope: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // ACK
                if let Some(eid) = envelope.get("envelope_id").and_then(|v| v.as_str()) {
                    let ack = serde_json::json!({ "envelope_id": eid });
                    let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(ack.to_string().into())).await;
                }

                let env_type = envelope.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if env_type == "disconnect" { break; }

                // Button clicks — update original message in-place (grey out buttons)
                if env_type == "interactive" {
                    if let Some(action) = extract_button_action(&envelope) {
                        // Handle mention monitor buttons
                        if action.action_id == "mention_reply" || action.action_id == "mention_skip" {
                            let (emoji, label) = if action.action_id == "mention_reply" {
                                ("🤖", "正在回复...")
                            } else {
                                ("⏭", "已跳过")
                            };
                            let updated_blocks = serde_json::json!([
                                {
                                    "type": "section",
                                    "text": { "type": "mrkdwn", "text": action.original_section }
                                },
                                {
                                    "type": "context",
                                    "elements": [{ "type": "mrkdwn", "text": format!("{} _{}_", emoji, label) }]
                                }
                            ]);
                            let _ = ws_slack.update_message(
                                &action.channel_id, &action.message_ts,
                                &format!("{} {}", emoji, label), &updated_blocks,
                            ).await;

                            if action.action_id == "mention_reply" {
                                let reply_slack = ws_slack.clone();
                                let reply_config = ws_config.clone();
                                let reply_value = action.value.clone();
                                let dm_uid = ws_slack.dm_user_id.clone();
                                tokio::spawn(async move {
                                    crate::mention_monitor::handle_mention_reply_button(
                                        reply_slack, &reply_value, &reply_config, &dm_uid,
                                    ).await;
                                });
                            }
                            continue;
                        }

                        // Handle auth buttons
                        let allow = action.action_id == "auth_allow";
                        if resolve_first_pending(&ws_pending, allow).await {
                            let (emoji, label) = if allow { ("✅", "已允许") } else { ("❌", "已拒绝") };
                            let updated_blocks = serde_json::json!([
                                {
                                    "type": "section",
                                    "text": {
                                        "type": "mrkdwn",
                                        "text": action.original_section
                                    }
                                },
                                {
                                    "type": "context",
                                    "elements": [{
                                        "type": "mrkdwn",
                                        "text": format!("{} _{}_", emoji, label)
                                    }]
                                }
                            ]);
                            let fallback = format!("{} {}", emoji, label);
                            let _ = ws_slack.update_message(
                                &action.channel_id,
                                &action.message_ts,
                                &fallback,
                                &updated_blocks,
                            ).await;
                        }
                    }
                    continue;
                }

                if env_type != "events_api" { continue; }

                let user_msg = match extract_user_message(&envelope, &dm_user_id, &dm_channel_id) {
                    Some(m) => m,
                    None => continue,
                };

                let preview: String = user_msg.text.chars().take(60).collect();
                plog!("[Pawkit] Message: thread={:?} msg_ts={} text={}", user_msg.thread_ts, user_msg.msg_ts, preview);

                let lower = user_msg.text.to_lowercase();

                // Determine the reply thread: if in a thread use that, otherwise use msg_ts as new thread
                let reply_thread = user_msg.thread_ts.as_deref().unwrap_or(&user_msg.msg_ts);

                // Inline commands — reply in the message's thread context
                if lower == "!ping" {
                    let _ = ws_slack.reply_in_thread(reply_thread, "🏓 pong!").await;
                    continue;
                }
                if lower == "!auto on" {
                    ws_auto.store(true, Ordering::SeqCst);
                    let _ = ws_slack.reply_in_thread(reply_thread, "✅ 自动审批已开启").await;
                    continue;
                }
                if lower == "!auto off" {
                    ws_auto.store(false, Ordering::SeqCst);
                    let _ = ws_slack.reply_in_thread(reply_thread, "🔒 自动审批已关闭").await;
                    continue;
                }
                if lower == "!green on" {
                    ws_green.store(true, Ordering::SeqCst);
                    let _ = ws_slack.reply_in_thread(reply_thread, "🟢 绿灯模式已开启").await;
                    continue;
                }
                if lower == "!green off" {
                    ws_green.store(false, Ordering::SeqCst);
                    let _ = ws_slack.reply_in_thread(reply_thread, "🔴 绿灯模式已关闭").await;
                    continue;
                }
                // !mention — change mention monitor mode
                if lower.starts_with("!mention") {
                    let arg = lower.strip_prefix("!mention").unwrap().trim();
                    match arg {
                        "monitor" | "on" => {
                            *ws_mention_mode.lock().await = crate::mention_monitor::MentionMode::Monitor;
                            let _ = ws_slack.reply_in_thread(reply_thread, "👂 @mention 监听模式已开启").await;
                        }
                        "auto" | "auto_reply" => {
                            *ws_mention_mode.lock().await = crate::mention_monitor::MentionMode::AutoReply;
                            let _ = ws_slack.reply_in_thread(reply_thread, "🤖 @mention 自动回复已开启").await;
                        }
                        "rest" | "off" => {
                            *ws_mention_mode.lock().await = crate::mention_monitor::MentionMode::Rest;
                            let _ = ws_slack.reply_in_thread(reply_thread, "😴 @mention 休息模式").await;
                        }
                        _ => {
                            let current = ws_mention_mode.lock().await.label();
                            let _ = ws_slack.reply_in_thread(reply_thread, &format!(
                                "📡 当前: *{}*\n`!mention monitor` 监听 | `!mention auto` 自动回复 | `!mention rest` 休息",
                                current
                            )).await;
                        }
                    }
                    continue;
                }
                // !status — list active sessions and their threads
                if lower == "!status" {
                    let map = ws_thread_map.lock().await;
                    if map.is_empty() {
                        let _ = ws_slack.reply_in_thread(reply_thread, "📋 当前无活跃会话 thread").await;
                    } else {
                        // Collect session info including titles from active_sessions
                        let active = ws_active.lock().await;
                        let lines: Vec<String> = map.iter()
                            .map(|(sid, _ts)| {
                                let title = active.get(sid)
                                    .map(|i| i.title.as_str())
                                    .unwrap_or("?");
                                format!("• *{}* `{}`", title, &sid[..sid.len().min(8)])
                            })
                            .collect();
                        let _ = ws_slack.reply_in_thread(reply_thread, &format!("📋 活跃会话:\n{}", lines.join("\n"))).await;
                    }
                    continue;
                }
                // /btw command: queue message for the session in this thread
                if lower.starts_with("/btw ") {
                    let btw_text = user_msg.text[5..].trim().to_string();
                    if !btw_text.is_empty() {
                        // Send as prompt with a /btw prefix marker for the processor to handle
                        let _ = prompt_tx.send(Prompt {
                            text: format!("/btw {}", btw_text),
                            thread_ts: reply_thread.to_string(),
                            new_session: false,
                        }).await;
                    }
                    continue;
                }
                if (lower == "allow" || lower == "y" || lower == "deny" || lower == "n")
                    && has_pending_auth(&ws_pending).await
                {
                    let allow = lower == "allow" || lower == "y";
                    if resolve_first_pending(&ws_pending, allow).await {
                        let (e, a) = if allow { ("✅", "已允许") } else { ("❌", "已拒绝") };
                        let _ = ws_slack.reply_in_thread(reply_thread, &format!("{} {}", e, a)).await;
                        continue;
                    }
                }

                // Determine if this is a new session or reply to existing thread
                let is_new_session = user_msg.thread_ts.is_none();
                let thread_ts = if is_new_session {
                    // Top-level message → its own ts becomes the thread parent
                    user_msg.msg_ts.clone()
                } else {
                    // Thread reply → use the thread_ts
                    user_msg.thread_ts.clone().unwrap_or(user_msg.msg_ts.clone())
                };

                plog!("[Pawkit] Routing to thread={} new_session={}", thread_ts, is_new_session);

                let _ = prompt_tx.send(Prompt {
                    text: user_msg.text,
                    thread_ts,
                    new_session: is_new_session,
                }).await;
            }

            if ws_away.load(Ordering::SeqCst) {
                plog!("[Pawkit] Socket disconnected, reconnecting in 2s...");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    });

    // --- Task 2: Multi-session prompt processor ---
    let proc_slack = slack.clone();
    let proc_away = is_away.clone();
    let proc_store = session_store.clone();
    let proc_thread_map = session_thread_map.clone();
    let proc_active = active_sessions.clone();
    let default_wd = config.working_dir.clone();

    let processor = tokio::spawn(async move {
        // Per-thread session contexts: thread_ts → ThreadSession
        // NOT pre-populated — terminal session threads are notification-only until
        // the user sends a message, at which point a fresh session starts in that
        // session's working directory.
        let mut thread_sessions: HashMap<String, ThreadSession> = HashMap::new();

        while let Some(prompt) = prompt_rx.recv().await {
            if !proc_away.load(Ordering::SeqCst) { break; }

            let thread_ts = prompt.thread_ts.clone();

            // Handle /btw — queue for later delivery
            if prompt.text.starts_with("/btw ") {
                let btw_text = prompt.text[5..].trim().to_string();
                if let Some(ctx) = thread_sessions.get_mut(&thread_ts) {
                    ctx.btw_queue.push(btw_text);
                    let _ = proc_slack.reply_in_thread(&thread_ts, "📝 已记录，会在当前任务完成后转达").await;
                } else {
                    let _ = proc_slack.reply_in_thread(&thread_ts, "❓ 该 thread 没有活跃会话").await;
                }
                continue;
            }

            // Handle special commands — these operate on the thread's session
            if prompt.text.starts_with("!cd ") {
                let dir = prompt.text[4..].trim().to_string();
                if let Some(ctx) = thread_sessions.get_mut(&thread_ts) {
                    if dir.is_empty() {
                        let wd = ctx.session.working_dir().to_string();
                        let _ = proc_slack.reply_in_thread(&thread_ts, &format!("📂 当前工作目录: `{}`", wd)).await;
                    } else if std::path::Path::new(&dir).is_dir() {
                        ctx.session.set_working_dir(dir.clone());
                        let _ = proc_slack.reply_in_thread(&thread_ts, &format!("📂 已切换到: `{}`", dir)).await;
                    } else {
                        let _ = proc_slack.reply_in_thread(&thread_ts, &format!("❌ 目录不存在: `{}`", dir)).await;
                    }
                } else {
                    let _ = proc_slack.reply_in_thread(&thread_ts, "❓ 该 thread 没有活跃会话").await;
                }
                continue;
            }
            if prompt.text == "!stop" {
                if let Some(ctx) = thread_sessions.get_mut(&thread_ts) {
                    let wd = ctx.session.working_dir().to_string();
                    ctx.session.reset();
                    ctx.claude_session_id = None;
                    let _ = proc_slack.reply_in_thread(&thread_ts, &format!("⏹ 会话已重置。工作目录: `{}`", wd)).await;
                } else {
                    let _ = proc_slack.reply_in_thread(&thread_ts, "❓ 该 thread 没有活跃会话").await;
                }
                continue;
            }

            // Convert !command → /command for Claude Code passthrough
            // (e.g., !compact → /compact, !clear → /clear)
            let prompt_text = if prompt.text.starts_with('!') && !is_pawkit_command(&prompt.text) {
                let cc_cmd = format!("/{}", &prompt.text[1..]);
                let _ = proc_slack.reply_in_thread(&thread_ts, &format!("🔧 → `{}`", cc_cmd)).await;
                cc_cmd
            } else {
                prompt.text.clone()
            };

            // Get or create session for this thread
            if prompt.new_session || !thread_sessions.contains_key(&thread_ts) {
                // Check if this thread maps to an existing session (terminal or previous Slack)
                let (existing_sid, wd) = {
                    let map = proc_thread_map.lock().await;
                    // Reverse lookup: find session_id for this thread_ts
                    let sid = map.iter()
                        .find(|(_k, v)| v.as_str() == thread_ts)
                        .map(|(k, _)| k.clone());
                    let wd = if let Some(ref sid) = sid {
                        let sessions = proc_active.lock().await;
                        sessions.get(sid)
                            .map(|i| i.working_dir.clone())
                            .filter(|w| !w.is_empty())
                            .unwrap_or_else(|| default_wd.clone())
                    } else {
                        default_wd.clone()
                    };
                    // Only resume for thread replies (not new top-level messages)
                    let resume_sid = if !prompt.new_session { sid } else { None };
                    (resume_sid, wd)
                };

                let ctx = if let Some(ref sid) = existing_sid {
                    // This thread belongs to an existing session → resume it
                    plog!("[Pawkit] Resuming session {} in thread {} wd={}", sid, thread_ts, wd);
                    let _ = proc_slack.reply_in_thread(&thread_ts, &format!("🔄 _继续会话 `{}`_", &sid[..sid.len().min(8)])).await;
                    ThreadSession {
                        session: ClaudeSession::new_resume(sid.clone(), wd),
                        claude_session_id: Some(sid.clone()),
                        btw_queue: Vec::new(),
                    }
                } else {
                    // Brand new session
                    if prompt.new_session {
                        let _ = proc_slack.reply_in_thread(&thread_ts, "🆕 _新会话_").await;
                    }
                    ThreadSession {
                        session: ClaudeSession::new(wd),
                        claude_session_id: None,
                        btw_queue: Vec::new(),
                    }
                };
                thread_sessions.insert(thread_ts.clone(), ctx);
            }

            let ctx = thread_sessions.get_mut(&thread_ts).unwrap();

            // Set active_thread_ts so hook_server auth buttons go to the right thread
            proc_slack.set_active_thread(&thread_ts).await;

            // Show typing indicator
            let _ = proc_slack.set_status_in_thread(&thread_ts, "🤔 思考中...").await;

            let mut result = ctx.session.run_prompt(&prompt_text).await;

            // If --resume failed (stale/invalid session), fall back to fresh session and retry
            if let Err(ref e) = result {
                if e.contains("No conversation found") || e.contains("no session") {
                    plog!("[Pawkit] Resume failed, starting fresh: {}", e);
                    let _ = proc_slack.reply_in_thread(&thread_ts, "⚠️ 会话无法恢复，已新建会话").await;
                    let wd = ctx.session.working_dir().to_string();
                    ctx.session = ClaudeSession::new(wd);
                    ctx.claude_session_id = None;
                    result = ctx.session.run_prompt(&prompt_text).await;
                }
            }

            // Clear typing indicator
            let _ = proc_slack.set_status_in_thread(&thread_ts, "").await;

            match result {
                Ok(output) => {
                    // Track session in store + update thread mapping
                    if let Some(ref sid) = output.session_id {
                        // Update session_thread_map so hooks route to this thread
                        proc_thread_map.lock().await.insert(sid.clone(), thread_ts.clone());
                        ctx.claude_session_id = Some(sid.clone());

                        let now = chrono::Utc::now().timestamp_millis();
                        let mut store = proc_store.lock().await;
                        if store.by_id(sid).is_none() {
                            let title = crate::session_store::generate_title(sid);
                            let wd = ctx.session.working_dir().to_string();
                            store.upsert(SessionRecord {
                                session_id: sid.clone(),
                                title,
                                working_dir: wd,
                                created_at: now,
                                last_active: now,
                                source: SessionSource::Slack,
                                slack_thread_ts: Some(thread_ts.clone()),
                                total_cost_usd: output.cost_usd.unwrap_or(0.0),
                            });
                            let _ = proc_slack.reply_in_thread(&thread_ts, &format!("_sid: `{}`_", &sid[..sid.len().min(8)])).await;
                        } else {
                            store.touch(sid);
                            if let Some(cost) = output.cost_usd {
                                store.add_cost(sid, cost);
                            }
                            store.save();
                        }
                    }

                    if output.is_error {
                        let _ = proc_slack.reply_in_thread(&thread_ts, &format!("⚠️ Claude 返回错误:\n```\n{}\n```", output.text)).await;
                    } else {
                        post_claude_output_in_thread(&proc_slack, &thread_ts, &output).await;
                    }
                }
                Err(e) => {
                    let _ = proc_slack.reply_in_thread(&thread_ts, &format!("❌ 执行失败:\n```\n{}\n```", e)).await;
                }
            }

            // Process queued /btw messages after each prompt completes
            let ctx = thread_sessions.get_mut(&thread_ts).unwrap();
            let btw_messages: Vec<String> = ctx.btw_queue.drain(..).collect();
            if !btw_messages.is_empty() {
                let follow_up = format!(
                    "BTW, the user wanted to add this context:\n{}",
                    btw_messages.join("\n---\n")
                );
                let _ = proc_slack.reply_in_thread(&thread_ts, &format!("📝 转达中: _{}_", btw_messages.join("; "))).await;
                let _ = proc_slack.set_status_in_thread(&thread_ts, "🤔 thinking about btw...").await;
                match ctx.session.run_prompt(&follow_up).await {
                    Ok(output) => {
                        let _ = proc_slack.set_status_in_thread(&thread_ts, "").await;
                        if output.is_error {
                            let _ = proc_slack.reply_in_thread(&thread_ts, &format!("⚠️ {}", output.text)).await;
                        } else {
                            post_claude_output_in_thread(&proc_slack, &thread_ts, &output).await;
                        }
                    }
                    Err(e) => {
                        let _ = proc_slack.set_status_in_thread(&thread_ts, "").await;
                        let _ = proc_slack.reply_in_thread(&thread_ts, &format!("❌ btw 执行失败: {}", e)).await;
                    }
                }
            }
        }
    });

    let (listener_res, processor_res) = tokio::join!(listener, processor);
    plog!("[Pawkit] Remote session ended. listener={:?} processor={:?}", listener_res, processor_res);
    let _ = slack.reply("🐱 *Pawkit 远程模式已关闭，回家啦~*").await;
}
