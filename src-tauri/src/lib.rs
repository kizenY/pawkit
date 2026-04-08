mod auto_review;
mod config;
mod executor;
mod hook_server;
mod claude_session;
#[macro_use]
mod logger;
mod slack_bridge;
mod win_focus;

use config::SharedConfig;
use executor::{execute_action, ActionResult};
use hook_server::{AuthDecision, LastTerminalSession, PendingRequests, SessionAllowTools};
use slack_bridge::SlackBridge;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, WebviewWindow,
};

const HOOK_SERVER_PORT: u16 = 9527;

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
fn focus_claude_terminal() -> bool {
    win_focus::focus_claude_window()
}

#[tauri::command]
async fn trigger_check_pr(
    trigger: tauri::State<'_, auto_review::ManualPollTrigger>,
) -> Result<bool, String> {
    trigger.notify_one();
    Ok(true)
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
    state: tauri::State<'_, SharedConfig>,
    away_flag: tauri::State<'_, AwayFlag>,
) -> Result<(), String> {
    let actions = {
        let config = state.lock().unwrap();
        config.actions.actions.clone()
    };
    let is_away = away_flag.0.load(Ordering::SeqCst);

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
    let check_pr_item = MenuItemBuilder::with_id("_pawkit_check_pr", "🔍 Check PR")
        .build(app)
        .map_err(|e| e.to_string())?;
    menu_builder = menu_builder.item(&check_pr_item);
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

    // Add separator and Quit
    menu_builder = menu_builder.separator();
    let quit_item = MenuItemBuilder::with_id("_pawkit_quit", "退出 Pawkit")
        .build(app)
        .map_err(|e| e.to_string())?;
    menu_builder = menu_builder.item(&quit_item);

    let menu = menu_builder.build().map_err(|e| e.to_string())?;

    window.popup_menu(&menu).map_err(|e| e.to_string())?;

    Ok(())
}

fn build_tray_menu(
    app: &tauri::App,
) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItemBuilder::with_id("show", "Show Pawkit").build(app)?;
    let hide = MenuItemBuilder::with_id("hide", "Hide Pawkit").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&hide)
        .separator()
        .item(&quit)
        .build()?;

    let _tray = TrayIconBuilder::new()
        .icon(tauri::include_image!("icons/32x32.png"))
        .tooltip("Pawkit")
        .menu(&menu)
        .on_menu_event(move |app, event| {
            match event.id().as_ref() {
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "hide" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.hide();
                    }
                }
                "quit" => {
                    app.exit(0);
                }
                _ => {}
            }
        })
        .build(app)?;

    Ok(())
}

fn start_config_watcher(app_handle: tauri::AppHandle, shared_config: SharedConfig) {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let config_dir = config::get_config_dir();

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    let _ = tx.send(());
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
    let auto_approve = Arc::new(AtomicBool::new(false));
    let session_allow_tools: SessionAllowTools = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let last_terminal_session: LastTerminalSession = Arc::new(tokio::sync::Mutex::new(
        hook_server::load_last_terminal_session(),
    ));

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

            build_tray_menu(app)?;

            let app_handle = app.handle().clone();
            start_config_watcher(app_handle.clone(), shared_config.clone());

            // Start the HTTP hook server with away-mode support
            hook_server::start_hook_server(
                app_handle.clone(),
                pending_requests.clone(),
                HOOK_SERVER_PORT,
                is_away.clone(),
                slack_bridge.clone(),
                auto_approve.clone(),
                slack_config.critical_tools.clone(),
                last_terminal_session.clone(),
                session_allow_tools.clone(),
            );

            // Start auto-review polling
            auto_review::start_auto_review(
                app_handle.clone(),
                auto_review_config.clone(),
                pending_review_items.clone(),
                slack_bridge.clone(),
                is_away.clone(),
                manual_poll_trigger.clone(),
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

            // Handle native context menu events (including away/home mode)
            let menu_config = shared_config.clone();
            let menu_is_away = is_away.clone();
            let menu_pending = pending_requests.clone();
            let menu_slack = slack_bridge.clone();
            let menu_auto = auto_approve.clone();
            let menu_session = last_terminal_session.clone();
            let menu_trigger = manual_poll_trigger.clone();
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref().to_string();

                if id == "_pawkit_quit" {
                    app.exit(0);
                    return;
                }
                if id == "_pawkit_check_pr" {
                    menu_trigger.notify_one();
                    plog!("[Pawkit] Manual check-pr triggered from menu");
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
                    let session_clone = menu_session.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(slack) = slack_clone {
                            let initial_session = session_clone.lock().await.clone();
                            slack_bridge::run_remote_session(
                                slack, pending_clone, away_flag, slack_config, initial_session,
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
                    let _ = app.emit("mode_changed", "home");
                    plog!("[Pawkit] 已切换到回家模式");
                    return;
                }

                // Find and execute the action
                let action = {
                    let config = menu_config.lock().unwrap();
                    config.actions.actions.iter().find(|a| a.id == id).cloned()
                };

                if let Some(action) = action {
                    let app_handle = app.clone();
                    let _ = app_handle.emit("action_started", &action.id);
                    std::thread::spawn(move || {
                        let result = execute_action(&action);
                        let _ = app_handle.emit("action_finished", &result);
                    });
                }
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
