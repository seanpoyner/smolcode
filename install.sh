#!/usr/bin/env bash
#
# smolcode installer: builds the release binary and puts it on your PATH.
# Idempotent and safe to re-run. Prefers a symlink so rebuilds propagate.
#
set -euo pipefail

echo "==> smolcode installer"
echo "    SLM-optimized terminal coding agent (Rust + LiteForge)"
echo

# Always operate from the script's own directory (the crate root).
cd "$(dirname "$0")"

# 1) Build the optimized release binary.
echo "==> Building release binary (cargo build --release)…"
cargo build --release

BIN="$PWD/target/release/smolcode"
if [ ! -x "$BIN" ]; then
  echo "error: expected binary not found at $BIN" >&2
  exit 1
fi

# 2) Install into ~/.local/bin. Symlink first so future rebuilds are picked up
#    automatically; fall back to a plain copy if symlinking is unavailable.
DEST_DIR="$HOME/.local/bin"
DEST="$DEST_DIR/smolcode"
mkdir -p "$DEST_DIR"

if ln -sfn "$BIN" "$DEST" 2>/dev/null; then
  echo "==> Symlinked $DEST -> $BIN"
else
  cp -f "$BIN" "$DEST"
  chmod +x "$DEST"
  echo "==> Copied binary to $DEST"
fi

# 2b) Install optional learned-router ONNX models when present locally.
MODELS_SRC="$PWD/router_clf/onnx"
MODELS_DEST="${XDG_CONFIG_HOME:-$HOME/.config}/smolcode/router_clf/onnx"
if [ -f "$MODELS_SRC/specialty/model.onnx" ]; then
  mkdir -p "$MODELS_DEST"
  cp -r "$MODELS_SRC"/* "$MODELS_DEST"/
  echo "==> Installed learned-router models to $MODELS_DEST"
else
  echo "note: no learned-router models at $MODELS_SRC — the TUI will use regex routing."
  echo "      Drop ONNX artifacts into router_clf/onnx/ or ~/.config/smolcode/router_clf/onnx/."
fi

# 3) Make sure ~/.local/bin is actually on PATH; advise the user if not.
case ":$PATH:" in
  *":$DEST_DIR:"*)
    : # already on PATH, nothing to do
    ;;
  *)
    echo
    echo "note: $DEST_DIR is not on your PATH. Add it to your shell rc:"
    echo "      bash/zsh:  export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo "      fish:      fish_add_path ~/.local/bin"
    ;;
esac

# 4) Install shell completions for whichever shell is in use (best-effort).
echo "==> Installing shell completions…"
install_completion() {
  local shell="$1" dir="$2" file="$3"
  mkdir -p "$dir" 2>/dev/null || return 0
  if "$DEST" --completions "$shell" >"$dir/$file" 2>/dev/null; then
    echo "    $shell -> $dir/$file"
  else
    rm -f "$dir/$file" 2>/dev/null || true
  fi
}
# fish: autoloaded from ~/.config/fish/completions
install_completion fish "${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions" "smolcode.fish"
# bash: sourced via bash-completion's user dir
install_completion bash "${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions" "smolcode"
# zsh: drop into a user functions dir (must be on $fpath)
install_completion zsh "$HOME/.zsh/completions" "_smolcode"
echo "    (zsh: ensure ~/.zsh/completions is on your \$fpath, then 'compinit')"

# 5) Verify clipboard support for the TUI 'y' yank (optional but recommended).
if ! command -v wl-copy >/dev/null 2>&1 \
   && ! command -v xclip >/dev/null 2>&1 \
   && ! command -v xsel  >/dev/null 2>&1 \
   && ! command -v pbcopy >/dev/null 2>&1; then
  echo
  echo "note: no clipboard tool found (ctrl+x y won't copy). Install one:"
  echo "      Wayland:  sudo apt install wl-clipboard"
  echo "      X11:      sudo apt install xclip"
fi

# 6) Done.
echo
echo "✓ installed: run \`smolcode --help\`"
echo "  Default backend: local Ollama (granite4.1:8b at http://localhost:11434/v1)."
