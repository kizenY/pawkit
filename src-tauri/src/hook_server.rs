use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{oneshot, Mutex};

use crate::plog;
use crate::slack_bridge::SlackBridge;

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
}

#[derive(Debug, Clone)]
pub enum AuthDecision {
    Allow,
    Deny,
}

/// Last known terminal session info (session ID + working directory)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSession {
    pub session_id: String,
    pub working_dir: String,
}

pub type LastTerminalSession = Arc<Mutex<Option<TerminalSession>>>;

/// Tools the user has chosen to auto-allow for this session ("Allow All")
pub type SessionAllowTools = Arc<Mutex<Vec<String>>>;

#[derive(Clone)]
struct AppState {
    pending: PendingRequests,
    app_handle: tauri::AppHandle,
    is_away: Arc<AtomicBool>,
    slack: Option<Arc<SlackBridge>>,
    auto_approve: Arc<AtomicBool>,
    critical_tools: Vec<String>,
    last_terminal_session: LastTerminalSession,
    session_allow_tools: SessionAllowTools,
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

/// Handle Claude Code notification (task completed, etc.)
/// In away mode, forward the notification content to Slack.
async fn handle_notification(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    plog!("[Pawkit] Notification payload: {}", serde_json::to_string(&payload).unwrap_or_default());

    // Capture session ID from notifications too — but only from terminal sessions
    let session_id = payload.get("session_id").and_then(|v| v.as_str());
    if let Some(sid) = session_id {
        if !state.is_away.load(Ordering::SeqCst) {
            save_last_terminal_session(sid);
            *state.last_terminal_session.lock().await = load_last_terminal_session();
        }
    }

    // In away mode, forward notification content to Slack
    if state.is_away.load(Ordering::SeqCst) {
        if let Some(ref slack) = state.slack {
            // Try to extract useful text from the notification
            // Claude Code may send: message, result, or other fields
            let message = payload.get("message").and_then(|v| v.as_str())
                .or_else(|| payload.get("result").and_then(|v| v.as_str()))
                .or_else(|| payload.get("text").and_then(|v| v.as_str()));

            if let Some(msg) = message {
                if !msg.is_empty() {
                    let _ = slack.reply(&format!("🔔 {}", msg)).await;
                }
            }
        }
    }

    let _ = state.app_handle.emit("claude_knock", ());
    StatusCode::OK
}

async fn handle_pre_tool_use(
    State(state): State<AppState>,
    Json(input): Json<HookInput>,
) -> (StatusCode, Json<HookResponse>) {
    let tool_name = input.tool_name.clone().unwrap_or_else(|| "Unknown".into());

    // Signal that Claude Code is actively working
    let _ = state.app_handle.emit("claude_active", ());

    // Capture session ID only when NOT in away mode — this ensures we only
    // track terminal sessions, not Pawkit's own `claude -p` sessions from Slack.
    if let Some(ref sid) = input.session_id {
        if !state.is_away.load(Ordering::SeqCst) {
            plog!("[Pawkit] Captured terminal session_id: {} (tool={})", sid, tool_name);
            save_last_terminal_session(sid);
            *state.last_terminal_session.lock().await = load_last_terminal_session();
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
                let _ = slack
                    .reply(&format!("🔓 自动允许: *{}*\n`{}`", tool_name, summary))
                    .await;
            }
            return make_allow_response();
        }

        // Critical tool → post to Slack with buttons and wait for user decision
        if let Some(ref slack) = state.slack {
            let request_id = uuid::Uuid::new_v4().to_string();
            let (tx, rx) = oneshot::channel::<AuthDecision>();

            {
                let mut pending = state.pending.lock().await;
                pending.insert(request_id.clone(), tx);
            }

            // Post auth request with Allow/Deny buttons
            let _ = slack.post_auth_buttons(&tool_name, &summary, &request_id).await;

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
    if state.is_away.load(Ordering::SeqCst) {
        return StatusCode::OK;
    }
    let _ = state.app_handle.emit("claude_task_done", ());
    StatusCode::OK
}

/// Handle UserPromptSubmit — user just pressed enter, Claude starts thinking
async fn handle_user_prompt(
    State(state): State<AppState>,
) -> StatusCode {
    let _ = state.app_handle.emit("claude_active", ());
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
    slack: Option<Arc<SlackBridge>>,
    auto_approve: Arc<AtomicBool>,
    critical_tools: Vec<String>,
    last_terminal_session: LastTerminalSession,
    session_allow_tools: SessionAllowTools,
) {
    let state = AppState {
        pending,
        app_handle,
        is_away,
        slack,
        auto_approve,
        critical_tools,
        last_terminal_session,
        session_allow_tools,
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

/// File path for persisting the last terminal session across restarts
fn session_file_path() -> std::path::PathBuf {
    crate::config::get_config_dir().join(".last_terminal_session.json")
}

/// Persist the last terminal session ID to disk
fn save_last_terminal_session(session_id: &str) {
    // Resolve working_dir from Claude Code session files
    let working_dir = resolve_session_working_dir(session_id).unwrap_or_default();
    let ts = TerminalSession {
        session_id: session_id.to_string(),
        working_dir,
    };
    match serde_json::to_string(&ts) {
        Ok(json) => {
            if let Err(e) = std::fs::write(session_file_path(), &json) {
                plog!("[Pawkit] Failed to persist session: {}", e);
            } else {
                plog!("[Pawkit] Persisted terminal session: {}", json);
            }
        }
        Err(e) => plog!("[Pawkit] Failed to serialize session: {}", e),
    }
}

/// Load the last terminal session from disk (survives app restarts)
pub fn load_last_terminal_session() -> Option<TerminalSession> {
    match std::fs::read_to_string(session_file_path()) {
        Ok(s) if !s.trim().is_empty() => {
            match serde_json::from_str::<TerminalSession>(s.trim()) {
                Ok(ts) => {
                    plog!("[Pawkit] Loaded persisted terminal session: {} (cwd={})", ts.session_id, ts.working_dir);
                    Some(ts)
                }
                Err(e) => {
                    plog!("[Pawkit] Failed to parse persisted session: {}", e);
                    None
                }
            }
        }
        _ => None,
    }
}

/// Find the working directory for a session by scanning Claude Code's session files.
/// Sessions are stored at ~/.claude/projects/{project_slug}/{session_id}.jsonl
/// and the first user message line contains a "cwd" field.
fn resolve_session_working_dir(session_id: &str) -> Option<String> {
    let claude_dir = dirs::home_dir()?.join(".claude").join("projects");
    if !claude_dir.exists() {
        return None;
    }

    // Scan all project directories for the session file
    let filename = format!("{}.jsonl", session_id);
    for entry in std::fs::read_dir(&claude_dir).ok()? {
        let entry = entry.ok()?;
        if entry.file_type().ok()?.is_dir() {
            let session_file = entry.path().join(&filename);
            if session_file.exists() {
                // Read the first few lines to find a "cwd" field
                if let Ok(content) = std::fs::read_to_string(&session_file) {
                    for line in content.lines().take(10) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                            if let Some(cwd) = json.get("cwd").and_then(|v| v.as_str()) {
                                plog!("[Pawkit] Resolved session {} cwd: {}", session_id, cwd);
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
