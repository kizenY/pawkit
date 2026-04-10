use crate::plog;
use crate::claude_session::{ClaudeOutput, ClaudeSession};
use crate::config::SlackConfig;
use crate::hook_server::{AuthDecision, PendingRequests, TerminalSession};
use crate::session_store::{SessionRecord, SessionSource, SessionStore};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

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

    pub async fn get_active_thread(&self) -> String {
        self.active_thread_ts.lock().await.clone()
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

async fn post_claude_output(slack: &SlackBridge, output: &ClaudeOutput) {
    let text = &output.text;
    if text.is_empty() {
        let _ = slack.reply("_(无输出)_").await;
        return;
    }

    let max_len = 3000;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_len {
        let _ = slack.reply(text).await;
    } else {
        let total = (chars.len() + max_len - 1) / max_len;
        for (i, chunk) in chars.chunks(max_len).enumerate() {
            let chunk_text: String = chunk.iter().collect();
            let _ = slack.reply(&format!("_({}/{})_\n{}", i + 1, total, chunk_text)).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    if let Some(cost) = output.cost_usd {
        if let Some(dur) = output.duration_ms {
            let _ = slack.reply(&format!("💰 ${:.4} ⏱ {:.1}s", cost, dur as f64 / 1000.0)).await;
        } else {
            let _ = slack.reply(&format!("💰 ${:.4}", cost)).await;
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

    Some(UserMessage { text, thread_ts })
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
}

fn extract_button_action(envelope: &serde_json::Value) -> Option<ButtonAction> {
    let payload = envelope.get("payload")?;
    let actions = payload.get("actions")?.as_array()?;
    let action = actions.first()?;
    let action_id = action.get("action_id")?.as_str()?.to_string();
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

    Some(ButtonAction { action_id, message_ts, channel_id, original_section })
}

// ── Main session loop ──

/// Prompt with metadata: whether to start a new session
struct Prompt {
    text: String,
    new_session: bool,
}

pub async fn run_remote_session(
    slack: Arc<SlackBridge>,
    pending: PendingRequests,
    is_away: Arc<AtomicBool>,
    config: SlackConfig,
    initial_session: Option<TerminalSession>,
    session_store: Arc<Mutex<SessionStore>>,
    green_light: Arc<AtomicBool>,
) {
    if let Err(e) = slack.init().await {
        plog!("[Pawkit] Slack init failed: {}", e);
        is_away.store(false, Ordering::SeqCst);
        return;
    }

    // Post welcome as top-level message — this starts the first thread
    // Use --continue to inherit the last local session
    let _welcome_ts = match slack.post_top_message(&format!(
        "🐱 *Pawkit 远程模式已启动*\n\
         📂 工作目录: `{}`\n\
         _在此 thread 中继续最近的会话_\n\
         _(先退出终端 Claude Code 再外出，确保会话能被继承)_\n\n\
         新消息(非thread回复) = 新建会话\n\
         `!ping` `!cd` `!stop` `!auto on/off` `!green on/off`\n\
         `/btw <msg>` 追加消息到当前会话",
        config.working_dir
    )).await {
        Ok(ts) => ts,
        Err(e) => {
            plog!("[Pawkit] Failed to post welcome: {}", e);
            is_away.store(false, Ordering::SeqCst);
            return;
        }
    };

    // If we have a specific terminal session (ID + working dir from hook server),
    // resume that exact session with its original working directory.
    // --continue is unreliable because it scopes to working_dir and would only
    // find previous Slack sessions (since Slack uses a different working_dir).
    let session = if let Some(ref ts) = initial_session {
        let wd = if ts.working_dir.is_empty() { config.working_dir.clone() } else { ts.working_dir.clone() };
        plog!("[Pawkit] Resuming terminal session: {} (cwd={})", ts.session_id, wd);
        Arc::new(Mutex::new(ClaudeSession::new_resume(ts.session_id.clone(), wd)))
    } else {
        plog!("[Pawkit] No terminal session captured, starting fresh");
        Arc::new(Mutex::new(ClaudeSession::new(config.working_dir.clone())))
    };

    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<Prompt>(32);
    let auto_approve = Arc::new(AtomicBool::new(false));
    let btw_queue: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // --- Task 1: Socket Mode listener ---
    let ws_slack = slack.clone();
    let ws_pending = pending.clone();
    let ws_away = is_away.clone();
    let ws_auto = auto_approve.clone();
    let ws_green = green_light.clone();
    let ws_btw = btw_queue.clone();

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
                        let allow = action.action_id == "auth_allow";
                        if resolve_first_pending(&ws_pending, allow).await {
                            let (emoji, label) = if allow { ("✅", "已允许") } else { ("❌", "已拒绝") };
                            // Replace the buttons with a resolved status line
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
                plog!("[Pawkit] Message: thread={:?} text={}", user_msg.thread_ts, preview);

                let lower = user_msg.text.to_lowercase();
                let active_thread = ws_slack.get_active_thread().await;

                // Inline commands — reply in whatever context
                if lower == "!ping" {
                    let _ = ws_slack.reply("🏓 pong!").await;
                    continue;
                }
                if lower == "!auto on" {
                    ws_auto.store(true, Ordering::SeqCst);
                    let _ = ws_slack.reply("✅ 自动审批已开启").await;
                    continue;
                }
                if lower == "!auto off" {
                    ws_auto.store(false, Ordering::SeqCst);
                    let _ = ws_slack.reply("🔒 自动审批已关闭").await;
                    continue;
                }
                if lower == "!green on" {
                    ws_green.store(true, Ordering::SeqCst);
                    let _ = ws_slack.reply("🟢 绿灯模式已开启").await;
                    continue;
                }
                if lower == "!green off" {
                    ws_green.store(false, Ordering::SeqCst);
                    let _ = ws_slack.reply("🔴 绿灯模式已关闭").await;
                    continue;
                }
                // /btw command: queue message for current session
                if lower.starts_with("/btw ") {
                    let btw_text = user_msg.text[5..].trim().to_string();
                    if !btw_text.is_empty() {
                        ws_btw.lock().await.push(btw_text);
                        let _ = ws_slack.reply("📝 已记录，会在当前任务完成后转达").await;
                    }
                    continue;
                }
                if (lower == "allow" || lower == "y" || lower == "deny" || lower == "n")
                    && has_pending_auth(&ws_pending).await
                {
                    let allow = lower == "allow" || lower == "y";
                    if resolve_first_pending(&ws_pending, allow).await {
                        let (e, a) = if allow { ("✅", "已允许") } else { ("❌", "已拒绝") };
                        let _ = ws_slack.reply(&format!("{} {}", e, a)).await;
                        continue;
                    }
                }

                // Determine if this is a new session or continuing
                let is_in_active_thread = user_msg.thread_ts.as_deref() == Some(&active_thread);
                let is_new_session = !is_in_active_thread;

                if is_new_session {
                    // User sent a top-level message or replied in a different thread
                    // → new session, new thread parent
                    // The message itself becomes the thread parent (its ts)
                    // We need to get the ts from the event
                    let event = envelope.get("payload")
                        .and_then(|p| p.get("event"));
                    let msg_ts = event
                        .and_then(|e| e.get("ts"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    // If it's a top-level message, use its ts as the new thread
                    // If it's in another thread, use that thread_ts
                    let new_thread = user_msg.thread_ts.as_deref().unwrap_or(msg_ts);
                    ws_slack.set_active_thread(new_thread).await;

                    plog!("[Pawkit] New session, thread={}", new_thread);
                }

                let _ = prompt_tx.send(Prompt {
                    text: user_msg.text,
                    new_session: is_new_session,
                }).await;
            }

            if ws_away.load(Ordering::SeqCst) {
                plog!("[Pawkit] Socket disconnected, reconnecting in 2s...");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    });

    // --- Task 2: Prompt processor ---
    let proc_slack = slack.clone();
    let proc_session = session.clone();
    let proc_away = is_away.clone();
    let proc_store = session_store.clone();
    let proc_btw = btw_queue.clone();

    let processor = tokio::spawn(async move {
        while let Some(prompt) = prompt_rx.recv().await {
            if !proc_away.load(Ordering::SeqCst) { break; }

            // Handle special commands
            if prompt.text.starts_with("!cd ") {
                let dir = prompt.text[4..].trim().to_string();
                if dir.is_empty() {
                    let wd = proc_session.lock().await.working_dir().to_string();
                    let _ = proc_slack.reply(&format!("📂 当前工作目录: `{}`", wd)).await;
                } else if std::path::Path::new(&dir).is_dir() {
                    proc_session.lock().await.set_working_dir(dir.clone());
                    let _ = proc_slack.reply(&format!("📂 已切换到: `{}`", dir)).await;
                } else {
                    let _ = proc_slack.reply(&format!("❌ 目录不存在: `{}`", dir)).await;
                }
                continue;
            }
            if prompt.text == "!stop" {
                let wd = proc_session.lock().await.working_dir().to_string();
                proc_session.lock().await.reset();
                let _ = proc_slack.reply(&format!("⏹ 会话已重置。工作目录: `{}`", wd)).await;
                continue;
            }
            if prompt.text.starts_with('!') {
                let _ = proc_slack.reply(&format!("❓ 未知命令: `{}`", prompt.text)).await;
                continue;
            }

            // New session → reset Claude session to start fresh
            if prompt.new_session {
                let wd = proc_session.lock().await.working_dir().to_string();
                *proc_session.lock().await = ClaudeSession::new(wd);
                let _ = proc_slack.reply("🆕 _新会话_").await;
            }

            // Show typing indicator via Slack Assistants API (best-effort)
            let _ = proc_slack.set_status("🤔 思考中...").await;

            let mut session = proc_session.lock().await;
            let result = session.run_prompt(&prompt.text).await;

            // Clear typing indicator
            let _ = proc_slack.clear_status().await;

            match result {
                Ok(output) => {
                    // Track session in store
                    if let Some(ref sid) = output.session_id {
                        let thread_ts = proc_slack.get_active_thread().await;
                        let now = chrono::Utc::now().timestamp_millis();
                        let mut store = proc_store.lock().await;
                        if store.by_id(sid).is_none() {
                            // First output for this session — register & post session ID
                            let title = crate::session_store::generate_title(sid);
                            let wd = proc_session.lock().await.working_dir().to_string();
                            store.upsert(SessionRecord {
                                session_id: sid.clone(),
                                title: title.clone(),
                                working_dir: wd,
                                created_at: now,
                                last_active: now,
                                source: SessionSource::Slack,
                                slack_thread_ts: if thread_ts.is_empty() { None } else { Some(thread_ts) },
                                total_cost_usd: output.cost_usd.unwrap_or(0.0),
                            });
                            let _ = proc_slack.reply(&format!("_sid: `{}`_", &sid[..sid.len().min(8)])).await;
                        } else {
                            store.touch(sid);
                            if let Some(cost) = output.cost_usd {
                                store.add_cost(sid, cost);
                            }
                            store.save();
                        }
                    }

                    if output.is_error {
                        let _ = proc_slack.reply(&format!("⚠️ Claude 返回错误:\n```\n{}\n```", output.text)).await;
                    } else {
                        post_claude_output(&proc_slack, &output).await;
                    }
                }
                Err(e) => {
                    let _ = proc_slack.reply(&format!("❌ 执行失败:\n```\n{}\n```", e)).await;
                }
            }

            // F8: Process queued /btw messages after each prompt completes
            let btw_messages: Vec<String> = {
                let mut q = proc_btw.lock().await;
                q.drain(..).collect()
            };
            if !btw_messages.is_empty() {
                let follow_up = format!(
                    "BTW, the user wanted to add this context:\n{}",
                    btw_messages.join("\n---\n")
                );
                let _ = proc_slack.reply(&format!("📝 转达中: _{}_", btw_messages.join("; "))).await;
                let _ = proc_slack.set_status("🤔 thinking about btw...").await;
                let mut session = proc_session.lock().await;
                match session.run_prompt(&follow_up).await {
                    Ok(output) => {
                        let _ = proc_slack.clear_status().await;
                        if output.is_error {
                            let _ = proc_slack.reply(&format!("⚠️ {}", output.text)).await;
                        } else {
                            post_claude_output(&proc_slack, &output).await;
                        }
                    }
                    Err(e) => {
                        let _ = proc_slack.clear_status().await;
                        let _ = proc_slack.reply(&format!("❌ btw 执行失败: {}", e)).await;
                    }
                }
            }
        }
    });

    let (listener_res, processor_res) = tokio::join!(listener, processor);
    plog!("[Pawkit] Remote session ended. listener={:?} processor={:?}", listener_res, processor_res);
    let _ = slack.reply("🐱 *Pawkit 远程模式已关闭，回家啦~*").await;
}
