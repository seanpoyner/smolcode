//! Time lsp::diagnostics on a .rs file (was hanging ~21s on big repos).
//! Run: cargo run --release --example lsp_probe

use std::time::Instant;

fn main() {
    let root = std::path::Path::new("..");
    let file = "smolcode-cli/src/main.rs";
    println!("rust-analyzer installed: {}", smolcode::lsp::available_for(file));
    let t0 = Instant::now();
    let diags = smolcode::lsp::diagnostics(root, file);
    println!("diagnostics() took {:.1}s, returned {} items", t0.elapsed().as_secs_f32(), diags.len());
}
