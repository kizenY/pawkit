use crate::claude_session::{ClaudeOutput, ClaudeSession};
use crate::config::SlackConfig;
use crate::hook_server::{AuthDecision, PendingRequests, TerminalSession};
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
        println!("[Pawkit] Slack bot user ID: {}", user_id);
        *self.bot_user_id.lock().await = user_id;

        let data = self
            .api_post("conversations.open", &serde_json::json!({ "users": self.dm_user_id }))
            .await?;
        let channel_id = data["channel"]["id"].as_str().unwrap_or("").to_string();
        if channel_id.is_empty() {
            return Err("conversations.open returned empty channel ID".to_string());
        }
        println!("[Pawkit] DM channel ID: {}", channel_id);
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

/// Extract user message + thread info from a Socket Mode event
fn extract_user_message(envelope: &serde_json::Value, dm_user_id: &str, dm_channel_id: &str) -> Option<UserMessage> {
    let payload = envelope.get("payload")?;
    let event = payload.get("event")?;

    if event.get("type")?.as_str()? != "message" { return None; }
    if event.get("subtype").is_some() { return None; }
    if event.get("bot_id").is_some() { return None; }
    if event.get("user")?.as_str()? != dm_user_id { return None; }
    if event.get("channel")?.as_str()? != dm_channel_id { return None; }

    let text = event.get("text")?.as_str()?.trim().to_string();
    if text.is_empty() { return None; }

    let thread_ts = event.get("thread_ts").and_then(|v| v.as_str()).map(String::from);

    Some(UserMessage { text, thread_ts })
}

fn extract_button_action(envelope: &serde_json::Value) -> Option<(String, String)> {
    let payload = envelope.get("payload")?;
    let actions = payload.get("actions")?.as_array()?;
    let action = actions.first()?;
    let action_id = action.get("action_id")?.as_str()?.to_string();
    let value = action.get("value")?.as_str()?.to_string();
    Some((action_id, value))
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
) {
    if let Err(e) = slack.init().await {
        eprintln!("[Pawkit] Slack init failed: {}", e);
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
         `!ping` `!cd` `!stop` `!auto on/off`",
        config.working_dir
    )).await {
        Ok(ts) => ts,
        Err(e) => {
            eprintln!("[Pawkit] Failed to post welcome: {}", e);
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
        println!("[Pawkit] Resuming terminal session: {} (cwd={})", ts.session_id, wd);
        Arc::new(Mutex::new(ClaudeSession::new_resume(ts.session_id.clone(), wd)))
    } else {
        println!("[Pawkit] No terminal session captured, starting fresh");
        Arc::new(Mutex::new(ClaudeSession::new(config.working_dir.clone())))
    };

    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<Prompt>(32);
    let auto_approve = Arc::new(AtomicBool::new(false));

    // --- Task 1: Socket Mode listener ---
    let ws_slack = slack.clone();
    let ws_pending = pending.clone();
    let ws_away = is_away.clone();
    let ws_auto = auto_approve.clone();

    let listener = tokio::spawn(async move {
        while ws_away.load(Ordering::SeqCst) {
            println!("[Pawkit] Connecting Socket Mode...");
            let ws_url = match ws_slack.connect_socket().await {
                Ok(url) => url,
                Err(e) => {
                    eprintln!("[Pawkit] Socket Mode connect failed: {}", e);
                    let _ = ws_slack.reply(&format!("⚠️ Socket Mode 连接失败: `{}`", e)).await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    eprintln!("[Pawkit] WebSocket connect failed: {}", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            println!("[Pawkit] Socket Mode connected!");
            let _ = ws_slack.reply("🔗 已连接").await;

            let (mut ws_tx, mut ws_rx) = ws_stream.split();
            let dm_user_id = ws_slack.dm_user_id.clone();
            let dm_channel_id = ws_slack.dm_channel_id.lock().await.clone();

            while let Some(msg) = ws_rx.next().await {
                if !ws_away.load(Ordering::SeqCst) { break; }

                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[Pawkit] WebSocket error: {}", e);
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

                // Button clicks
                if env_type == "interactive" {
                    if let Some((action_id, _)) = extract_button_action(&envelope) {
                        let allow = action_id == "auth_allow";
                        if resolve_first_pending(&ws_pending, allow).await {
                            let (emoji, action) = if allow { ("✅", "已允许") } else { ("❌", "已拒绝") };
                            let _ = ws_slack.reply(&format!("{} {}", emoji, action)).await;
                        }
                    }
                    continue;
                }

                if env_type != "events_api" { continue; }

                let user_msg = match extract_user_message(&envelope, &dm_user_id, &dm_channel_id) {
                    Some(m) => m,
                    None => continue,
                };

                println!("[Pawkit] Message: thread={:?} text={}", user_msg.thread_ts, &user_msg.text[..user_msg.text.len().min(60)]);

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

                    println!("[Pawkit] New session, thread={}", new_thread);
                }

                let _ = prompt_tx.send(Prompt {
                    text: user_msg.text,
                    new_session: is_new_session,
                }).await;
            }

            if ws_away.load(Ordering::SeqCst) {
                println!("[Pawkit] Socket disconnected, reconnecting in 2s...");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    });

    // --- Task 2: Prompt processor ---
    let proc_slack = slack.clone();
    let proc_session = session.clone();
    let proc_away = is_away.clone();

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

            let _ = proc_slack.reply("⏳ _处理中..._").await;

            let mut session = proc_session.lock().await;
            match session.run_prompt(&prompt.text).await {
                Ok(output) => {
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
        }
    });

    let _ = tokio::join!(listener, processor);
    let _ = slack.reply("🐱 *Pawkit 远程模式已关闭，回家啦~*").await;
}
