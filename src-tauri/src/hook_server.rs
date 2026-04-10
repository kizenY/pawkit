use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{oneshot, Mutex};

use std::time::Instant;

use crate::plog;
use crate::claude_session::ClaudeSession;
use crate::session_store::{self, SessionRecord, SessionSource, SessionStore};
use crate::slack_bridge::{SessionThreadMap, SlackBridge};

/// Per-session last hook activity timestamps for stuck detection
pub type LastHookActivity = Arc<Mutex<HashMap<String, Instant>>>;

/// Active sessions currently tracked (have sent hooks recently)
pub type ActiveSessions = Arc<Mutex<HashMap<String, ActiveSessionInfo>>>;

#[derive(Debug, Clone, Serialize)]
pub struct ActiveSessionInfo {
    pub session_id: String,
    pub title: String,
    pub working_dir: String,
    pub pid: Option<u32>,
}

/// Pending auth requests waiting for user decision
pub type PendingRequests = Arc<Mutex<HashMap<String, oneshot::Sender<AuthDecision>>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookInput {
    /// The hook event name, e.g. "PreToolUse"
    #[serde(default)]
    pub hook_event_name: Option<String>,
    /// Tool name being requested
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Tool input parameters
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    /// Session ID if available
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResponse {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    #[serde(rename = "permissionDecision")]
    pub permission_decision: String,
}

/// Payload emitted to the frontend when auth is needed
#[derive(Debug, Clone, Serialize)]
pub struct AuthRequestPayload {
    pub request_id: String,
    pub tool_name: String,
    pub tool_input_summary: String,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AuthDecision {
    Allow,
    Deny,
}

/// Tools the user has chosen to auto-allow for this session ("Allow All")
pub type SessionAllowTools = Arc<Mutex<Vec<String>>>;

#[derive(Clone)]
struct AppState {
    pending: PendingRequests,
    app_handle: tauri::AppHandle,
    is_away: Arc<AtomicBool>,
    is_busy: Arc<AtomicBool>,
    slack: Option<Arc<SlackBridge>>,
    auto_approve: Arc<AtomicBool>,
    green_light: Arc<AtomicBool>,
    critical_tools: Vec<String>,
    session_store: Arc<Mutex<SessionStore>>,
    session_allow_tools: SessionAllowTools,
    last_hook_activity: LastHookActivity,
    active_sessions: ActiveSessions,
    /// PIDs of internal `claude -p` processes (LLM title gen) that should not spawn cats.
    /// Contains the shell PID; we check if a session's PID is a descendant.
    internal_pids: Arc<Mutex<HashSet<u32>>>,
    /// Maps Claude session_id → Slack thread_ts for per-session notification routing
    session_thread_map: SessionThreadMap,
}

/// Summarize tool input into a short readable string
fn summarize_tool_input(tool_name: &str, input: &Option<serde_json::Value>) -> String {
    let Some(val) = input else {
        return String::new();
    };

    match tool_name {
        "Bash" => {
            if let Some(cmd) = val.get("command").and_then(|v| v.as_str()) {
                let truncated: String = cmd.chars().take(120).collect();
                if cmd.len() > 120 {
                    format!("$ {}...", truncated)
                } else {
                    format!("$ {}", truncated)
                }
            } else {
                String::new()
            }
        }
        "Edit" | "Write" => {
            if let Some(path) = val.get("file_path").and_then(|v| v.as_str()) {
                format!("{}", path)
            } else {
                String::new()
            }
        }
        "Read" => {
            if let Some(path) = val.get("file_path").and_then(|v| v.as_str()) {
                format!("{}", path)
            } else {
                String::new()
            }
        }
        _ => {
            let s = val.to_string();
            let truncated: String = s.chars().take(100).collect();
            if s.len() > 100 {
                format!("{}...", truncated)
            } else {
                truncated
            }
        }
    }
}

/// Tools that are safe to auto-allow without user approval
const SAFE_TOOLS: &[&str] = &[
    "Read", "Glob", "Grep", "Agent", "Skill", "ToolSearch",
    "TaskCreate", "TaskUpdate", "TaskGet", "TaskList", "TaskOutput", "TaskStop",
    "EnterPlanMode", "ExitPlanMode", "WebSearch", "WebFetch",
    "ListMcpResourcesTool", "ReadMcpResourceTool",
];

fn is_safe_tool(tool_name: &str) -> bool {
    SAFE_TOOLS.iter().any(|&s| s == tool_name)
}

/// Explicitly allow — returns permissionDecision: "allow" so Claude Code
/// skips its own permission check and does NOT ask the user in the terminal.
fn make_allow_response() -> (StatusCode, Json<HookResponse>) {
    make_decision_response(AuthDecision::Allow)
}

fn make_decision_response(decision: AuthDecision) -> (StatusCode, Json<HookResponse>) {
    let permission = match decision {
        AuthDecision::Allow => "allow",
        AuthDecision::Deny => "deny",
    };
    (
        StatusCode::OK,
        Json(HookResponse {
            hook_specific_output: Some(HookSpecificOutput {
                hook_event_name: "PreToolUse".to_string(),
                permission_decision: permission.to_string(),
            }),
        }),
    )
}

/// Check if a Bash command is safe enough to auto-allow without bothering the user.
fn is_safe_bash_command(tool_input: &Option<serde_json::Value>) -> bool {
    let Some(val) = tool_input else { return false };
    let Some(cmd) = val.get("command").and_then(|v| v.as_str()) else { return false };

    let cmd = cmd.trim();

    // Extract the first token (the actual command) — handle pipes/chains later
    let first_token = cmd.split_whitespace().next().unwrap_or("");

    // Safe read-only commands
    const SAFE_COMMANDS: &[&str] = &[
        "ls", "dir", "cat", "head", "tail", "less", "more",
        "find", "fd", "tree", "file", "stat", "wc", "du", "df",
        "echo", "printf", "pwd", "env", "set", "hostname", "whoami", "date",
        "sort", "uniq", "tee", "diff", "comm", "tr", "cut", "paste", "column",
        "grep", "rg", "ag", "awk", "sed",  // read-only usage is safe
        "which", "where", "type", "command", "hash",
        "node", "python", "python3", "ruby", "java", "go", "rustc", "cargo",
        "npm", "npx", "pnpm", "yarn", "pip", "pip3",
        "git",  // handled more specifically below
        "curl", "wget",  // typically safe for reads
        "jq", "yq", "xq",
        "test", "[",
        "true", "false",
        "sleep",
    ];

    // Dangerous commands — never auto-allow
    const DANGEROUS_COMMANDS: &[&str] = &[
        "rm", "rmdir", "del", "rd",
        "mv", "move", "rename", "ren",
        "cp", "copy", "xcopy", "robocopy",
        "chmod", "chown", "chgrp",
        "kill", "taskkill", "pkill", "killall",
        "shutdown", "reboot", "halt",
        "mkfs", "fdisk", "format",
        "dd", "shred",
        "ssh", "scp", "rsync",
        "docker", "kubectl", "terraform",
        "sudo", "su", "runas",
        "powershell", "pwsh", "cmd",
        "reg",
    ];

    // Dangerous git subcommands
    const DANGEROUS_GIT: &[&str] = &[
        "push", "reset", "rebase", "merge", "cherry-pick",
        "checkout", "switch", "restore",
        "clean", "stash",
        "commit", "tag",
        "remote", "config",
    ];

    // Check dangerous first
    if DANGEROUS_COMMANDS.iter().any(|&c| first_token == c) {
        return false;
    }

    // Special handling for git — allow read-only subcommands
    if first_token == "git" {
        let sub = cmd.split_whitespace().nth(1).unwrap_or("");
        if DANGEROUS_GIT.iter().any(|&g| sub == g) {
            return false;
        }
        return true; // git status, git log, git diff, git branch, git show, etc.
    }

    // Check if it's in the safe list
    if SAFE_COMMANDS.iter().any(|&c| first_token == c) {
        // But check for pipe/chain to dangerous commands
        if cmd.contains("| rm") || cmd.contains("&& rm") || cmd.contains("; rm")
            || cmd.contains("> /") || cmd.contains(">> /")
        {
            return false;
        }
        return true;
    }

    false
}

/// Check if a PID is a descendant of any PID in the internal set.
/// Used to detect LLM title gen `claude -p` child processes.
fn is_internal_pid(pid: u32, internal_pids: &HashSet<u32>) -> bool {
    if internal_pids.is_empty() {
        return false;
    }
    let ancestors = crate::win_focus::get_ancestor_pids(pid);
    ancestors.iter().any(|a| internal_pids.contains(a))
}

/// Try to discover a new session. Called from every hook handler so that
/// a cat appears as soon as the first hook (UserPrompt, PreToolUse, Stop, etc.) arrives.
async fn try_discover_session(state: &AppState, session_id: &str) {
    // Track activity for stuck detection
    state.last_hook_activity.lock().await.insert(session_id.to_string(), Instant::now());

    let mut active = state.active_sessions.lock().await;
    if active.contains_key(session_id) {
        return; // Already tracked
    }

    // Resolve the PID — if it's a descendant of an internal LLM title gen process, skip.
    let pid = resolve_session_pid(session_id);
    if let Some(p) = pid {
        let internals = state.internal_pids.lock().await;
        if is_internal_pid(p, &internals) {
            plog!("[Pawkit] Skipping internal LLM title session: {} pid={}", session_id, p);
            return;
        }
    }

    let (title, needs_llm_title) = {
        let store = state.session_store.lock().await;
        match store.by_id(session_id).map(|r| r.title.clone()) {
            Some(t) => (t, false),
            None => (session_store::generate_title(session_id), true),
        }
    };

    let working_dir = {
        let store = state.session_store.lock().await;
        store.by_id(session_id)
            .map(|r| r.working_dir.clone())
            .filter(|w| !w.is_empty())
    }.unwrap_or_else(|| session_store::resolve_session_working_dir(session_id).unwrap_or_default());

    // If session store had empty working_dir but we resolved it, update the store
    if !working_dir.is_empty() {
        let mut store = state.session_store.lock().await;
        let needs_wd_update = store.by_id(session_id)
            .map(|r| r.working_dir.is_empty())
            .unwrap_or(false);
        if needs_wd_update {
            store.set_working_dir(session_id, &working_dir);
        }
    }

    let info = ActiveSessionInfo {
        session_id: session_id.to_string(),
        title: title.clone(),
        working_dir: working_dir.clone(),
        pid,
    };
    active.insert(session_id.to_string(), info.clone());
    plog!("[Pawkit] Session discovered: {} title={} wd={} pid={:?}", session_id, title, working_dir, pid);
    let _ = state.app_handle.emit("session_discovered", &info);

    if needs_llm_title {
        spawn_llm_title_refinement(
            session_id.to_string(),
            state.app_handle.clone(),
            state.active_sessions.clone(),
            state.session_store.clone(),
            state.internal_pids.clone(),
        );
    }
}

/// Handle Claude Code notification (task completed, etc.)
/// In away mode, forward the notification content to Slack.
async fn handle_notification(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    plog!("[Pawkit] Notification payload: {}", serde_json::to_string(&payload).unwrap_or_default());

    let session_id = payload.get("session_id").and_then(|v| v.as_str());
    if let Some(sid) = session_id {
        try_discover_session(&state, sid).await;
        if !state.is_away.load(Ordering::SeqCst) {
            upsert_terminal_session(&state.session_store, sid).await;
        }
    }

    // In away mode, forward notification content to the session's Slack thread
    if state.is_away.load(Ordering::SeqCst) {
        if let Some(ref slack) = state.slack {
            let message = payload.get("message").and_then(|v| v.as_str())
                .or_else(|| payload.get("result").and_then(|v| v.as_str()))
                .or_else(|| payload.get("text").and_then(|v| v.as_str()));

            if let Some(msg) = message {
                if !msg.is_empty() {
                    // F4: Enrich with session title
                    let session_prefix = if let Some(sid) = session_id {
                        let store = state.session_store.lock().await;
                        store.by_id(sid)
                            .map(|r| format!("[{}] ", r.title))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let notification = format!("🔔 {}{}", session_prefix, msg);

                    // Route to the session's thread if mapped, otherwise use default
                    let thread_ts = if let Some(sid) = session_id {
                        state.session_thread_map.lock().await.get(sid).cloned()
                    } else {
                        None
                    };
                    if let Some(ts) = thread_ts {
                        let _ = slack.reply_in_thread(&ts, &notification).await;
                    } else {
                        let _ = slack.reply(&notification).await;
                    }
                }
            }
        }
    }

    // Emit with session_id for multi-cat routing
    let sid_payload = serde_json::json!({ "session_id": session_id });
    let _ = state.app_handle.emit("claude_knock", &sid_payload);

    // Keep busy state alive during notifications
    if state.is_busy.load(Ordering::SeqCst) {
        let _ = state.app_handle.emit("claude_active", &sid_payload);
    }

    StatusCode::OK
}

async fn handle_pre_tool_use(
    State(state): State<AppState>,
    Json(input): Json<HookInput>,
) -> (StatusCode, Json<HookResponse>) {
    let tool_name = input.tool_name.clone().unwrap_or_else(|| "Unknown".into());

    // Signal that Claude Code is actively working (with session_id for multi-cat routing)
    state.is_busy.store(true, Ordering::SeqCst);
    let sid_payload = serde_json::json!({ "session_id": input.session_id });
    let _ = state.app_handle.emit("claude_active", &sid_payload);

    // Discover session + track activity
    if let Some(ref sid) = input.session_id {
        try_discover_session(&state, sid).await;
    }

    // Capture session ID only when NOT in away mode — this ensures we only
    // track terminal sessions, not Pawkit's own `claude -p` sessions from Slack.
    if let Some(ref sid) = input.session_id {
        if !state.is_away.load(Ordering::SeqCst) {
            plog!("[Pawkit] Captured terminal session_id: {} (tool={})", sid, tool_name);
            upsert_terminal_session(&state.session_store, sid).await;
        } else {
            plog!("[Pawkit] Ignoring session_id from away-mode: {} (tool={})", sid, tool_name);
        }
    }

    // Auto-allow safe tools without prompting
    if is_safe_tool(&tool_name) {
        return make_allow_response();
    }

    // Auto-allow safe Bash commands (ls, find, git status, etc.)
    if tool_name == "Bash" && is_safe_bash_command(&input.tool_input) {
        return make_allow_response();
    }

    // Auto-allow tools the user chose "Allow All" for this session
    {
        let session_tools = state.session_allow_tools.lock().await;
        if session_tools.iter().any(|t| t == &tool_name) {
            return make_allow_response();
        }
    }

    let summary = summarize_tool_input(&tool_name, &input.tool_input);

    // Resolve the Slack thread for this session (used by green light + away mode)
    let session_thread_ts = if let Some(ref sid) = input.session_id {
        state.session_thread_map.lock().await.get(sid).cloned()
    } else {
        None
    };

    // === Green light mode: auto-approve everything, just notify ===
    if state.green_light.load(Ordering::SeqCst) {
        if state.is_away.load(Ordering::SeqCst) {
            if let Some(ref slack) = state.slack {
                let s = slack.clone();
                let msg = format!("🟢 *{}*\n`{}`", tool_name, summary);
                let ts = session_thread_ts.clone();
                tokio::spawn(async move {
                    if let Some(ts) = ts {
                        let _ = s.reply_in_thread(&ts, &msg).await;
                    } else {
                        let _ = s.reply(&msg).await;
                    }
                });
            }
        }
        let _ = state.app_handle.emit("green_light_approved", &serde_json::json!({
            "session_id": input.session_id,
            "tool_name": tool_name,
            "summary": summary,
        }));
        return make_allow_response();
    }

    // === Away mode: route to Slack ===
    if state.is_away.load(Ordering::SeqCst) {
        let is_critical = state.critical_tools.iter().any(|t| t == &tool_name);

        // Non-critical tools: auto-allow in away mode
        if !is_critical {
            return make_allow_response();
        }

        // Auto-approve mode: allow everything
        if state.auto_approve.load(Ordering::SeqCst) {
            if let Some(ref slack) = state.slack {
                let msg = format!("🔓 自动允许: *{}*\n`{}`", tool_name, summary);
                if let Some(ref ts) = session_thread_ts {
                    let _ = slack.reply_in_thread(ts, &msg).await;
                } else {
                    let _ = slack.reply(&msg).await;
                }
            }
            return make_allow_response();
        }

        // Critical tool → post to Slack with buttons in the session's thread
        if let Some(ref slack) = state.slack {
            let request_id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = oneshot::channel::<AuthDecision>();

            {
                let mut pending = state.pending.lock().await;
                pending.insert(request_id.clone(), tx);
            }

            // Post auth request with Allow/Deny buttons in the session's thread
            if let Some(ref ts) = session_thread_ts {
                let _ = slack.post_auth_buttons_in_thread(ts, &tool_name, &summary, &request_id).await;
            } else {
                let _ = slack.post_auth_buttons(&tool_name, &summary, &request_id).await;
            }

            // Wait for decision (5 min timeout for remote)
            let decision =
                match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                    Ok(Ok(decision)) => decision,
                    _ => {
                        let mut pending = state.pending.lock().await;
                        pending.remove(&request_id);
                        let _ = slack.reply("⏰ 权限请求超时，已拒绝").await;
                        AuthDecision::Deny
                    }
                };

            return make_decision_response(decision);
        }

        // No slack configured but in away mode — deny for safety
        return make_decision_response(AuthDecision::Deny);
    }

    // === Home mode: emit to frontend (existing behavior) ===
    let request_id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel::<AuthDecision>();

    {
        let mut pending = state.pending.lock().await;
        pending.insert(request_id.clone(), tx);
    }

    let payload = AuthRequestPayload {
        request_id: request_id.clone(),
        tool_name: tool_name.clone(),
        tool_input_summary: summary,
        session_id: input.session_id.clone(),
    };
    let _ = state.app_handle.emit("claude_auth_request", &payload);

    // Wait for user decision (115s timeout — 5s less than Claude Code's 120s HTTP timeout
    // to ensure we always return a response before the HTTP connection drops)
    let decision = match tokio::time::timeout(std::time::Duration::from_secs(115), rx).await {
        Ok(Ok(decision)) => decision,
        _ => {
            let mut pending = state.pending.lock().await;
            pending.remove(&request_id);
            AuthDecision::Deny
        }
    };

    make_decision_response(decision)
}

/// Handle Stop — Claude finished responding
async fn handle_stop(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    plog!("[Pawkit] Stop hook fired");
    state.is_busy.store(false, Ordering::SeqCst);

    let session_id = payload.get("session_id").and_then(|v| v.as_str());

    // Safety net: discover session even if UserPrompt/PreToolUse were missed
    if let Some(sid) = session_id {
        try_discover_session(&state, sid).await;
    }

    let sid_payload = serde_json::json!({ "session_id": session_id });

    // Always emit claude_stopped (both home and away mode) for busy detection
    let _ = state.app_handle.emit("claude_stopped", &sid_payload);

    // claude_task_done (bell notification) only in home mode
    if !state.is_away.load(Ordering::SeqCst) {
        let _ = state.app_handle.emit("claude_task_done", &sid_payload);
    }

    // F1: Clear Slack "thinking" status in away mode (per-session thread)
    if state.is_away.load(Ordering::SeqCst) {
        if let Some(ref slack) = state.slack {
            let s = slack.clone();
            let thread_ts = if let Some(sid) = session_id {
                state.session_thread_map.lock().await.get(sid).cloned()
            } else {
                None
            };
            tokio::spawn(async move {
                if let Some(ts) = thread_ts {
                    let _ = s.set_status_in_thread(&ts, "").await;
                } else {
                    let _ = s.clear_status().await;
                }
            });
        }
    }
    StatusCode::OK
}

/// Handle UserPromptSubmit — user just pressed enter, Claude starts thinking
async fn handle_user_prompt(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    state.is_busy.store(true, Ordering::SeqCst);
    let session_id = payload.get("session_id").and_then(|v| v.as_str());
    let sid_payload = serde_json::json!({ "session_id": session_id });
    let _ = state.app_handle.emit("claude_active", &sid_payload);

    // Discover session on first prompt (earliest hook a new session fires)
    if let Some(sid) = session_id {
        try_discover_session(&state, sid).await;
        if !state.is_away.load(Ordering::SeqCst) {
            upsert_terminal_session(&state.session_store, sid).await;
        }
    }

    // F1: Set Slack "thinking" status in away mode (per-session thread)
    if state.is_away.load(Ordering::SeqCst) {
        if let Some(ref slack) = state.slack {
            let s = slack.clone();
            let thread_ts = if let Some(sid) = session_id {
                state.session_thread_map.lock().await.get(sid).cloned()
            } else {
                None
            };
            tokio::spawn(async move {
                if let Some(ts) = thread_ts {
                    let _ = s.set_status_in_thread(&ts, "🤔 thinking...").await;
                } else {
                    let _ = s.set_status("🤔 thinking...").await;
                }
            });
        }
    }
    StatusCode::OK
}

/// Debug: emit a test event by name
async fn handle_test_emit(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    let event = payload.get("event").and_then(|v| v.as_str()).unwrap_or("");
    let data = payload.get("data").cloned().unwrap_or(serde_json::Value::Null);
    plog!("[Pawkit] Test emit: event={} data={}", event, data);
    let _ = state.app_handle.emit(event, data);
    StatusCode::OK
}

/// Start the HTTP hook server on the given port
pub fn start_hook_server(
    app_handle: tauri::AppHandle,
    pending: PendingRequests,
    port: u16,
    is_away: Arc<AtomicBool>,
    is_busy: Arc<AtomicBool>,
    slack: Option<Arc<SlackBridge>>,
    auto_approve: Arc<AtomicBool>,
    green_light: Arc<AtomicBool>,
    critical_tools: Vec<String>,
    session_store: Arc<Mutex<SessionStore>>,
    session_allow_tools: SessionAllowTools,
    last_hook_activity: LastHookActivity,
    active_sessions: ActiveSessions,
    internal_pids: Arc<Mutex<HashSet<u32>>>,
    session_thread_map: SessionThreadMap,
) {
    let state = AppState {
        pending,
        app_handle,
        is_away,
        is_busy,
        slack,
        auto_approve,
        green_light,
        critical_tools,
        session_store,
        session_allow_tools,
        last_hook_activity,
        active_sessions,
        internal_pids,
        session_thread_map,
    };

    let app = Router::new()
        .route("/hook/pre-tool-use", post(handle_pre_tool_use))
        .route("/hook/notification", post(handle_notification))
        .route("/hook/stop", post(handle_stop))
        .route("/hook/user-prompt", post(handle_user_prompt))
        .route("/hook/test-emit", post(handle_test_emit))
        .with_state(state);

    tauri::async_runtime::spawn(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                plog!("[Pawkit] Failed to bind hook server on port {}: {}", port, e);
                return;
            }
        };
        plog!("[Pawkit] Hook server listening on http://127.0.0.1:{}", port);
        if let Err(e) = axum::serve(listener, app).await {
            plog!("[Pawkit] Hook server error: {}", e);
        }
    });
}

