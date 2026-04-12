mod auto_review;
pub mod cli;
mod config;
mod executor;
mod hook_server;
mod claude_session;
#[macro_use]
mod logger;
pub mod session_store;
mod slack_bridge;
mod win_focus;

use config::SharedConfig;
use executor::{execute_action, ActionResult};
use hook_server::{AuthDecision, PendingRequests, SessionAllowTools};

/// Wrapper so we can manage Arc<AtomicBool> as Tauri state for green light
struct GreenLightFlag(Arc<AtomicBool>);
use session_store::SessionStore;
use slack_bridge::SlackBridge;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, WebviewWindow,
};

const HOOK_SERVER_PORT: u16 = 9527;

/// Track which session was last focused, so clicking the same cat toggles (minimizes)
/// while clicking a different cat switches tabs.
static LAST_FOCUSED_SESSION: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Wrapper so we can manage Arc<AtomicBool> as Tauri state
struct AwayFlag(Arc<AtomicBool>);

#[tauri::command]
fn get_actions(state: tauri::State<SharedConfig>) -> Vec<config::Action> {
    let config = state.lock().unwrap();
    config
        .actions
        .actions
        .iter()
        .filter(|a| a.enabled)
        .cloned()
        .collect()
}

#[tauri::command]
fn get_pet_config(state: tauri::State<SharedConfig>) -> config::PetConfig {
    let config = state.lock().unwrap();
    config.pet.clone()
}

#[tauri::command]
async fn run_action(
    action_id: String,
    state: tauri::State<'_, SharedConfig>,
    app: tauri::AppHandle,
) -> Result<ActionResult, String> {
    let action = {
        let config = state.lock().unwrap();
        config
            .actions
            .actions
            .iter()
            .find(|a| a.id == action_id)
            .cloned()
    };

    let action = action.ok_or_else(|| format!("Action not found: {}", action_id))?;

    let _ = app.emit("action_started", &action.id);

    let result = tokio::task::spawn_blocking(move || execute_action(&action))
        .await
        .map_err(|e| format!("Task failed: {}", e))?;

    let _ = app.emit("action_finished", &result);

    Ok(result)
}

