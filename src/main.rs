//! smolcode CLI binary — thin entry point over the `smolcode` library.

use anyhow::{Context, Result};
use smolcode::config::{Config, Flags};
use smolcode::eval;
use smolcode::headless;
use smolcode::hooks::Hooks;
use smolcode::mcp_tools;
use smolcode::permission::PermissionSet;
use smolcode::prompts;
use smolcode::tools::Tools;
use smolcode::tui;
use std::io::{self, Write};

#[tokio::main]
async fn main() -> Result<()> {
    let mut flags = Flags::default();
    let mut dir = ".".to_string();
    let mut no_tui = false;
    let mut resume_last = false;
    let mut task_parts: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model" => flags.model = args.next(),
            "--url" => flags.base_url = args.next(),
            "--key" => flags.api_key = args.next(),
            "--agent" => flags.agent = args.next(),
            "--plan" => flags.agent = Some("plan".into()),
            "--dir" => dir = args.next().unwrap_or(dir),
            "--yolo" => flags.yolo = true,
            "--local" => flags.base_url = Some("http://localhost:11435/v1".into()),
            "--no-tui" | "--repl" => no_tui = true,
            "--continue" | "-c" => resume_last = true,
            "--completions" => {
                let shell = args.next().unwrap_or_default();
                match smolcode::completions::generate(&shell) {
                    Ok(script) => {
                        print!("{script}");
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(2);
                    }
                }
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other => task_parts.push(other.to_string()),
        }
    }

    let cfg = Config::load(flags);
    let root = std::fs::canonicalize(&dir).with_context(|| format!("workspace dir: {dir}"))?;
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
    let mcp = std::sync::Arc::new(mcp_tools::McpTools::connect(cfg.mcp.clone()).await);

    // eval harness: `smolcode eval [smoke|dev|<dir>]`
    if task_parts.first().map(|s| s == "eval").unwrap_or(false) {
        let set = task_parts.get(1).cloned().unwrap_or_else(|| "smoke".into());
        run_eval(&client, &cfg, hooks, mcp, &set).await;
        return Ok(());
    }

    // one-shot
    if !task_parts.is_empty() {
        banner(&cfg, &root.display().to_string());
        let tools = Tools::new(root.clone(), cfg.yolo);
        let task = task_parts.join(" ");
        let system = prompts::resolve_system(&agent, &root, &task);
        headless::run_task(
            &client,
            &cfg.model,
            &tools,
            &task,
            system,
            agent.read_only,
            perms,
            hooks,
            mcp,
        )
        .await;
        return Ok(());
    }

    // headless REPL
    if no_tui {
        banner(&cfg, &root.display().to_string());
        let tools = Tools::new(root.clone(), cfg.yolo);
        loop {
            print!("\x1b[1;32m› \x1b[0m");
            io::stdout().flush().ok();
            let mut line = String::new();
            if io::stdin().read_line(&mut line)? == 0 {
                break;
            }
            let task = line.trim();
            if task.is_empty() {
                continue;
            }
            if task == "exit" || task == "quit" {
                break;
            }
            let system = prompts::resolve_system(&agent, &root, task);
            headless::run_task(
                &client,
                &cfg.model,
                &tools,
                task,
                system,
                agent.read_only,
                perms.clone(),
                hooks.clone(),
                mcp.clone(),
            )
            .await;
            println!();
        }
        return Ok(());
    }

    // default: opencode-style TUI
    tui::run(
        client,
        cfg.model.clone(),
        cfg.base_url.clone(),
        root,
        cfg.yolo,
        cfg.agent.clone(),
        hooks,
        mcp,
        resume_last,
    )
    .await
}

