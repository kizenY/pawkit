#[allow(unused_imports)]
use crate::plog;
use crate::claude_session::ClaudeSession;
use crate::config::SlackConfig;
use crate::slack_bridge::SlackBridge;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

// ── Types ──

/// Mention monitor operating mode
#[derive(Debug, Clone, PartialEq)]
pub enum MentionMode {
    /// Prompt user for approval before replying
    Monitor,
    /// Automatically reply without approval
    AutoReply,
    /// Stop monitoring entirely
    Rest,
}

impl MentionMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "monitor" => Self::Monitor,
            "auto_reply" | "auto" => Self::AutoReply,
            _ => Self::Rest,
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Monitor => "monitor",
            Self::AutoReply => "auto_reply",
            Self::Rest => "rest",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Monitor => "监听模式",
            Self::AutoReply => "自动回复",
            Self::Rest => "休息模式",
        }
    }
}

pub type SharedMentionMode = Arc<Mutex<MentionMode>>;

/// A detected @mention event in a channel
struct MentionEvent {
    channel: String,
    thread_ts: Option<String>,
    msg_ts: String,
    user: String,
    text: String,
}

/// Tracks which mentions we've already processed (prevent duplicates)
type SeenMentions = Arc<Mutex<HashSet<String>>>;

// ── Main monitor loop ──

