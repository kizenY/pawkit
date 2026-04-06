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

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub actions: ActionsConfig,
    pub pet: PetConfig,
}

pub fn get_config_dir() -> PathBuf {
    // In development, use project config/ directory
    let dev_config = PathBuf::from("config");
    if dev_config.exists() {
        return dev_config;
    }

    // In production, use the app's resource directory or user config
    if let Some(config_dir) = dirs::config_dir() {
        let app_config = config_dir.join("pawkit");
        if !app_config.exists() {
            let _ = fs::create_dir_all(&app_config);
        }
        return app_config;
    }

    dev_config
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

pub fn load_all_config() -> AppConfig {
    let config_dir = get_config_dir();
    AppConfig {
        actions: load_actions(&config_dir),
        pet: load_pet_config(&config_dir),
    }
}
