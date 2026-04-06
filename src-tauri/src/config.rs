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
}

fn default_working_dir() -> String { "E:\\develop\\code".to_string() }
fn default_poll_interval() -> u64 { 2000 }
fn default_output_buffer() -> u64 { 1000 }

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub actions: ActionsConfig,
    pub pet: PetConfig,
}

pub fn get_config_dir() -> PathBuf {
    // Try relative to the executable first (works for both dev and production)
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            // In dev mode, exe is in src-tauri/target/debug, config is at project root
            // Walk up to find config/ directory
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

    // Try current working directory
    let cwd_config = PathBuf::from("config");
    if cwd_config.exists() {
        return cwd_config;
    }

    // Fallback: user config directory
    if let Some(config_dir) = dirs::config_dir() {
        let app_config = config_dir.join("pawkit");
        if !app_config.exists() {
            let _ = fs::create_dir_all(&app_config);
        }
        return app_config;
    }

    cwd_config
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
        }
    }
}

pub fn load_all_config() -> AppConfig {
    let config_dir = get_config_dir();
    AppConfig {
        actions: load_actions(&config_dir),
        pet: load_pet_config(&config_dir),
    }
}
