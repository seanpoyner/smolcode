#!/usr/bin/env bash
# Publish the sanitized TUI repo to GitHub (public).
# Requires: gh auth login (or GH_TOKEN / GITHUB_TOKEN in the environment)
set -euo pipefail

cd "$(dirname "$0")/.."

if ! gh auth status -h github.com >/dev/null 2>&1; then
  echo "error: not logged into GitHub. Run: gh auth login" >&2
  exit 1
fi

OWNER="${GITHUB_OWNER:-seanpoyner}"
REPO="${GITHUB_REPO:-smolcode}"

if gh repo view "${OWNER}/${REPO}" >/dev/null 2>&1; then
  echo "==> ${OWNER}/${REPO} already exists; pushing"
  git remote remove github 2>/dev/null || true
  git remote add github "https://github.com/${OWNER}/${REPO}.git"
  git push -u github main
else
  echo "==> Creating public repo ${OWNER}/${REPO}"
  gh repo create "${OWNER}/${REPO}" --public --source=. --remote=github --push \
    --description "SLM-optimized terminal coding agent (TUI + headless), built on LiteForge"
fi

echo "✓ https://github.com/${OWNER}/${REPO}"
