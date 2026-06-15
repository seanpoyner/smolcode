//! Deterministic smoke test for the task_batch fan-out (`delegate::delegate_batch`).
//!
//! Models don't always *choose* to call the `task_batch` tool, so this exercises the
//! parallel dispatch directly: it fires 3 read-only `explore` subagents at once and
//! prints the wall-clock. Concurrent execution => total time ~= the slowest single
//! subagent, not the sum of all three.
//!
//!   cargo run --release --example fanout_smoke -- [model] [base_url]
//!
//! Defaults to granite4.1:8b on hal. Run from the repo root so the subagents have a
//! real workspace to read.

use smolcode::delegate;
use std::path::PathBuf;
use std::time::Instant;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let model = args.get(1).cloned().unwrap_or_else(|| "granite4.1:8b".to_string());
    let base_url = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "http://localhost:11434/v1".to_string());

    let client = liteforge::ForgeClient::builder()
        .base_url(base_url.clone())
        .default_model(model.clone())
        .api_key("ollama".to_string())
        .build_async();

    let root = std::fs::canonicalize("..").unwrap_or_else(|_| PathBuf::from("."));
    let jobs = vec![
        ("explore".to_string(), "In ONE sentence, what does engine/config.py do?".to_string()),
        ("explore".to_string(), "In ONE sentence, what does engine/tools.py define?".to_string()),
        ("explore".to_string(), "In ONE sentence, what does bench/run.py measure?".to_string()),
    ];
    let n = jobs.len();

    println!("fanning out {n} explore subagents on {model} @ {base_url} ...");
    let t = Instant::now();
    let out = delegate::delegate_batch(&client, &model, root, jobs).await;
    let secs = t.elapsed().as_secs_f64();

    println!("\n=== delegate_batch finished {n} subagents in {secs:.1}s (concurrent => ~slowest, not sum) ===\n");
    println!("{out}");
}