#[tauri::command]
async fn respond_auth(
    request_id: String,
    allow: bool,
    pending: tauri::State<'_, PendingRequests>,
) -> Result<bool, String> {
    let mut pending = pending.lock().await;
    if let Some(tx) = pending.remove(&request_id) {
        let decision = if allow {
            AuthDecision::Allow
        } else {
            AuthDecision::Deny
        };
        let _ = tx.send(decision);
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Allow this request AND auto-allow all future requests for the same tool type this session
#[tauri::command]
async fn respond_auth_all(
    request_id: String,
    tool_name: String,
    pending: tauri::State<'_, PendingRequests>,
    session_tools: tauri::State<'_, SessionAllowTools>,
) -> Result<bool, String> {
    // Add tool to session auto-allow list
    {
        let mut tools = session_tools.lock().await;
        if !tools.contains(&tool_name) {
            tools.push(tool_name);
        }
    }
    // Allow this request
    let mut pending = pending.lock().await;
    if let Some(tx) = pending.remove(&request_id) {
        let _ = tx.send(AuthDecision::Allow);
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
async fn approve_review_item(
    id: String,
    pending_reviews: tauri::State<'_, auto_review::PendingReviewItems>,
) -> Result<bool, String> {
    let item = {
        let mut items = pending_reviews.lock().await;
        let pos = items.iter().position(|i| i.id == id);
        pos.map(|p| items.remove(p))
    };
    if let Some(item) = item {
        if let Some(tx) = auto_review::get_approved_sender() {
            let _ = tx.send(item).await;
            return Ok(true);
        }
    }
    Ok(false)
}

#[tauri::command]
async fn skip_review_item(
    id: String,
    pending_reviews: tauri::State<'_, auto_review::PendingReviewItems>,
) -> Result<bool, String> {
    let mut items = pending_reviews.lock().await;
    if let Some(pos) = items.iter().position(|i| i.id == id) {
        let item = items.remove(pos);
        // Mark notification as read on skip
        if !item.notification_id.is_empty() {
            let notif_id = item.notification_id.clone();
            tauri::async_runtime::spawn(async move {
                auto_review::mark_notification_read_by_id(&notif_id).await;
            });
        }
        return Ok(true);
    }
    Ok(false)
}

#[tauri::command]
fn get_hook_port() -> u16 {
    HOOK_SERVER_PORT
}

#[tauri::command]
async fn get_active_sessions(
    active_sessions: tauri::State<'_, hook_server::ActiveSessions>,
) -> Result<Vec<hook_server::ActiveSessionInfo>, String> {
    let sessions = active_sessions.lock().await;
    Ok(sessions.values().cloned().collect())
}

#[tauri::command]
async fn focus_claude_terminal(
    session_id: Option<String>,
    active_sessions: tauri::State<'_, hook_server::ActiveSessions>,
) -> Result<bool, String> {
    plog!("[Pawkit] focus_claude_terminal called: session_id={:?}", session_id);
    if let Some(ref sid) = session_id {
        let sessions = active_sessions.lock().await;
        plog!("[Pawkit] active_sessions keys: {:?}", sessions.keys().collect::<Vec<_>>());
        if let Some(info) = sessions.get(sid.as_str()) {
            plog!("[Pawkit] found session: sid={} pid={:?}", sid, info.pid);
            if let Some(pid) = info.pid {
                let is_same = {
                    let last = LAST_FOCUSED_SESSION.lock().unwrap();
                    last.as_deref() == Some(sid.as_str())
                };
                let result = win_focus::focus_session_terminal(pid, is_same);
                if result {
                    *LAST_FOCUSED_SESSION.lock().unwrap() = Some(sid.clone());
                }
                return Ok(result);
            }
        }
    }
    Ok(win_focus::focus_claude_window())
}

#[tauri::command]
async fn trigger_check_pr(
    trigger: tauri::State<'_, auto_review::ManualPollTrigger>,
) -> Result<bool, String> {
    trigger.notify_one();
    Ok(true)
}

#[tauri::command]
async fn kill_session(
    session_id: String,
    active_sessions: tauri::State<'_, hook_server::ActiveSessions>,
    app: tauri::AppHandle,
) -> Result<bool, String> {
    let info = {
        let mut sessions = active_sessions.lock().await;
        sessions.remove(&session_id)
    };
    if let Some(info) = info {
        if let Some(pid) = info.pid {
            plog!("[Pawkit] Killing session {} (pid={})", session_id, pid);
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .spawn();
            }
        }
        let _ = app.emit("session_ended", &serde_json::json!({ "session_id": session_id }));
        Ok(true)
    } else {
        Ok(false)
    }
}

#[tauri::command]
fn reload_config(state: tauri::State<SharedConfig>) -> bool {
    let new_config = config::load_all_config();
    let mut config = state.lock().unwrap();
    *config = new_config;
    true
}

#[tauri::command]
async fn show_context_menu(
    window: WebviewWindow,
    session_id: Option<String>,
    state: tauri::State<'_, SharedConfig>,
    away_flag: tauri::State<'_, AwayFlag>,
    green_flag: tauri::State<'_, GreenLightFlag>,
    session_store_state: tauri::State<'_, Arc<tokio::sync::Mutex<SessionStore>>>,
    active_sessions: tauri::State<'_, hook_server::ActiveSessions>,
) -> Result<(), String> {
    let actions = {
        let config = state.lock().unwrap();
        config.actions.actions.clone()
    };
    let is_away = away_flag.0.load(Ordering::SeqCst);
    let is_green = green_flag.0.load(Ordering::SeqCst);

    let app = window.app_handle();

    // Build menu from actions config
    let mut menu_builder = MenuBuilder::new(app);

    // Away/Home toggle — show only the opposite of current state
    if is_away {
        let home_item = MenuItemBuilder::with_id("_pawkit_home", "🏠 回家了")
            .build(app)
            .map_err(|e| e.to_string())?;
        menu_builder = menu_builder.item(&home_item);
    } else {
        let away_item = MenuItemBuilder::with_id("_pawkit_away", "🏖 外出模式")
            .build(app)
            .map_err(|e| e.to_string())?;
        menu_builder = menu_builder.item(&away_item);
    }

    // Green light toggle
    if is_green {
        let item = MenuItemBuilder::with_id("_pawkit_green_off", "🔴 普通模式")
            .build(app)
            .map_err(|e| e.to_string())?;
        menu_builder = menu_builder.item(&item);
    } else {
        let item = MenuItemBuilder::with_id("_pawkit_green_on", "🟢 绿灯模式")
            .build(app)
            .map_err(|e| e.to_string())?;
        menu_builder = menu_builder.item(&item);
    }

    let check_pr_item = MenuItemBuilder::with_id("_pawkit_check_pr", "🔍 Check PR")
        .build(app)
        .map_err(|e| e.to_string())?;
    menu_builder = menu_builder.item(&check_pr_item);

    // Recent sessions section
    {
        let store = session_store_state.lock().await;
        let recent = store.recent(5);
        if !recent.is_empty() {
            menu_builder = menu_builder.separator();
            let header = MenuItemBuilder::with_id("_group_sessions", "  Recent Sessions  ")
                .enabled(false)
                .build(app)
                .map_err(|e| e.to_string())?;
            menu_builder = menu_builder.item(&header);
            for record in recent {
                let dir_name = std::path::Path::new(&record.working_dir)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let title = if record.title.is_empty() { &dir_name } else { &record.title };
                let label = format!("  {} ({})", title, dir_name);
                let item = MenuItemBuilder::with_id(
                    format!("_pawkit_resume_{}", record.session_id),
                    label,
                )
                .build(app)
                .map_err(|e| e.to_string())?;
                menu_builder = menu_builder.item(&item);
            }
        }
    }

    menu_builder = menu_builder.separator();

    // Group actions
    let mut groups: std::collections::BTreeMap<String, Vec<&config::Action>> = std::collections::BTreeMap::new();
    let mut ungrouped: Vec<&config::Action> = Vec::new();

    for action in &actions {
        if !action.enabled {
            continue;
        }
        if let Some(ref group) = action.group {
            groups.entry(group.clone()).or_default().push(action);
        } else {
            ungrouped.push(action);
        }
    }

    // Add ungrouped items first
    for action in &ungrouped {
        let label = format!("{} {}", action.icon.as_deref().unwrap_or(">"), action.name);
        let item = MenuItemBuilder::with_id(&action.id, label)
            .build(app)
            .map_err(|e| e.to_string())?;
        menu_builder = menu_builder.item(&item);
    }

    // Add grouped items with submenu headers
    for (group_name, group_actions) in &groups {
        if !ungrouped.is_empty() || groups.len() > 1 {
            menu_builder = menu_builder.separator();
        }
        // Add group label as disabled item
        let group_label = MenuItemBuilder::with_id(
            format!("_group_{}", group_name),
            format!("  {}  ", group_name),
        )
        .enabled(false)
        .build(app)
        .map_err(|e| e.to_string())?;
        menu_builder = menu_builder.item(&group_label);

        for action in group_actions {
            let label = format!("{} {}", action.icon.as_deref().unwrap_or(">"), action.name);
            let item = MenuItemBuilder::with_id(&action.id, label)
                .build(app)
                .map_err(|e| e.to_string())?;
            menu_builder = menu_builder.item(&item);
        }
    }

    // Add separator and Exit/Quit
    menu_builder = menu_builder.separator();
    let has_active_sessions = !active_sessions.lock().await.is_empty();
    if let Some(ref sid) = session_id {
        if has_active_sessions {
            // Right-clicking a specific cat with active session → "Exit Session"
            let item = MenuItemBuilder::with_id(
                format!("_pawkit_kill_{}", sid),
                "❌ 退出会话",
            )
            .build(app)
            .map_err(|e| e.to_string())?;
            menu_builder = menu_builder.item(&item);
        } else {
            let quit_item = MenuItemBuilder::with_id("_pawkit_quit", "退出 Pawkit")
                .build(app)
                .map_err(|e| e.to_string())?;
            menu_builder = menu_builder.item(&quit_item);
        }
    } else {
        let quit_item = MenuItemBuilder::with_id("_pawkit_quit", "退出 Pawkit")
            .build(app)
            .map_err(|e| e.to_string())?;
        menu_builder = menu_builder.item(&quit_item);
    }

    let menu = menu_builder.build().map_err(|e| e.to_string())?;

    window.popup_menu(&menu).map_err(|e| e.to_string())?;

    Ok(())
}

fn create_tray_menu<M: Manager<tauri::Wry>>(
    app: &M,
    config: &config::AppConfig,
) -> Result<tauri::menu::Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    let mut menu_builder = MenuBuilder::new(app);

    // Show/Hide pet
    let show = MenuItemBuilder::with_id("_tray_show", "显示宠物").build(app)?;
    let hide = MenuItemBuilder::with_id("_tray_hide", "隐藏宠物").build(app)?;
    menu_builder = menu_builder.item(&show).item(&hide).separator();

    // Group actions (same logic as context menu)
    let mut groups: std::collections::BTreeMap<String, Vec<&config::Action>> =
        std::collections::BTreeMap::new();
    let mut ungrouped: Vec<&config::Action> = Vec::new();

    for action in &config.actions.actions {
        if !action.enabled {
            continue;
        }
        if let Some(ref group) = action.group {
            groups.entry(group.clone()).or_default().push(action);
        } else {
            ungrouped.push(action);
        }
    }

    for action in &ungrouped {
        let label = format!("{} {}", action.icon.as_deref().unwrap_or(">"), action.name);
        let item = MenuItemBuilder::with_id(&action.id, label).build(app)?;
        menu_builder = menu_builder.item(&item);
    }

    for (group_name, group_actions) in &groups {
        if !ungrouped.is_empty() || groups.len() > 1 {
            menu_builder = menu_builder.separator();
        }
        let group_label = MenuItemBuilder::with_id(
            format!("_group_{}", group_name),
            format!("  {}  ", group_name),
        )
        .enabled(false)
        .build(app)?;
        menu_builder = menu_builder.item(&group_label);

        for action in group_actions {
            let label = format!("{} {}", action.icon.as_deref().unwrap_or(">"), action.name);
            let item = MenuItemBuilder::with_id(&action.id, label).build(app)?;
            menu_builder = menu_builder.item(&item);
        }
    }

    menu_builder = menu_builder.separator();
    let quit = MenuItemBuilder::with_id("_tray_quit", "退出 Pawkit").build(app)?;
    menu_builder = menu_builder.item(&quit);

    Ok(menu_builder.build()?)
}

fn build_tray_menu(
    app: &tauri::App,
    shared_config: SharedConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let menu = {
        let config = shared_config.lock().unwrap();
        create_tray_menu(app, &config)?
    };

    let tray_config = shared_config.clone();
    let _tray = TrayIconBuilder::with_id("main_tray")
        .icon(tauri::include_image!("icons/32x32.png"))
        .tooltip("Pawkit")
        .menu(&menu)
        .on_menu_event(move |app: &tauri::AppHandle, event| {
            let id = event.id().as_ref().to_string();
            match id.as_str() {
                "_tray_show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "_tray_hide" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.hide();
                    }
                }
                "_tray_quit" => {
                    app.exit(0);
                }
                // Action items are handled by app.on_menu_event() to avoid double execution
                _ => {}
            }
        })
        .build(app)?;

    Ok(())
}