pub async fn run_mention_monitor(
    slack: Arc<SlackBridge>,
    mode: SharedMentionMode,
    config: SlackConfig,
    stop_flag: Arc<AtomicBool>,
) {
    // Initialize Slack connection (get bot_user_id, dm_channel_id)
    if let Err(e) = slack.init().await {
        plog!("[Pawkit/Mention] Slack init failed: {}", e);
        return;
    }

    let seen: SeenMentions = Arc::new(Mutex::new(HashSet::new()));
    let dm_user_id = config.dm_user_id.clone();

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            plog!("[Pawkit/Mention] Stop flag set, exiting monitor");
            break;
        }

        // Check mode — if rest, sleep and recheck
        {
            let current_mode = mode.lock().await.clone();
            if current_mode == MentionMode::Rest {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }

        plog!("[Pawkit/Mention] Connecting Socket Mode...");
        let ws_url = match slack.connect_socket().await {
            Ok(url) => url,
            Err(e) => {
                plog!("[Pawkit/Mention] Socket Mode connect failed: {}", e);
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };

        let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok((stream, _)) => stream,
            Err(e) => {
                plog!("[Pawkit/Mention] WebSocket connect failed: {}", e);
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        };

        plog!("[Pawkit/Mention] Socket Mode connected");
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        let bot_user_id = slack.get_bot_user_id().lock().await.clone();
        let dm_channel_id = slack.get_dm_channel_id().lock().await.clone();

        while let Some(msg) = ws_rx.next().await {
            if stop_flag.load(Ordering::SeqCst) { break; }

            // Check mode — if switched to rest mid-connection, break to reconnect loop
            {
                let current_mode = mode.lock().await.clone();
                if current_mode == MentionMode::Rest {
                    plog!("[Pawkit/Mention] Mode switched to rest, disconnecting");
                    break;
                }
            }

            let text = match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => t,
                Ok(tokio_tungstenite::tungstenite::Message::Ping(data)) => {
                    let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await;
                    continue;
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                Err(e) => {
                    plog!("[Pawkit/Mention] WebSocket error: {}", e);
                    break;
                }
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
            if env_type != "events_api" { continue; }

            // Extract mention event
            let mention = match extract_mention_event(&envelope, &dm_user_id, &bot_user_id, &dm_channel_id) {
                Some(m) => m,
                None => continue,
            };

            // Deduplicate
            let mention_key = format!("{}_{}", mention.channel, mention.msg_ts);
            {
                let mut seen_lock = seen.lock().await;
                if seen_lock.contains(&mention_key) { continue; }
                seen_lock.insert(mention_key);
                // Prune if too large
                if seen_lock.len() > 500 { seen_lock.clear(); }
            }

            let preview: String = mention.text.chars().take(80).collect();
            plog!("[Pawkit/Mention] @mention from {} in {}: {}", mention.user, mention.channel, preview);

            let current_mode = mode.lock().await.clone();
            let slack_clone = slack.clone();
            let config_clone = config.clone();
            let dm_user_id_clone = dm_user_id.clone();

            // Spawn handler so we don't block the listener
            tokio::spawn(async move {
                handle_mention(slack_clone, mention, current_mode, config_clone, &dm_user_id_clone).await;
            });
        }

        if !stop_flag.load(Ordering::SeqCst) {
            plog!("[Pawkit/Mention] Disconnected, reconnecting in 3s...");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}

// ── Event extraction ──

/// Extract a mention event from a Socket Mode envelope.
/// Returns Some if the message is in a channel (not DM) and mentions dm_user_id.
fn extract_mention_event(
    envelope: &serde_json::Value,
    dm_user_id: &str,
    bot_user_id: &str,
    dm_channel_id: &str,
) -> Option<MentionEvent> {
    let payload = envelope.get("payload")?;
    let event = payload.get("event")?;

    let event_type = event.get("type")?.as_str()?;
    if event_type != "message" { return None; }
    // Skip subtypes (message_changed, bot_message, etc.)
    if event.get("subtype").is_some() { return None; }
    // Skip messages from the bot itself
    let user = event.get("user")?.as_str()?;
    if user == bot_user_id { return None; }
    // Skip messages from the monitored user (don't reply to yourself)
    if user == dm_user_id { return None; }

    let channel = event.get("channel")?.as_str()?;
    // Skip DM channel — that's handled by the away mode handler
    if channel == dm_channel_id { return None; }

    let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");

    // Check if the message mentions the user
    let mention_tag = format!("<@{}>", dm_user_id);
    if !text.contains(&mention_tag) { return None; }

    let thread_ts = event.get("thread_ts").and_then(|v| v.as_str()).map(String::from);
    let msg_ts = event.get("ts")?.as_str()?.to_string();

    Some(MentionEvent {
        channel: channel.to_string(),
        thread_ts,
        msg_ts,
        user: user.to_string(),
        text: text.to_string(),
    })
}

// ── Mention handling ──

async fn handle_mention(
    slack: Arc<SlackBridge>,
    mention: MentionEvent,
    mode: MentionMode,
    config: SlackConfig,
    dm_user_id: &str,
) {
    match mode {
        MentionMode::Monitor => handle_mention_monitor(slack, mention, config, dm_user_id).await,
        MentionMode::AutoReply => handle_mention_auto_reply(slack, mention, config, dm_user_id).await,
        MentionMode::Rest => {} // shouldn't reach here
    }
}

/// Monitor mode: notify user via DM with approve/skip buttons
async fn handle_mention_monitor(
    slack: Arc<SlackBridge>,
    mention: MentionEvent,
    _config: SlackConfig,
    _dm_user_id: &str,
) {
    let sender_name = slack.get_user_name(&mention.user).await.unwrap_or_else(|_| mention.user.clone());
    let preview: String = mention.text.chars().take(200).collect();

    let request_id = format!("mention_{}_{}", mention.channel, mention.msg_ts);

    // Post notification with approve/skip buttons to DM
    let dm_channel = slack.get_dm_channel_id().lock().await.clone();
    let text = format!(
        "💬 *@mention from {}* in <#{}>\n```{}```",
        sender_name, mention.channel, preview
    );
    let blocks = serde_json::json!([
        {
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": format!("💬 *@mention from {}* in <#{}>\n```{}```", sender_name, mention.channel, preview)
            }
        },
        {
            "type": "actions",
            "block_id": format!("mention_{}", request_id),
            "elements": [
                {
                    "type": "button",
                    "text": { "type": "plain_text", "text": "🤖 Auto-reply" },
                    "action_id": "mention_reply",
                    "style": "primary",
                    "value": serde_json::json!({
                        "channel": mention.channel,
                        "thread_ts": mention.thread_ts,
                        "msg_ts": mention.msg_ts,
                        "text": mention.text,
                        "user": mention.user,
                    }).to_string()
                },
                {
                    "type": "button",
                    "text": { "type": "plain_text", "text": "⏭ Skip" },
                    "action_id": "mention_skip",
                    "value": request_id.clone()
                }
            ]
        }
    ]);

    let body = serde_json::json!({
        "channel": dm_channel,
        "text": text,
        "blocks": blocks,
    });

    if let Err(e) = slack.api_post_public("chat.postMessage", &body).await {
        plog!("[Pawkit/Mention] Failed to post approval request: {}", e);
    }
}

/// Auto-reply mode: fetch context, generate reply with Claude Sonnet, post back
async fn handle_mention_auto_reply(
    slack: Arc<SlackBridge>,
    mention: MentionEvent,
    config: SlackConfig,
    dm_user_id: &str,
) {
    let sender_name = slack.get_user_name(&mention.user).await.unwrap_or_else(|_| mention.user.clone());

    // Fetch thread/channel context
    let context = build_conversation_context(&slack, &mention).await;

    // Build prompt for Claude
    let prompt = build_reply_prompt(&sender_name, &mention.text, &context, dm_user_id);

    // Run Claude with Sonnet model
    let mut session = ClaudeSession::new_with_model(config.working_dir.clone(), "sonnet".to_string());
    let reply_thread_ts = mention.thread_ts.as_deref().unwrap_or(&mention.msg_ts);

    // Notify DM that we're auto-replying
    let preview: String = mention.text.chars().take(80).collect();
    let _ = slack.post_top_message(&format!(
        "🤖 自动回复 @mention from {} in <#{}>\n_{}_",
        sender_name, mention.channel, preview
    )).await;

    match session.run_prompt(&prompt).await {
        Ok(output) => {
            if output.is_error || output.text.is_empty() {
                plog!("[Pawkit/Mention] Claude returned error or empty: {}", output.text);
                let _ = slack.post_top_message(&format!(
                    "⚠️ 自动回复失败: {}",
                    if output.text.is_empty() { "无输出" } else { &output.text }
                )).await;
                return;
            }

            // Post reply in the original channel thread
            match slack.post_in_channel(&mention.channel, Some(reply_thread_ts), &output.text).await {
                Ok(_) => {
                    let cost_info = output.cost_usd.map(|c| format!(" 💰${:.4}", c)).unwrap_or_default();
                    let _ = slack.post_top_message(&format!("✅ 已回复{}", cost_info)).await;
                    plog!("[Pawkit/Mention] Auto-reply posted to {} thread {}", mention.channel, reply_thread_ts);
                }
                Err(e) => {
                    plog!("[Pawkit/Mention] Failed to post reply: {}", e);
                    let _ = slack.post_top_message(&format!("❌ 回复发送失败: {}", e)).await;
                }
            }
        }
        Err(e) => {
            plog!("[Pawkit/Mention] Claude execution failed: {}", e);
            let _ = slack.post_top_message(&format!("❌ Claude 执行失败: {}", e)).await;
        }
    }
}

/// Build conversation context from thread or channel history
async fn build_conversation_context(slack: &SlackBridge, mention: &MentionEvent) -> String {
    let messages = if let Some(ref thread_ts) = mention.thread_ts {
        slack.fetch_thread_messages(&mention.channel, thread_ts, 15).await.unwrap_or_default()
    } else {
        slack.fetch_channel_messages(&mention.channel, 10).await.unwrap_or_default()
    };

    if messages.is_empty() {
        return String::new();
    }

    let mut context_parts = Vec::new();
    for msg in &messages {
        let user = msg["user"].as_str().unwrap_or("unknown");
        let text = msg["text"].as_str().unwrap_or("");
        if !text.is_empty() {
            context_parts.push(format!("<@{}>: {}", user, text));
        }
    }

    context_parts.join("\n")
}

/// Build the prompt for Claude to generate a reply
fn build_reply_prompt(sender: &str, mention_text: &str, context: &str, user_id: &str) -> String {
    let mut prompt = format!(
        "You are acting as a helpful assistant replying on behalf of <@{}> in a Slack conversation. \
         Someone mentioned them and expects a response.\n\n\
         IMPORTANT RULES:\n\
         - Reply naturally as if you are the user's assistant\n\
         - Be concise and helpful\n\
         - Use the conversation context to understand what's being discussed\n\
         - If you don't have enough context to give a meaningful answer, say so politely\n\
         - Do NOT use markdown headers or code blocks unless the topic is technical\n\
         - Keep the tone professional but friendly\n\
         - Reply in the same language as the message\n\n",
        user_id
    );

    if !context.is_empty() {
        prompt.push_str(&format!("CONVERSATION CONTEXT:\n{}\n\n", context));
    }

    prompt.push_str(&format!(
        "MESSAGE FROM {} THAT MENTIONS YOU:\n{}\n\n\
         Generate a reply. Output ONLY the reply text, nothing else.",
        sender, mention_text
    ));

    prompt
}

/// Handle a mention_reply button click (from monitor mode approval).
/// Called by the Socket Mode button handler.
pub async fn handle_mention_reply_button(
    slack: Arc<SlackBridge>,
    value_json: &str,
    config: &SlackConfig,
    dm_user_id: &str,
) {
    let value: serde_json::Value = match serde_json::from_str(value_json) {
        Ok(v) => v,
        Err(e) => {
            plog!("[Pawkit/Mention] Failed to parse button value: {}", e);
            return;
        }
    };

    let channel = value["channel"].as_str().unwrap_or("").to_string();
    let thread_ts = value["thread_ts"].as_str().map(String::from);
    let msg_ts = value["msg_ts"].as_str().unwrap_or("").to_string();
    let text = value["text"].as_str().unwrap_or("").to_string();
    let user = value["user"].as_str().unwrap_or("").to_string();

    if channel.is_empty() || msg_ts.is_empty() { return; }

    let mention = MentionEvent {
        channel,
        thread_ts,
        msg_ts,
        user,
        text,
    };

    handle_mention_auto_reply(slack, mention, config.clone(), dm_user_id).await;
}
