use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionsConfig {
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(rename = "type")]
    pub action_type: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,

    // shell
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,

    // script
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,

    // url / http
    #[serde(default)]
    pub url: Option<String>,

    // http
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub timeout: Option<u64>,

    // pipeline
    #[serde(default)]
    pub steps: Option<Vec<PipelineStep>>,
    #[serde(default)]
    pub on_failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetConfigFile {
    pub pet: PetConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PetConfig {
    #[serde(default = "default_sprite")]
    pub sprite: String,
    #[serde(default = "default_scale")]
    pub scale: u32,
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: u64,
    #[serde(default = "default_start_position")]
    pub start_position: String,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default)]
    pub click_through: bool,
}

fn default_true() -> bool { true }
fn default_sprite() -> String { "pixel-cat".to_string() }
fn default_scale() -> u32 { 2 }
fn default_idle_timeout() -> u64 { 300 }
fn default_start_position() -> String { "bottom-right".to_string() }
fn default_opacity() -> f64 { 1.0 }

pub type SharedConfig = Arc<Mutex<AppConfig>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub app_token: String,
    #[serde(default)]
    pub dm_user_id: String,
    #[serde(default = "default_working_dir")]
    pub working_dir: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_output_buffer")]
    pub output_buffer_ms: u64,
    #[serde(default)]
    pub critical_tools: Vec<String>,
    /// Mention monitor mode: "monitor" (prompt approval), "auto_reply", or "rest" (off)
    #[serde(default = "default_mention_mode")]
    pub mention_mode: String,
}

fn default_working_dir() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| if cfg!(target_os = "windows") { "C:\\".to_string() } else { "/".to_string() })
}
fn default_poll_interval() -> u64 { 2000 }
fn default_output_buffer() -> u64 { 1000 }
fn default_mention_mode() -> String { "rest".to_string() }

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub actions: ActionsConfig,
    pub pet: PetConfig,
}

pub fn get_config_dir() -> PathBuf {
    // Dev mode: use project config/ directory (exe is in src-tauri/target/debug)
    if cfg!(debug_assertions) {
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let mut dir = exe_dir.to_path_buf();
                for _ in 0..5 {
                    let config_candidate = dir.join("config");
                    if config_candidate.exists() {
                        return config_candidate;
                    }
                    if let Some(parent) = dir.parent() {
                        dir = parent.to_path_buf();
                    } else {
                        break;
                    }
                }
            }
        }

        let cwd_config = PathBuf::from("config");
        if cwd_config.exists() {
            return cwd_config;
        }
    }

    // Production: always use user config directory (%APPDATA%/pawkit)
    if let Some(config_dir) = dirs::config_dir() {
        let app_config = config_dir.join("pawkit");
        if !app_config.exists() {
            let _ = fs::create_dir_all(&app_config);
        }
        return app_config;
    }

    PathBuf::from("config")
}

/// Copy bundled default configs to user config dir on first launch.
/// Only copies files that don't already exist (never overwrites user config).
pub fn seed_default_configs() {
    if cfg!(debug_assertions) {
        return; // Dev mode uses project config/ directly
    }

    let config_dir = get_config_dir();

    // Find bundled defaults: check exe parent (Windows/Linux) and
    // macOS .app/Contents/Resources/ (Tauri bundles resources there).
    let defaults_dir = std::env::current_exe().ok().and_then(|exe| {
        // 1. Next to the executable (Windows, Linux)
        let beside_exe = exe.parent()?.join("config-defaults");
        if beside_exe.exists() {
            return Some(beside_exe);
        }
        // 2. macOS: .app/Contents/MacOS/../Resources/config-defaults
        let resources = exe.parent()?.parent()?.join("Resources").join("config-defaults");
        if resources.exists() {
            return Some(resources);
        }
        None
    });

    let defaults_dir = match defaults_dir {
        Some(d) => d,
        None => return,
    };

    let defaults = ["actions.yaml", "pet.yaml", "auto_review.yaml"];
    for filename in &defaults {
        let target = config_dir.join(filename);
        if !target.exists() {
            let source = defaults_dir.join(filename);
            if source.exists() {
                if let Err(e) = fs::copy(&source, &target) {
                    eprintln!("Failed to seed default config {}: {}", filename, e);
                }
            }
        }
    }
}