/// Resolve the PID of a Claude Code session by scanning ~/.claude/sessions/*.json
fn resolve_session_pid(session_id: &str) -> Option<u32> {
    let sessions_dir = dirs::home_dir()?.join(".claude").join("sessions");
    if !sessions_dir.exists() {
        return None;
    }
    for entry in std::fs::read_dir(&sessions_dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if json.get("sessionId").and_then(|v| v.as_str()) == Some(session_id) {
                    // The filename is {pid}.json
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(pid) = stem.parse::<u32>() {
                            return Some(pid);
                        }
                    }
                    // Also try the pid field
                    if let Some(pid) = json.get("pid").and_then(|v| v.as_u64()) {
                        return Some(pid as u32);
                    }
                }
            }
        }
    }
    None
}

/// Upsert a terminal session into the session store (called from hook handlers).
async fn upsert_terminal_session(store: &Arc<Mutex<SessionStore>>, session_id: &str) {
    let mut store = store.lock().await;
    if let Some(record) = store.by_id(session_id) {
        // Backfill empty working_dir if we can resolve it now
        if record.working_dir.is_empty() {
            if let Some(wd) = session_store::resolve_session_working_dir(session_id) {
                plog!("[Pawkit] Backfilling working_dir for {}: {}", session_id, wd);
                store.set_working_dir(session_id, &wd);
            }
        }
        store.touch_and_save(session_id);
    } else {
        let title = session_store::generate_title(session_id);
        let working_dir = session_store::resolve_session_working_dir(session_id).unwrap_or_default();
        let now = chrono::Utc::now().timestamp_millis();
        plog!("[Pawkit] New terminal session: {} title={} cwd={}", session_id, title, working_dir);
        store.upsert(SessionRecord {
            session_id: session_id.to_string(),
            title,
            working_dir,
            created_at: now,
            last_active: now,
            source: SessionSource::Terminal,
            slack_thread_ts: None,
            total_cost_usd: 0.0,
        });
    }
}

