//! Headless driver: run the agent and print its events to stdout (no TUI).
//! Used for one-shot tasks and the `--no-tui` REPL. Approval via stdin.

use crate::agent::{run_agent, AgentEvent};
use crate::hooks::Hooks;
use crate::permission::PermissionSet;
use crate::tools::Tools;
use liteforge::AsyncForgeClient;
use std::io::Write;
use tokio::sync::mpsc;

#[allow(clippy::too_many_arguments)]
pub async fn run_task(
    client: &AsyncForgeClient,
    model: &str,
    tools: &Tools,
    task: &str,
    system: String,
    read_only: bool,
    perms: PermissionSet,
    hooks: Hooks,
    mcp: std::sync::Arc<crate::mcp_tools::McpTools>,
) {
    let (tx, mut rx) = mpsc::channel::<AgentEvent>(64);
    tokio::spawn(run_agent(
        client.clone(),
        model.to_string(),
        crate::router::Ladder::default_local(),
        tools.clone(),
        task.to_string(),
        system,
        read_only,
        Vec::new(),
        perms,
        hooks,
        mcp,
        crate::router::Think::Off,
        tx,
    ));

    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::Token(_) => {}
            AgentEvent::Assistant(s) => {
                if !s.trim().is_empty() {
                    println!("\x1b[90m{}\x1b[0m", crate::redact::redact(s.trim()));
                }
            }
            AgentEvent::ToolCall { name, args } => {
                println!("\x1b[34m▸\x1b[0m \x1b[1m{}\x1b[0m \x1b[90m{}\x1b[0m", name, crate::agent::clip(&args, 80));
            }
            AgentEvent::ToolResult { name: _, text } => {
                let safe = crate::redact::redact(&text);
                println!("\x1b[90m  {}\x1b[0m", crate::agent::clip(&safe.replace('\n', " "), 160));
            }
            AgentEvent::Approval { desc, resp } => {
                print!("\x1b[33m  approve: {desc}? [y/N] \x1b[0m");
                std::io::stdout().flush().ok();
                let mut s = String::new();
                std::io::stdin().read_line(&mut s).ok();
                let ok = matches!(s.trim().to_lowercase().as_str(), "y" | "yes");
                let _ = resp.send(ok);
            }
            AgentEvent::Final(s) => {
                println!("\x1b[1;35m✓\x1b[0m {}", crate::redact::redact(s.trim()));
            }
            AgentEvent::Error(s) => {
                println!("\x1b[33m⚠ {s}\x1b[0m");
            }
            AgentEvent::Done => break,
        }
    }
    // don't leak background dev servers when a one-shot run exits
    crate::bgproc::stop_all();
}
