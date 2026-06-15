//! High-level engine API for CLI, Python bindings, and other integrations.
//!
//! Wraps config loading, workspace setup, MCP, hooks, and the event-driven
//! agent loop behind a single `Engine` type.

use crate::agent::{run_agent, AgentEvent};
use crate::config::{Config, Flags};
use crate::hooks::Hooks;
use crate::mcp_tools::McpTools;
use crate::permission::PermissionSet;
use crate::prompts::{self, Agent};
use crate::router::Think;
use crate::tools::Tools;
use liteforge::AsyncForgeClient;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Which tool surface to expose to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProfile {
    /// All built-in tools plus MCP (coding agent).
    Full,
    /// Read-only subset (plan agent).
    Plan,
    /// File ops only; Python may register extra tools (web builder).
    Web,
}

/// Options for a single agent turn.
#[derive(Debug, Clone)]
pub struct RunOpts {
    pub task: String,
    pub think: Think,
    pub yolo: bool,
}

impl Default for RunOpts {
    fn default() -> Self {
        Self {
            task: String::new(),
            think: Think::Off,
            yolo: false,
        }
    }
}

/// One persistent agent context: workspace, client, MCP, conversation history.
pub struct Engine {
    pub client: AsyncForgeClient,
    pub cfg: Config,
    pub root: PathBuf,
    pub tools: Tools,
    pub agent: Agent,
    pub perms: PermissionSet,
    pub hooks: Hooks,
    pub mcp: Arc<McpTools>,
    pub history: Vec<(String, String)>,
    pub think: Think,
    pub profile: ToolProfile,
}

impl Engine {
    /// Load layered config and connect MCP servers.
    pub async fn open(flags: Flags, workspace: impl AsRef<Path>) -> anyhow::Result<Self> {
        let cfg = Config::load(flags);
        let root = std::fs::canonicalize(workspace.as_ref())
            .map_err(|e| anyhow::anyhow!("workspace dir: {}", e))?;
        let client = liteforge::ForgeClient::builder()
            .base_url(cfg.base_url.clone())
            .default_model(cfg.model.clone())
            .api_key(cfg.api_key.clone())
            .build_async();
        let agents = prompts::builtin();
        let agent = agents
            .iter()
            .find(|a| a.name == cfg.agent)
            .or_else(|| agents.first())
            .cloned()
            .expect("at least one builtin agent");
        let perms = PermissionSet::for_agent(agent.read_only, cfg.yolo);
        let hooks = Hooks::new(cfg.hooks.clone());
        let mcp = Arc::new(McpTools::connect(cfg.mcp.clone()).await);
        let tools = Tools::new(root.clone(), cfg.yolo);
        let profile = if agent.read_only {
            ToolProfile::Plan
        } else {
            ToolProfile::Full
        };
        Ok(Self {
            client,
            cfg,
            root,
            tools,
            agent,
            perms,
            hooks,
            mcp,
            history: Vec::new(),
            think: Think::Off,
            profile,
        })
    }

    /// Open with an explicit agent name and yolo override.
    pub async fn open_with(
        flags: Flags,
        workspace: impl AsRef<Path>,
        agent_name: &str,
        yolo: bool,
    ) -> anyhow::Result<Self> {
        let mut flags = flags;
        flags.agent = Some(agent_name.to_string());
        flags.yolo = yolo;
        let mut engine = Self::open(flags, workspace).await?;
        engine.perms = PermissionSet::for_agent(engine.agent.read_only, yolo);
        engine.tools = Tools::new(engine.root.clone(), yolo);
        engine.profile = match agent_name {
            "plan" => ToolProfile::Plan,
            "web" | "web_builder" => ToolProfile::Web,
            _ => ToolProfile::Full,
        };
        Ok(engine)
    }

    pub fn workspace(&self) -> &Path {
        &self.root
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub fn set_think(&mut self, think: Think) {
        self.think = think;
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.cfg.model = model.into();
    }

    pub fn set_agent(&mut self, name: &str) {
        if let Some(a) = prompts::builtin().into_iter().find(|a| a.name == name) {
            self.agent = a;
            self.profile = if self.agent.read_only {
                ToolProfile::Plan
            } else if name == "web" || name == "web_builder" {
                ToolProfile::Web
            } else {
                ToolProfile::Full
            };
        }
    }

    /// Run one task and return a channel of agent events.
    pub async fn run_turn(&mut self, opts: RunOpts) -> mpsc::Receiver<AgentEvent> {
        let (tx, rx) = mpsc::channel::<AgentEvent>(64);
        let system = prompts::resolve_system(&self.agent, &self.root, &opts.task);
        let read_only = self.agent.read_only || self.profile == ToolProfile::Plan;
        let perms = if opts.yolo {
            PermissionSet::for_agent(read_only, true)
        } else {
            self.perms.clone()
        };
        let think = opts.think;
        let task = opts.task.clone();
        let client = self.client.clone();
        let model = self.cfg.model.clone();
        let tools = self.tools.clone();
        let history = self.history.clone();
        let hooks = self.hooks.clone();
        let mcp = self.mcp.clone();
        tokio::spawn(run_agent(
            client,
            model,
            crate::router::Ladder::default_local(),
            tools,
            task,
            system,
            read_only,
            history,
            perms,
            hooks,
            mcp,
            think,
            tx,
        ));
        rx
    }

    /// Append a completed turn to conversation history.
    pub fn record_turn(&mut self, user: String, assistant: String) {
        self.history.push((user, assistant));
    }

    /// List all files in the workspace (relative paths).
    pub fn workspace_files(&self) -> Vec<String> {
        self.tools.workspace_files()
    }

    /// Read a workspace file's contents.
    pub fn read_workspace_file(&self, path: &str) -> Option<String> {
        self.tools.read_workspace_file(path)
    }
}