/// Scan for already-running Claude Code sessions on startup.
/// Reads ~/.claude/sessions/*.json, checks PID liveness, and emits session_discovered for each.
pub async fn scan_existing_sessions(
    app_handle: &tauri::AppHandle,
    active_sessions: &ActiveSessions,
    session_store: &Arc<Mutex<SessionStore>>,
    internal_pids: &Arc<Mutex<HashSet<u32>>>,
) {
    let sessions_dir = match dirs::home_dir() {
        Some(h) => h.join(".claude").join("sessions"),
        None => return,
    };
    if !sessions_dir.exists() {
        return;
    }

    let entries = match std::fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        // Extract PID from filename (e.g., "12345.json")
        let pid: Option<u32> = path.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse().ok());

        // Check if the process is alive
        let alive = pid.map_or(false, |p| crate::is_process_alive(p));
        if !alive {
            continue;
        }

        // Skip internal LLM title gen processes
        // (At startup, internal_pids is empty so this is a no-op — correct behavior
        // since LLM title gen processes won't survive a restart)

        // Read the session file to get session_id
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let session_id = match json.get("sessionId").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        // Check if already tracked
        let mut active = active_sessions.lock().await;
        if active.contains_key(&session_id) {
            continue;
        }

        // Generate title and working dir
        let (title, needs_llm_title) = {
            let store = session_store.lock().await;
            match store.by_id(&session_id).map(|r| r.title.clone()) {
                Some(t) => (t, false),
                None => (session_store::generate_title(&session_id), true),
            }
        };

        let working_dir = {
            let store = session_store.lock().await;
            store.by_id(&session_id)
                .map(|r| r.working_dir.clone())
                .filter(|w| !w.is_empty())
        }.unwrap_or_else(|| session_store::resolve_session_working_dir(&session_id).unwrap_or_default());

        // If session store had empty working_dir but we resolved it, update the store
        if !working_dir.is_empty() {
            let mut store = session_store.lock().await;
            let needs_wd_update = store.by_id(&session_id)
                .map(|r| r.working_dir.is_empty())
                .unwrap_or(false);
            if needs_wd_update {
                store.set_working_dir(&session_id, &working_dir);
            }
        }

        let info = ActiveSessionInfo {
            session_id: session_id.clone(),
            title: title.clone(),
            working_dir: working_dir.clone(),
            pid,
        };
        active.insert(session_id.clone(), info.clone());
        plog!("[Pawkit] Startup scan: found session {} title={} wd={} pid={:?}", session_id, title, working_dir, pid);
        let _ = app_handle.emit("session_discovered", &info);

        // Spawn LLM title refinement in background
        if needs_llm_title {
            spawn_llm_title_refinement(
                session_id,
                app_handle.clone(),
                active_sessions.clone(),
                session_store.clone(),
                internal_pids.clone(),
            );
        }
    }
}

