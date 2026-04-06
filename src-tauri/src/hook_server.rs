use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::{oneshot, Mutex};

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

#[derive(Clone)]
struct AppState {
    pending: PendingRequests,
    app_handle: tauri::AppHandle,
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

async fn handle_pre_tool_use(
    State(state): State<AppState>,
    Json(input): Json<HookInput>,
) -> (StatusCode, Json<HookResponse>) {
    let tool_name = input.tool_name.clone().unwrap_or_else(|| "Unknown".into());

    // Auto-allow safe tools without prompting the user
    if is_safe_tool(&tool_name) {
        return (
            StatusCode::OK,
            Json(HookResponse {
                hook_specific_output: None,
            }),
        );
    }

    let summary = summarize_tool_input(&tool_name, &input.tool_input);
    let request_id = uuid::Uuid::new_v4().to_string();

    // Create a oneshot channel to wait for user decision
    let (tx, rx) = oneshot::channel::<AuthDecision>();

    // Store the sender
    {
        let mut pending = state.pending.lock().await;
        pending.insert(request_id.clone(), tx);
    }

    // Emit event to frontend
    let payload = AuthRequestPayload {
        request_id: request_id.clone(),
        tool_name: tool_name.clone(),
        tool_input_summary: summary,
    };
    let _ = state.app_handle.emit("claude_auth_request", &payload);

    // Wait for user decision (with timeout)
    let decision = match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
        Ok(Ok(decision)) => decision,
        _ => {
            // Timeout or channel dropped - clean up and deny
            let mut pending = state.pending.lock().await;
            pending.remove(&request_id);
            AuthDecision::Deny
        }
    };

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

/// Start the HTTP hook server on the given port
pub fn start_hook_server(app_handle: tauri::AppHandle, pending: PendingRequests, port: u16) {
    let state = AppState {
        pending,
        app_handle,
    };

    let app = Router::new()
        .route("/hook/pre-tool-use", post(handle_pre_tool_use))
        .with_state(state);

    tauri::async_runtime::spawn(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[Pawkit] Failed to bind hook server on port {}: {}", port, e);
                return;
            }
        };
        println!("[Pawkit] Hook server listening on http://127.0.0.1:{}", port);
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[Pawkit] Hook server error: {}", e);
        }
    });
}