/// Rebuild the tray menu after config changes
fn rebuild_tray_menu(app_handle: &tauri::AppHandle, shared_config: &SharedConfig) {
    // Build the menu while holding the lock, then release before set_menu().
    // set_menu() dispatches to the main thread — holding the lock here would
    // deadlock if the main thread is also waiting on the same mutex (e.g. in
    // on_menu_event).
    let menu_result = {
        let config = shared_config.lock().unwrap();
        create_tray_menu(app_handle, &config)
    };
    match menu_result {
        Ok(menu) => {
            if let Some(tray) = app_handle.tray_by_id("main_tray") {
                let _ = tray.set_menu(Some(menu));
            }
        }
        Err(e) => {
            plog!("[Pawkit] Failed to rebuild tray menu: {}", e);
        }
    }
}

/// Check if a process is still alive by PID.
fn is_process_alive(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| {
                let out = String::from_utf8_lossy(&o.stdout);
                out.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Use kill -0 to check if process exists
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn start_config_watcher(app_handle: tauri::AppHandle, shared_config: SharedConfig) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let config_dir = config::get_config_dir();

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    // Only react to YAML config files — ignore log files and other writes
                    let is_yaml = event.paths.iter().any(|p| {
                        p.extension().map_or(false, |ext| ext == "yaml" || ext == "yml")
                    });
                    if is_yaml {
                        let _ = tx.send(());
                    }
                }
            }
        })
        .expect("Failed to create file watcher");

        watcher
            .watch(config_dir.as_ref(), RecursiveMode::NonRecursive)
            .expect("Failed to watch config directory");

        loop {
            if rx.recv().is_ok() {
                // Debounce: drain any additional events within 500ms
                std::thread::sleep(std::time::Duration::from_millis(500));
                while rx.try_recv().is_ok() {}

                let new_config = config::load_all_config();
                {
                    let mut config = shared_config.lock().unwrap();
                    *config = new_config;
                }
                rebuild_tray_menu(&app_handle, &shared_config);
                let _ = app_handle.emit("config_changed", ());
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    logger::init();
    plog!("Pawkit starting...");

    config::seed_default_configs();
    let initial_config = config::load_all_config();
    let shared_config: SharedConfig = Arc::new(Mutex::new(initial_config));
    let pending_requests: PendingRequests =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    // Shared mode state
    let is_away = Arc::new(AtomicBool::new(false));
    let is_busy = Arc::new(AtomicBool::new(false));
    let auto_approve = Arc::new(AtomicBool::new(false));
    let green_light = Arc::new(AtomicBool::new(false));
    let session_allow_tools: SessionAllowTools = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let last_hook_activity: hook_server::LastHookActivity =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let active_sessions: hook_server::ActiveSessions =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let internal_pids: Arc<tokio::sync::Mutex<std::collections::HashSet<u32>>> =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
    let session_store: Arc<tokio::sync::Mutex<SessionStore>> =
        Arc::new(tokio::sync::Mutex::new(SessionStore::load()));
    let session_thread_map: slack_bridge::SessionThreadMap =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    // Load auto-review config
    let config_dir = config::get_config_dir();
    let auto_review_config = config::load_auto_review_config(&config_dir);
    let pending_review_items: auto_review::PendingReviewItems =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let manual_poll_trigger: auto_review::ManualPollTrigger =
        Arc::new(tokio::sync::Notify::new());

    // Load Slack config and create bridge (if configured)
    let slack_config = config::load_slack_config(&config_dir);
    let slack_bridge: Option<Arc<SlackBridge>> =
        if !slack_config.bot_token.is_empty() && !slack_config.dm_user_id.is_empty() {
            Some(Arc::new(SlackBridge::new(
                slack_config.bot_token.clone(),
                slack_config.app_token.clone(),
                slack_config.dm_user_id.clone(),
            )))
        } else {
            None
        };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .manage(shared_config.clone())
        .manage(pending_requests.clone())
        .manage(AwayFlag(is_away.clone()))
        .manage(GreenLightFlag(green_light.clone()))
        .manage(session_store.clone())
        .manage(active_sessions.clone())
        .manage(session_allow_tools.clone())
        .manage(pending_review_items.clone())
        .manage(manual_poll_trigger.clone())
        .invoke_handler(tauri::generate_handler![
            get_actions,
            get_pet_config,
            run_action,
            reload_config,
            show_context_menu,
            respond_auth,
            approve_review_item,
            skip_review_item,
            respond_auth_all,
            get_hook_port,
            focus_claude_terminal,
            trigger_check_pr,
            kill_session,
            get_active_sessions,
        ])
        .setup(move |app| {
            // Fix transparent window on Windows - clear both window and webview backgrounds
            let window = app.get_webview_window("main").unwrap();
            #[cfg(target_os = "windows")]
            {
                use tauri::window::Color;
                // Clear the window background
                let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));
                // Clear the webview background via window-vibrancy
                let _ = window_vibrancy::clear_blur(&window);

                // Set WS_EX_NOACTIVATE so clicking the pet doesn't steal focus
                // from fullscreen apps (e.g. video players). The window stays
                // always-on-top but never becomes the foreground window on click.
                unsafe {
                    let hwnd = window.hwnd().unwrap().0 as isize;
                    const GWL_EXSTYLE: i32 = -20;
                    const WS_EX_NOACTIVATE: isize = 0x08000000;
                    extern "system" {
                        fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
                        fn SetWindowLongPtrW(hwnd: isize, index: i32, new_long: isize) -> isize;
                    }
                    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_NOACTIVATE);
                }
            }

            build_tray_menu(app, shared_config.clone())?;

            let app_handle = app.handle().clone();
            start_config_watcher(app_handle.clone(), shared_config.clone());

            // Start the HTTP hook server with away-mode support
            hook_server::start_hook_server(
                app_handle.clone(),
                pending_requests.clone(),
                HOOK_SERVER_PORT,
                is_away.clone(),
                is_busy.clone(),
                slack_bridge.clone(),
                auto_approve.clone(),
                green_light.clone(),
                slack_config.critical_tools.clone(),
                session_store.clone(),
                session_allow_tools.clone(),
                last_hook_activity.clone(),
                active_sessions.clone(),
                internal_pids.clone(),
                session_thread_map.clone(),
            );

            // Scan for existing Claude sessions on startup
            {
                let scan_handle = app_handle.clone();
                let scan_sessions = active_sessions.clone();
                let scan_store = session_store.clone();
                let scan_internal = internal_pids.clone();
                tauri::async_runtime::spawn(async move {
                    // Short delay for internal setup; frontend pulls via get_active_sessions after this
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    hook_server::scan_existing_sessions(&scan_handle, &scan_sessions, &scan_store, &scan_internal).await;
                });
            }

            // Start auto-review polling
            auto_review::start_auto_review(
                app_handle.clone(),
                auto_review_config.clone(),
                pending_review_items.clone(),
                slack_bridge.clone(),
                is_away.clone(),
                manual_poll_trigger.clone(),
                session_thread_map.clone(),
            );

            // Poll foreground window to detect when user switches to Claude terminal
            let focus_handle = app_handle.clone();
            std::thread::spawn(move || {
                let mut was_focused = false;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    let is_focused = win_focus::is_claude_window_focused();
                    if is_focused && !was_focused {
                        let _ = focus_handle.emit("terminal_focused", ());
                    }
                    was_focused = is_focused;
                }
            });

            // F9: Session liveness polling — check every 10s if active session PIDs are still alive
            let liveness_handle = app_handle.clone();
            let liveness_sessions = active_sessions.clone();
            let liveness_activity = last_hook_activity.clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    let sessions: Vec<(String, Option<u32>)> = {
                        let s = liveness_sessions.blocking_lock();
                        s.values().map(|i| (i.session_id.clone(), i.pid)).collect()
                    };
                    for (sid, pid) in sessions {
                        let alive = pid.map_or(true, |pid| is_process_alive(pid));
                        if !alive {
                            plog!("[Pawkit] Session process dead: {} (pid={:?})", sid, pid);
                            liveness_sessions.blocking_lock().remove(&sid);
                            liveness_activity.blocking_lock().remove(&sid);
                            let _ = liveness_handle.emit("session_ended", &serde_json::json!({
                                "session_id": sid,
                            }));
                        }
                    }
                }
            });

            // F7: Stuck detection — check every 60s if any session has been inactive too long
            let stuck_handle = app_handle.clone();
            let stuck_activity = last_hook_activity.clone();
            let stuck_slack = slack_bridge.clone();
            let stuck_is_away = is_away.clone();
            let stuck_store = session_store.clone();
            std::thread::spawn(move || {
                // Track which sessions we've already notified about (debounce)
                let mut notified: std::collections::HashSet<String> = std::collections::HashSet::new();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(60));

                    let activity = stuck_activity.blocking_lock().clone();
                    let now = std::time::Instant::now();
                    let threshold = std::time::Duration::from_secs(10 * 60); // 10 minutes

                    for (sid, last) in &activity {
                        if now.duration_since(*last) > threshold && !notified.contains(sid) {
                            // Get session title for context
                            let title = {
                                let store = stuck_store.blocking_lock();
                                store.by_id(sid).map(|r| r.title.clone()).unwrap_or_else(|| sid[..8.min(sid.len())].to_string())
                            };
                            plog!("[Pawkit] Session possibly stuck: {} ({})", sid, title);
                            let _ = stuck_handle.emit("session_stuck", &serde_json::json!({
                                "session_id": sid,
                                "title": title,
                            }));

                            if stuck_is_away.load(Ordering::SeqCst) {
                                if let Some(ref slack) = stuck_slack {
                                    let s = slack.clone();
                                    let msg = format!("⚠️ [{}] 会话可能卡住了（10分钟无活动）\n回复 `!nudge` 尝试唤醒，或 `!kill` 终止", title);
                                    tauri::async_runtime::spawn(async move {
                                        let _ = s.reply(&msg).await;
                                    });
                                }
                            }
                            notified.insert(sid.clone());
                        }
                    }

                    // Clear notifications for sessions that have new activity
                    notified.retain(|sid| {
                        activity.get(sid).map_or(false, |last| now.duration_since(*last) > threshold)
                    });
                }
            });

            // Handle native context menu events (including away/home mode)
            let menu_config = shared_config.clone();
            let menu_is_away = is_away.clone();
            let menu_pending = pending_requests.clone();
            let menu_slack = slack_bridge.clone();
            let menu_auto = auto_approve.clone();
            let menu_green = green_light.clone();
            let menu_session_store = session_store.clone();
            let active_sessions_menu = active_sessions.clone();
            let menu_trigger = manual_poll_trigger.clone();
            let session_thread_map = session_thread_map.clone();
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref().to_string();

                // Tray show/hide/quit — handle here as well (tray on_menu_event may not fire)
                if id == "_tray_show" {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_always_on_top(true);
                    }
                    plog!("[Pawkit] Tray: show window");
                    return;
                }
                if id == "_tray_hide" {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.hide();
                    }
                    plog!("[Pawkit] Tray: hide window");
                    return;
                }
                if id == "_tray_quit" {
                    app.exit(0);
                    return;
                }
                if id == "_pawkit_quit" {
                    app.exit(0);
                    return;
                }
                if id == "_pawkit_check_pr" {
                    menu_trigger.notify_one();
                    plog!("[Pawkit] Manual check-pr triggered from menu");
                    return;
                }
                // Green light toggle
                if id == "_pawkit_green_on" {
                    menu_green.store(true, Ordering::SeqCst);
                    let _ = app.emit("green_light_changed", true);
                    plog!("[Pawkit] 绿灯模式已开启");
                    return;
                }
                if id == "_pawkit_green_off" {
                    menu_green.store(false, Ordering::SeqCst);
                    let _ = app.emit("green_light_changed", false);
                    plog!("[Pawkit] 绿灯模式已关闭");
                    return;
                }
                // Kill specific session
                if id.starts_with("_pawkit_kill_") {
                    let session_id = id.strip_prefix("_pawkit_kill_").unwrap().to_string();
                    let active_clone = active_sessions_menu.clone();
                    let app_clone = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let info = {
                            let mut sessions = active_clone.lock().await;
                            sessions.remove(&session_id)
                        };
                        if let Some(info) = info {
                            if let Some(pid) = info.pid {
                                plog!("[Pawkit] Killing session {} (pid={}) from menu", session_id, pid);
                                #[cfg(target_os = "windows")]
                                {
                                    use std::os::windows::process::CommandExt;
                                    const CREATE_NO_WINDOW: u32 = 0x08000000;
                                    let _ = std::process::Command::new("taskkill")
                                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                                        .creation_flags(CREATE_NO_WINDOW)
                                        .spawn();
                                }
                                #[cfg(not(target_os = "windows"))]
                                {
                                    let _ = std::process::Command::new("kill")
                                        .args(["-9", &pid.to_string()])
                                        .spawn();
                                }
                            }
                            let _ = app_clone.emit("session_ended", &serde_json::json!({
                                "session_id": session_id,
                            }));
                        }
                    });
                    return;
                }
                // Resume session
                if id.starts_with("_pawkit_resume_") {
                    let session_id = id.strip_prefix("_pawkit_resume_").unwrap().to_string();
                    let store_clone = menu_session_store.clone();
                    tauri::async_runtime::spawn(async move {
                        let working_dir = {
                            let store = store_clone.lock().await;
                            store.by_id(&session_id).map(|r| r.working_dir.clone()).unwrap_or_default()
                        };
                        if let Err(e) = executor::launch_resume_terminal(&session_id, &working_dir) {
                            plog!("[Pawkit] Failed to resume session: {}", e);
                        }
                    });
                    return;
                }
                if id.starts_with("_group_") {
                    return;
                }

                // Away/Home mode from context menu
                if id == "_pawkit_away" {
                    if menu_is_away.load(Ordering::SeqCst) {
                        return;
                    }
                    let config_dir = config::get_config_dir();
                    let slack_config = config::load_slack_config(&config_dir);
                    if slack_config.bot_token.is_empty() || slack_config.dm_user_id.is_empty() {
                        plog!("[Pawkit] Slack 未配置 bot_token 或 dm_user_id，无法进入外出模式");
                        return;
                    }
                    menu_is_away.store(true, Ordering::SeqCst);
                    let away_flag = menu_is_away.clone();
                    let pending_clone = menu_pending.clone();
                    let slack_clone = menu_slack.clone();
                    let store_clone = menu_session_store.clone();
                    let green_clone = menu_green.clone();
                    let active_clone = active_sessions_menu.clone();
                    let thread_map_clone = session_thread_map.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(slack) = slack_clone {
                            slack_bridge::run_remote_session(
                                slack, pending_clone, away_flag, slack_config,
                                store_clone, green_clone, active_clone, thread_map_clone,
                            ).await;
                        }
                    });
                    let _ = app.emit("mode_changed", "away");
                    plog!("[Pawkit] 已切换到外出模式");
                    return;
                }
                if id == "_pawkit_home" {
                    if !menu_is_away.load(Ordering::SeqCst) {
                        return;
                    }
                    menu_is_away.store(false, Ordering::SeqCst);
                    menu_auto.store(false, Ordering::SeqCst);
                    // Clear session→thread mappings (they're only valid during away mode)
                    let stm = session_thread_map.clone();
                    tauri::async_runtime::spawn(async move {
                        stm.lock().await.clear();
                    });
                    let _ = app.emit("mode_changed", "home");
                    plog!("[Pawkit] 已切换到回家模式");
                    return;
                }

                // Find and execute the action
                plog!("[menu_event] looking up action id={}", id);
                let action = {
                    let config = menu_config.lock().unwrap();
                    config.actions.actions.iter().find(|a| a.id == id).cloned()
                };

                if let Some(action) = action {
                    plog!("[menu_event] executing action: {} (type={})", action.id, action.action_type);
                    let app_handle = app.clone();
                    let _ = app_handle.emit("action_started", &action.id);
                    std::thread::spawn(move || {
                        let result = execute_action(&action);
                        plog!("[menu_event] action finished: {} success={}", result.action_id, result.success);
                        if !result.stderr.is_empty() {
                            plog!("[menu_event] stderr: {}", result.stderr);
                        }
                        let _ = app_handle.emit("action_finished", &result);
                    });
                } else {
                    plog!("[menu_event] action not found for id={}", id);
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