/// Run an eval set: `smoke` (inline cases), `dev` (bench/dev), or a directory
/// path. For each case spin a fresh temp workspace, run the agent (yolo) on the
/// prompt, then judge. Writes a JSON report to bench/results/ when that dir is
/// reachable.
async fn run_eval(
    client: &liteforge::AsyncForgeClient,
    cfg: &Config,
    hooks: Hooks,
    mcp: std::sync::Arc<mcp_tools::McpTools>,
    set: &str,
) {
    let cases = match set {
        "smoke" => eval::smoke_suite(),
        other => {
            let dir = if other == "dev" {
                std::path::PathBuf::from("bench/dev")
            } else {
                std::path::PathBuf::from(other)
            };
            match eval::load_dir(&dir) {
                Ok(c) if !c.is_empty() => c,
                Ok(_) => {
                    eprintln!(
                        "no eval cases found in {} (run from the repo root?)",
                        dir.display()
                    );
                    return;
                }
                Err(e) => {
                    eprintln!("eval: {e}");
                    return;
                }
            }
        }
    };

    let base = std::env::temp_dir().join(format!("smolcode-eval-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&base);
    println!(
        "\x1b[1;35msmolcode eval [{set}]\x1b[0m — {} @ {} — {} cases\n",
        cfg.model,
        cfg.base_url,
        cases.len()
    );
    let perms = PermissionSet::for_agent(false, true); // yolo: auto-approve
    let mut results = Vec::new();
    for case in &cases {
        let ws = base.join(&case.name);
        let _ = std::fs::create_dir_all(&ws);
        if let Err(e) = eval::prepare(case, &ws) {
            println!("  \x1b[33mskip {}: prepare failed: {e}\x1b[0m", case.name);
            continue;
        }
        let ws = std::fs::canonicalize(&ws).unwrap_or(ws);
        println!("\x1b[36m▶ {} ({})\x1b[0m", case.name, case.lang);
        let tools = Tools::new(ws.clone(), true);
        let agent = prompts::builtin().into_iter().next().expect("build agent");
        let system = prompts::resolve_system(&agent, &ws, &case.prompt);
        headless::run_task(
            client,
            &cfg.model,
            &tools,
            &case.prompt,
            system,
            false,
            perms.clone(),
            hooks.clone(),
            mcp.clone(),
        )
        .await;
        let r = eval::judge(case, &ws);
        println!(
            "  {}\n",
            if r.passed {
                "\x1b[32m✓ passed\x1b[0m".to_string()
            } else {
                format!("\x1b[31m✗ failed: {}\x1b[0m", r.detail)
            }
        );
        results.push(r);
    }
    let card = eval::scorecard(&results);
    println!("{card}");
    write_eval_results(set, &cfg.model, &results);
}

/// Best-effort: append a JSON line of results to bench/results/ if reachable.
fn write_eval_results(set: &str, model: &str, results: &[eval::CaseResult]) {
    let dir = std::path::Path::new("bench/results");
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let cases: Vec<String> = results
        .iter()
        .map(|r| {
            format!(
                "{{\"name\":{},\"passed\":{},\"detail\":{}}}",
                json_str(&r.name),
                r.passed,
                json_str(&r.detail)
            )
        })
        .collect();
    let passed = results.iter().filter(|r| r.passed).count();
    let line = format!(
        "{{\"set\":{},\"model\":{},\"passed\":{},\"total\":{},\"cases\":[{}]}}\n",
        json_str(set),
        json_str(model),
        passed,
        results.len(),
        cases.join(",")
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("eval.jsonl"))
    {
        let _ = f.write_all(line.as_bytes());
        println!("\x1b[90mresults appended to bench/results/eval.jsonl\x1b[0m");
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn banner(cfg: &Config, root: &str) {
    println!(
        "\x1b[1;35msmolcode\x1b[0m — \x1b[36m{}\x1b[0m @ {}",
        cfg.model, cfg.base_url
    );
    println!("workspace: \x1b[90m{root}\x1b[0m  ·  agent: {}", cfg.agent);
    if cfg.yolo {
        println!("\x1b[31m--yolo: writes/shell run WITHOUT approval\x1b[0m");
    }
    println!();
}

fn print_help() {
    println!(
        "smolcode — SLM coding agent (Rust + LiteForge)\n\n\
         USAGE:\n  smolcode [OPTIONS] [TASK...]\n\n\
         No TASK  -> opencode-style TUI.   A TASK -> runs once headless.\n\n\
         OPTIONS:\n\
         \x20 --model <M>   model id (default granite4.1:8b)\n\
         \x20 --url <U>     OpenAI-compatible base URL (default http://localhost:11434/v1)\n\
         \x20 --local       use local Ollama (http://localhost:11435/v1)\n\
         \x20 --key <K>     API key\n\
         \x20 --agent <A>   agent: build | plan\n\
         \x20 --plan        start in plan (read-only) agent\n\
         \x20 --dir <D>     workspace directory (default .)\n\
         \x20 --yolo        auto-approve writes/shell\n\
         \x20 --no-tui      headless REPL instead of the TUI\n\
         \x20 -c, --continue resume the most recent session (TUI)\n\
         \x20 --completions <bash|zsh|fish>  print a shell completion script\n\
         \x20 -h, --help    this help\n\n\
         Config: ~/.config/smolcode/config.toml, ./.smolcode/config.toml, env SMOLCODE_*."
    );
}
