mod config;
mod executor;
mod hook_server;
mod claude_session;
mod slack_bridge;
mod win_focus;

use config::SharedConfig;
use executor::{execute_action, ActionResult};
use hook_server::{AuthDecision, LastTerminalSession, PendingRequests};
use slack_bridge::SlackBridge;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::TrayIconBuilder,
    Emitter, Manager, WebviewWindow,
};

const HOOK_SERVER_PORT: u16 = 9527;

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

#[tauri::command]
fn get_hook_port() -> u16 {
    HOOK_SERVER_PORT
}

#[tauri::command]
fn focus_claude_terminal() -> bool {
    win_focus::focus_claude_window()
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
) -> Result<(), String> {
    let actions = {
        let config = state.lock().unwrap();
        config.actions.actions.clone()
    };

    let app = window.app_handle();

    // Build menu from actions config
    let mut menu_builder = MenuBuilder::new(app);

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
    is_away: Arc<AtomicBool>,
    pending: PendingRequests,
    slack: Option<Arc<SlackBridge>>,
    auto_approve: Arc<AtomicBool>,
    last_terminal_session: LastTerminalSession,
) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItemBuilder::with_id("show", "Show Pawkit").build(app)?;
    let hide = MenuItemBuilder::with_id("hide", "Hide Pawkit").build(app)?;
    let away = MenuItemBuilder::with_id("away", "🏖 外出模式").build(app)?;
    let home = MenuItemBuilder::with_id("home", "🏠 回家了").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&hide)
        .separator()
        .item(&away)
        .item(&home)
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
                "away" => {
                    if is_away.load(Ordering::SeqCst) {
                        return; // Already in away mode
                    }

                    // Load slack config
                    let config_dir = config::get_config_dir();
                    let slack_config = config::load_slack_config(&config_dir);

                    if slack_config.bot_token.is_empty() || slack_config.dm_user_id.is_empty() {
                        eprintln!("[Pawkit] Slack 未配置 bot_token 或 dm_user_id，无法进入外出模式");
                        return;
                    }

                    is_away.store(true, Ordering::SeqCst);

                    let away_flag = is_away.clone();
                    let pending_clone = pending.clone();
                    let slack_clone = slack.clone();

                    let session_id_clone = last_terminal_session.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(slack) = slack_clone {
                            let initial_session = session_id_clone.lock().await.clone();
                            slack_bridge::run_remote_session(
                                slack,
                                pending_clone,
                                away_flag,
                                slack_config,
                                initial_session,
                            )
                            .await;
                        }
                    });

                    let _ = app.emit("mode_changed", "away");
                    println!("[Pawkit] 已切换到外出模式");
                }
                "home" => {
                    if !is_away.load(Ordering::SeqCst) {
                        return; // Already home
                    }
                    is_away.store(false, Ordering::SeqCst);
                    auto_approve.store(false, Ordering::SeqCst);
                    let _ = app.emit("mode_changed", "home");
                    println!("[Pawkit] 已切换到回家模式");
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
                println!("[Pawkit] Config reloaded");
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_config = config::load_all_config();
    let shared_config: SharedConfig = Arc::new(Mutex::new(initial_config));
    let pending_requests: PendingRequests =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    // Shared mode state
    let is_away = Arc::new(AtomicBool::new(false));
    let auto_approve = Arc::new(AtomicBool::new(false));
    let last_terminal_session: LastTerminalSession = Arc::new(tokio::sync::Mutex::new(
        hook_server::load_last_terminal_session(),
    ));

    // Load Slack config and create bridge (if configured)
    let config_dir = config::get_config_dir();
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
        .invoke_handler(tauri::generate_handler![
            get_actions,
            get_pet_config,
            run_action,
            reload_config,
            show_context_menu,
            respond_auth,
            get_hook_port,
            focus_claude_terminal,
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
            }

            build_tray_menu(
                app,
                is_away.clone(),
                pending_requests.clone(),
                slack_bridge.clone(),
                auto_approve.clone(),
                last_terminal_session.clone(),
            )?;

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

            // Handle native context menu events
            let menu_config = shared_config.clone();
            app.on_menu_event(move |app, event| {
                let id = event.id().as_ref().to_string();

                if id == "_pawkit_quit" {
                    app.exit(0);
                    return;
                }
                if id.starts_with("_group_") {
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
