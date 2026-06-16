# smolcode

An SLM-optimized terminal coding agent, built in Rust on the [LiteForge](https://github.com/seanpoyner/liteforge) SDK.

**A tiny local model that writes code, runs it, and fixes it until it works.**

smolcode is an opencode-class coding agent for your terminal: a full ratatui TUI plus headless mode, optimized for small local models (roughly 32B and under, often 4B-15B). It talks to any OpenAI-compatible endpoint (Ollama, llama.cpp, vLLM, hosted APIs).

## Install

### Prebuilt binary (no toolchain needed)

Grab the binary for your platform from the [latest release](https://github.com/seanpoyner/smolcode/releases/latest):

| Platform | Asset |
|----------|-------|
| Linux x86_64 (glibc) | `smolcode-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64 (static, any distro/Alpine) | `smolcode-x86_64-unknown-linux-musl.tar.gz` |
| Linux ARM64 (glibc) | `smolcode-aarch64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 (static, Raspberry Pi/Alpine) | `smolcode-aarch64-unknown-linux-musl.tar.gz` |
| macOS Apple Silicon | `smolcode-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `smolcode-x86_64-apple-darwin.tar.gz` |
| Windows x86_64 | `smolcode-x86_64-pc-windows-msvc.zip` |

```bash
# Example: Linux x86_64 (static build runs anywhere)
curl -fsSL https://github.com/seanpoyner/smolcode/releases/latest/download/smolcode-x86_64-unknown-linux-musl.tar.gz | tar -xz
install -m755 smolcode ~/.local/bin/
```

Checksums for every asset are in `SHA256SUMS` on the release.

> The `musl` builds are fully static and ship without the learned ONNX router
> (regex routing instead) — small, dependency-free, and portable to any Linux.

### From crates.io

```bash
cargo install smolcode
```

### Python bindings

Embed the agent engine in Python (`Session` / `Config`):

```bash
pip install smolcode-core
```

### From source

```bash
git clone https://github.com/seanpoyner/smolcode.git
cd smolcode
./install.sh
```

`install.sh` builds the release binary and symlinks `target/release/smolcode` into `~/.local/bin`.

**Requirements (source/crates.io builds):** Rust 1.75+, a running OpenAI-compatible LLM server.

```bash
# Example: Ollama with a tool-capable model
ollama pull granite4.1:8b
ollama serve   # default http://localhost:11434
```

## Usage

```bash
smolcode                 # launch the ratatui TUI
smolcode "<task>"        # one-shot headless run, then exit
smolcode --no-tui        # headless REPL (alias: --repl)
```

### Flags

| Flag | Description |
|------|-------------|
| `--model <M>` | Model id (default `granite4.1:8b`) |
| `--url <U>` | OpenAI-compatible base URL (default `http://localhost:11434/v1`) |
| `--local` | Use alternate local Ollama port (`http://localhost:11435/v1`) |
| `--key <K>` | API key |
| `--agent <A>` | Agent to start in: `build` or `plan` |
| `--plan` | Start in the `plan` (read-only) agent |
| `--dir <D>` | Workspace directory (default `.`) |
| `--yolo` | Auto-approve writes and shell |
| `--no-tui`, `--repl` | Headless REPL instead of the TUI |
| `-h`, `--help` | Show help |

## Tools

The agent calls tools one step at a time:

- `read_file`, `write_file`, `str_replace`, `apply_patch`
- `search`, `list_dir`, `repo_map`
- `run_shell`, `run_python`
- `task`, `task_batch` (subagent delegation)
- plus any tools exposed by configured MCP servers

All file and shell tools are confined to the workspace root.

## TUI features

- Streaming token render, markdown output, syntect-highlighted code blocks
- Leader key `ctrl+x` which-key popup
- Pickers for models, agents, themes, sessions, and files
- `/` slash-command palette
- `@file` fuzzy attach
- `Tab` cycles agent; `F2` cycles model (Auto-first routing)
- `ctrl+z` / `ctrl+y` undo/redo of file edits
- Toggleable file sidebar and context-usage meter

## Config

Configuration is layered (later sources win):

1. Built-in defaults (local Ollama, `granite4.1:8b`)
2. Global `~/.config/smolcode/config.toml`
3. Project `./.smolcode/config.toml`
4. Environment: `SMOLCODE_BASE_URL`, `SMOLCODE_MODEL`, `SMOLCODE_API_KEY`
5. CLI flags

Example `~/.config/smolcode/config.toml`:

```toml
base_url = "http://localhost:11434/v1"
model = "granite4.1:8b"
api_key = "ollama"
```

Sessions persist to `~/.local/share/smolcode/sessions/`.

## Learned routing (optional)

By default the binary includes an ONNX routing classifier. When model artifacts are
not installed it falls back to transparent regex routing.

Place ONNX artifacts at one of:

- `~/.config/smolcode/router_clf/onnx/`
- `./router_clf/onnx/` (repo-local, gitignored)
- or set `SMALLCODE_ROUTER_CLF_DIR`

Build without routing deps:

```bash
cargo build --release --no-default-features
```

## Architecture

The core is an event-driven agent loop backed by LiteForge. A multi-format
tool-call extractor normalizes native `tool_calls`, Hermes `<tool_call>` tags,
fenced JSON, and bare JSON objects so small models work across providers.

## License

MIT. See [LICENSE](./LICENSE).