/// Spawn a background task to refine a session title using Claude Code LLM.
fn spawn_llm_title_refinement(
    session_id: String,
    app_handle: tauri::AppHandle,
    active_sessions: ActiveSessions,
    session_store: Arc<Mutex<SessionStore>>,
    internal_pids: Arc<Mutex<HashSet<u32>>>,
) {
    tokio::spawn(async move {
        // JSONL may not exist yet when the session is first discovered.
        // Retry a few times with increasing delay.
        let mut prompt_text = None;
        for delay_secs in [2, 4, 8] {
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            if let Some(p) = session_store::read_first_user_prompt(&session_id) {
                prompt_text = Some(p);
                break;
            }
        }
        let prompt_text = match prompt_text {
            Some(p) => p,
            None => {
                plog!("[Pawkit] LLM title: no user prompt found for {} after retries", &session_id[..session_id.len().min(8)]);
                return;
            }
        };
        match generate_llm_title(&prompt_text, internal_pids).await {
            Some(title) => {
                plog!("[Pawkit] LLM title for {}: {}", &session_id[..session_id.len().min(8)], title);
                // Update active session info
                if let Some(info) = active_sessions.lock().await.get_mut(&session_id) {
                    info.title = title.clone();
                }
                // Persist to session store
                session_store.lock().await.set_title(&session_id, &title);
                // Notify frontend
                let _ = app_handle.emit("session_title_updated", &serde_json::json!({
                    "session_id": session_id,
                    "title": title,
                }));
            }
            None => {
                plog!("[Pawkit] LLM title generation failed for {}, keeping heuristic", &session_id[..session_id.len().min(8)]);
            }
        }
    });
}