pub fn load_actions(config_dir: &PathBuf) -> ActionsConfig {
    let path = config_dir.join("actions.yaml");
    match fs::read_to_string(&path) {
        Ok(content) => {
            match serde_yaml::from_str::<ActionsConfig>(&content) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("Failed to parse actions.yaml: {}", e);
                    ActionsConfig { actions: vec![] }
                }
            }
        }
        Err(_) => ActionsConfig { actions: vec![] },
    }
}

pub fn load_pet_config(config_dir: &PathBuf) -> PetConfig {
    let path = config_dir.join("pet.yaml");
    match fs::read_to_string(&path) {
        Ok(content) => {
            match serde_yaml::from_str::<PetConfigFile>(&content) {
                Ok(config) => config.pet,
                Err(e) => {
                    eprintln!("Failed to parse pet.yaml: {}", e);
                    default_pet_config()
                }
            }
        }
        Err(_) => default_pet_config(),
    }
}

fn default_pet_config() -> PetConfig {
    PetConfig {
        sprite: default_sprite(),
        scale: default_scale(),
        idle_timeout: default_idle_timeout(),
        start_position: default_start_position(),
        opacity: default_opacity(),
        click_through: false,
    }
}

pub fn load_slack_config(config_dir: &PathBuf) -> SlackConfig {
    let path = config_dir.join("slack.yaml");
    match fs::read_to_string(&path) {
        Ok(content) => {
            match serde_yaml::from_str::<SlackConfig>(&content) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("Failed to parse slack.yaml: {}", e);
                    SlackConfig::default()
                }
            }
        }
        Err(_) => SlackConfig::default(),
    }
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            app_token: String::new(),
            dm_user_id: String::new(),
            working_dir: default_working_dir(),
            poll_interval_ms: default_poll_interval(),
            output_buffer_ms: default_output_buffer(),
            critical_tools: vec!["Bash".to_string()],
            mention_mode: default_mention_mode(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoReviewConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_review_interval")]
    pub interval_minutes: u64,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub repo_dirs: HashMap<String, String>,
    /// GitHub account to use (runs `gh auth switch -u <account>` before each poll)
    #[serde(default)]
    pub gh_account: Option<String>,
    /// Whether to auto-merge PRs after successful review (default: false)
    #[serde(default)]
    pub auto_merge: bool,
    /// Model override for reviews (None = default/opus, Some("sonnet") for faster/cheaper)
    #[serde(default)]
    pub model: Option<String>,
}

fn default_review_interval() -> u64 { 5 }

impl Default for AutoReviewConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: default_review_interval(),
            repos: Vec::new(),
            repo_dirs: HashMap::new(),
            gh_account: None,
            auto_merge: false,
            model: None,
        }
    }
}

pub fn load_auto_review_config(config_dir: &PathBuf) -> AutoReviewConfig {
    let path = config_dir.join("auto_review.yaml");
    match fs::read_to_string(&path) {
        Ok(content) => {
            match serde_yaml::from_str::<AutoReviewConfig>(&content) {
                Ok(config) => config,
                Err(e) => {
                    eprintln!("Failed to parse auto_review.yaml: {}", e);
                    AutoReviewConfig::default()
                }
            }
        }
        Err(_) => AutoReviewConfig::default(),
    }
}

pub fn load_all_config() -> AppConfig {
    let config_dir = get_config_dir();
    AppConfig {
        actions: load_actions(&config_dir),
        pet: load_pet_config(&config_dir),
    }
}
