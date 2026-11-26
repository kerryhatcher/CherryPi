# AGENTS.md — CherryPi Project Notes

## Logs

Logs are written in JSONL format to the platform data directory:

| Platform | Path |
|---|---|
| Linux | `~/.local/share/cherrypi/logs/` |
| macOS | `~/Library/Application Support/cherrypi/logs/` |
| Windows | `%APPDATA%/cherrypi/logs/` |

### Log files

- **`chat.jsonl`** — Full chat transcript. One JSON record per line: `{"timestamp","cwd","role","content"}`. Roles: `user`, `assistant`, `system`, `error`.
- **`cherrypi.jsonl`** — Structured application logs (tracing). Rotates at 25MB, keeps 7 compressed `.gz` archives.

### Viewing logs

```bash
# Tail the chat transcript
cat ~/.local/share/cherrypi/logs/chat.jsonl | jq .

# Filter errors only
grep '"role":"error"' ~/.local/share/cherrypi/logs/chat.jsonl | jq .

# Tail app logs
cat ~/.local/share/cherrypi/logs/cherrypi.jsonl | jq .

# Filter ERROR level only
grep '"ERROR"' ~/.local/share/cherrypi/logs/cherrypi.jsonl | jq .
```

## Config

Config file: `~/.config/cherrypi/config.toml` (Linux). Created with commented defaults on first run if missing.

Lookup order (first existing file wins): `--config <path>`, `./.cherrypi.toml`, `./.cherrypi.json`, `~/.config/cherrypi/config.toml`, `~/.config/cherrypi/config.json`. Files ending in `.json` are read as the legacy JSON format; everything else is TOML.

## Build

```bash
cargo build
cargo run
```

## Page rendering (cherrypi-fetch)

`web_fetch` with `render: true` renders the page in Servo through the
[`cherrypi-fetch`](https://crates.io/crates/cherrypi-fetch) crate from crates.io.
Its source and CLI live in
<https://github.com/kerryhatcher/CherryPi-Fetch>; fix renderer bugs there and
bump the version here.

```bash
cargo install cherrypi-fetch                    # the standalone CLI
cherrypi-fetch --json "https://example.com/"
cargo build -p cherrypi --no-default-features   # cherrypi without Servo; render requests then return an error
```

The User-Agent comes from `user_agent` under `[agent]` in the config.

## Status Bar Plugins

Plugins can contribute segments to the TUI status bar by declaring a `status_bar`
in their manifest. The command is polled on a configurable interval and its stdout
is rendered in the bottom bar.

### Manifest format

```json
{
  "name": "my-status-plugin",
  "status_bar": {
    "command": "scripts/status.sh",
    "refresh_ms": 3000,
    "label": "git"
  }
}
```

| Field | Required | Description |
|---|---|---|
| `command` | Yes | Shell command (supports `${PLUGIN_ROOT}` variable) |
| `refresh_ms` | No | Poll interval in ms (default: 2000) |
| `label` | No | Prefix shown before the output in the status bar |
| `stdin_context` | No | If true, pipe `{model, workspace, session_id}` JSON to stdin |

When `stdin_context: true`, the command receives a JSON object on stdin:

```json
{
  "model": {"display_name": "llama3.2"},
  "workspace": {"current_dir": "/home/user/project"},
  "session_id": "uuid-..."
}
```

This is compatible with [ferrisbar](https://github.com/kerryhatcher/ferrisbar) —
a Rust statusline renderer that reads this exact format. `cargo install ferrisbar`
and add a plugin manifest pointing to it:

```json
{
  "name": "ferrisbar",
  "status_bar": {
    "command": "ferrisbar",
    "stdin_context": true,
    "label": null
  }
}
```

### Example plugin

See `plugins/demo-status/` for a working example that shows git branch and
system load in the status bar.

### Plugin search paths

- `./plugins/` (project-local)
- `~/.local/share/cherrypi/plugins/` (installed)
- Custom paths via `--plugin-dir` CLI flag or config