/// Use Claude Code CLI to generate a concise session title from the first user prompt.
/// Registers the child PID in `internal_pids` so hooks from it are ignored by session discovery.
async fn generate_llm_title(
    prompt_text: &str,
    internal_pids: Arc<Mutex<HashSet<u32>>>,
) -> Option<String> {
    // Truncate to avoid sending too much context
    let truncated: String = prompt_text.chars().take(800).collect();
    let meta_prompt = format!(
        "根据以下编程会话的首条用户消息，生成一个简短的标题（不超过20个字符，中英文均可）。只输出标题本身，不要加引号或解释。\n\n{}",
        truncated
    );

    let working_dir = std::env::temp_dir().to_string_lossy().to_string();
    let mut session = ClaudeSession::new(working_dir);

    // Track the child PID so hooks from this process are filtered
    let pids_clone = internal_pids.clone();
    let tracked_pid = Arc::new(std::sync::Mutex::new(None::<u32>));
    let tracked_clone = tracked_pid.clone();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        session.run_prompt_tracked(&meta_prompt, move |pid| {
            plog!("[Pawkit] LLM title gen: registered internal pid={}", pid);
            *tracked_clone.lock().unwrap() = Some(pid);
            if let Ok(mut set) = pids_clone.try_lock() {
                set.insert(pid);
            }
        }),
    ).await;

    // Clean up tracked PID
    let cleanup_pid = *tracked_pid.lock().unwrap();
    if let Some(pid) = cleanup_pid {
        internal_pids.lock().await.remove(&pid);
    }

    match result {
        Ok(Ok(output)) => {
            let title = output.text.trim().trim_matches('"').trim().to_string();
            if !title.is_empty() && title.chars().count() <= 50 {
                Some(title)
            } else {
                None
            }
        }
        Ok(Err(e)) => {
            plog!("[Pawkit] LLM title error: {}", e);
            None
        }
        Err(_) => {
            plog!("[Pawkit] LLM title generation timed out");
            None
        }
    }
}
