//! smolcode core library — agent loop, tools, config, sessions.
//!
//! Shared by the `smolcode` CLI binary, Python bindings (`smolcode-py`), and tests.

pub mod agent;
pub mod agents_init;
pub mod autocompact;
pub mod bgproc;
pub mod clipboard;
pub mod commands;
pub mod completions;
pub mod config;
pub mod config_view;
pub mod context;
pub mod delegate;
pub mod engine;
pub mod eval;
pub mod extract;
pub mod fmt;
pub mod git;
pub mod glyphs;
pub mod headless;
pub mod hooks;
pub mod judge;
pub mod lsp;
pub mod mcp_tools;
pub mod multi_edit;
pub mod notify;
pub mod patch;
pub mod permission;
pub mod project;
pub mod prompts;
pub mod rag;
pub mod redact;
pub mod repair;
pub mod retry;
pub mod route_clf;
pub mod router;
pub mod rules;
pub mod search;
pub mod session;
pub mod session_ops;
pub mod skills;
pub mod stats;
pub mod stream;
pub mod structured;
pub mod symbols;
pub mod syntax;
pub mod testrun;
pub mod theme;
pub mod themes_extra;
pub mod trace;
pub mod transcript_search;
pub mod tree;
pub mod tools;
pub mod tui;
pub mod undo;
pub mod usage;
pub mod web;

// Re-export the stable engine surface for bindings and integrations.
pub use agent::{run_agent, AgentEvent};
pub use config::{Config, Flags};
pub use engine::{Engine, RunOpts, ToolProfile};
pub use permission::PermissionSet;
pub use prompts::Agent;
pub use router::Think;
pub use tools::{Tools, ToolExtension};
