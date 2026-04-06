mod config;
mod executor;
mod hook_server;

use config::SharedConfig;
use executor::{execute_action, ActionResult};
use hook_server::{AuthDecision, PendingRequests};
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

fn build_tray_menu(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
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
                println!("[Pawkit] Config reloaded");
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_config = config::load_all_config();
    let shared_config: SharedConfig = Arc::new(Mutex::new(initial_config));
    let pending_requests: PendingRequests = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

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

            build_tray_menu(app)?;
            let app_handle = app.handle().clone();
            start_config_watcher(app_handle.clone(), shared_config.clone());

            // Start the HTTP hook server for Claude Code integration
            hook_server::start_hook_server(app_handle.clone(), pending_requests.clone(), HOOK_SERVER_PORT);

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
