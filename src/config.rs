//! Layered config: defaults (local Ollama) < global toml < project toml < env < CLI flags.

use crate::hooks::CommandHook;
use serde::Deserialize;
use std::path::PathBuf;

/// Default OpenAI-compatible endpoint (local Ollama).
pub const DEFAULT_URL: &str = "http://localhost:11434/v1";
pub const DEFAULT_MODEL: &str = "granite4.1:8b";

#[derive(Clone)]
pub struct Config {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub agent: String,
    pub yolo: bool,
    pub hooks: Vec<CommandHook>,
    pub mcp: Vec<crate::mcp_tools::McpServerCfg>,
}

#[derive(Default, Deserialize)]
struct FileConfig {
    base_url: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    agent: Option<String>,
    #[serde(default)]
    hook: Vec<CommandHook>,
    #[serde(default)]
    mcp: Vec<crate::mcp_tools::McpServerCfg>,
}

/// Overrides parsed from CLI flags (None = not given).
#[derive(Default)]
pub struct Flags {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub agent: Option<String>,
    pub yolo: bool,
}

fn read_toml(path: PathBuf) -> Option<FileConfig> {
    let s = std::fs::read_to_string(path).ok()?;
    toml::from_str(&s).ok()
}

fn global_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("smolcode")
        .join("config.toml")
}

impl Config {
    pub fn load(flags: Flags) -> Self {
        let mut c = Config {
            base_url: DEFAULT_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key: "ollama".to_string(),
            agent: "build".to_string(),
            yolo: false,
            hooks: Vec::new(),
            mcp: Vec::new(),
        };
        for f in [read_toml(global_config_path()), read_toml(PathBuf::from(".smolcode/config.toml"))]
            .into_iter()
            .flatten()
        {
            if let Some(v) = f.base_url {
                c.base_url = v;
            }
            if let Some(v) = f.model {
                c.model = v;
            }
            if let Some(v) = f.api_key {
                c.api_key = v;
            }
            if let Some(v) = f.agent {
                c.agent = v;
            }
            c.hooks.extend(f.hook);
            c.mcp.extend(f.mcp);
        }
        if let Ok(v) = std::env::var("SMOLCODE_BASE_URL") {
            c.base_url = v;
        }
        if let Ok(v) = std::env::var("SMOLCODE_MODEL") {
            c.model = v;
        }
        if let Ok(v) = std::env::var("SMOLCODE_API_KEY") {
            c.api_key = v;
        }
        if let Some(v) = flags.base_url {
            c.base_url = v;
        }
        if let Some(v) = flags.model {
            c.model = v;
        }
        if let Some(v) = flags.api_key {
            c.api_key = v;
        }
        if let Some(v) = flags.agent {
            c.agent = v;
        }
        c.yolo = flags.yolo;
        c
    }
}
