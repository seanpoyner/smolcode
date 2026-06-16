# smolcode-core

Python bindings for [smolcode](https://github.com/seanpoyner/smolcode) — an
SLM-optimized, opencode-class terminal coding agent built in Rust on the
[LiteForge](https://github.com/seanpoyner/liteforge) SDK.

The `smolcode_core` extension module embeds the Rust agent engine (the agent
loop, tool execution, and multi-format tool-call extraction) so Python apps can
drive a full coding-agent session against any OpenAI-compatible endpoint.

## Install

```bash
pip install smolcode-core
```

## Quickstart

```python
import smolcode_core

# Load config (model, base_url, agent profile) and open a session.
session = smolcode_core.Session(
    workspace=".",
    agent="build",
    model="qwen2.5-coder:7b",
    base_url="http://localhost:11434/v1",
)

session.start_turn("Add a docstring to main.py", think=None, yolo=False)
while True:
    event = session.poll_event()
    if event is None:
        break
    print(event)
```

`Session` exposes the same engine the `smolcode` TUI uses: turn streaming via
`poll_event`, tool-approval gating (`approve`), custom Python tools
(`register_tool`), session persistence (`save` / `load_session` / `fork`), and
MCP discovery (`list_mcp`). `Config` mirrors the on-disk smolcode configuration.

## License

MIT
