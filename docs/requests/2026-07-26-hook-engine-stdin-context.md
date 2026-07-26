# Feature Request: Add `stdin_context` Support to the Hook Engine

**Date**: 2026-07-26
**Author**: crustytts plugin — local TTS notifications for AI agents
**Status**: Proposed

## Summary

CherryPi's `StatusBarManager` already supports a `stdin_context` flag that pipes runtime context (`{model, workspace, session_id}`) as JSON to the command's stdin. The `HookEngine` has no equivalent — hook commands run via `sh -c` with an empty stdin pipe. This limits hooks from accessing the workspace directory, session ID, or model information, forcing plugin authors to use fragile shell substitutions like `$(pwd)` instead.

Adding `stdin_context` support to the HookEngine would give plugin hooks the same first-class context access that status bars already enjoy, enabling smarter, tool-agnostic hook scripts.

## Motivation

The [crustytts](https://github.com/kerryhatcher/crustytts) plugin provides text-to-speech notifications for AI coding agents. It needs to:

1. Read the session transcript from CherryPi's unified `chat.jsonl` (at `~/.local/share/cherrypi/logs/chat.jsonl`)
2. Filter entries by the current working directory (the `cwd` field in each log entry)
3. Summarize the last exchange via Ollama
4. Speak the summary aloud

Currently, the hook command in `hooks.json` must be:

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [{
          "type": "command",
          "command": "crustytts --transcript $HOME/.local/share/cherrypi/logs/chat.jsonl --cwd $(pwd)"
        }]
      }
    ]
  }
}
```

This works but has problems:
- `$(pwd)` is a shell-time interpolation that can drift from the session's actual `cwd`
- The transcript path is hardcoded and platform-specific
- Every hook script that needs runtime context must reconstruct it from shell primitives
- The same binary (`crustytts`) works with Claude Code via its stdin protocol, but CherryPi's hooks can't use that mechanism

With `stdin_context` in the HookEngine, the hook command simplifies to just `crustytts`, and the binary reads the workspace directory from the context JSON on stdin — identical to how Claude Code passes the transcript path.

## Current Architecture

### Status Bar — already has `stdin_context`

File: `crates/plugins/src/status.rs` (lines 106–135)

The `StatusBarConfig` struct has a `stdin_context: bool` field (manifest.rs line 103). When true, `StatusBarManager::tick()` calls `poll_command_with_stdin()` which:

1. Spawns the command with `stdin(Stdio::piped())`
2. Writes `StatusContext` JSON to stdin
3. Closes the pipe
4. Reads stdout

The context payload is:

```json
{
  "model": {"display_name": "llama3.2"},
  "workspace": {"current_dir": "/home/user/project"},
  "session_id": "uuid-..."
}
```

### Hook Engine — has no stdin

File: `crates/plugins/src/hooks.rs` (lines 91–127)

The `HookEngine::register_from_config()` method parses hook rules from a plugin's `hooks.json` and stores them as `HookRule` structs. When `HookEngine::fire()` matches an event, it calls `execute_action()` which runs:

```rust
let output = Command::new("sh")
    .arg("-c")
    .arg(command)
    .output();
```

There is no stdin pipe. There is no context parameter. Hook commands run blind.

### The Gap

| Mechanism | Has stdin_context | Event-driven | Context Available |
|---|---|---|---|
| `StatusBarManager` | ✅ Yes | ❌ Polled (periodic) | `model`, `workspace`, `session_id` |
| `HookEngine` | ❌ No | ✅ Fires on event | Nothing |

The result: hook plugins that need workspace context must encode it in the command string via shell substitution (`$(pwd)`, `$HOME`), which is fragile and tool-specific.

## Proposed Changes

### 1. Add `stdin_context` field to `HookAction::Command`

Currently `HookAction::Command` only carries `command` and `plugin_root`:

```rust
pub enum HookAction {
    Command {
        command: String,
        plugin_root: String,
    },
    // ...
}
```

Add an optional `stdin_context` flag:

```rust
pub enum HookAction {
    Command {
        command: String,
        plugin_root: String,
        stdin_context: bool,
    },
    // ...
}
```

### 2. Parse `stdin_context` from the hook config

In `register_from_config()`, when parsing each hook action of type `"command"`, check for a `stdin_context` field in the JSON:

```rust
"command" => {
    if let Some(cmd) = action.get("command").and_then(|c| c.as_str()) {
        let expanded = cmd.replace("${PLUGIN_ROOT}", plugin_root);
        let stdin_context = action
            .get("stdin_context")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        parsed_actions.push(HookAction::Command {
            command: expanded,
            plugin_root: plugin_root.to_string(),
            stdin_context,
        });
    }
}
```

### 3. Pass context to `HookEngine::fire()`

Add a `StatusContext`-like payload (or reuse `StatusContext` directly) to the `fire()` method:

```rust
pub async fn fire(&self, event: &Event, ctx: &HookContext) -> Vec<HookResult> {
    // ...
    for action in &rule.actions {
        let result = self.execute_action(action, event, ctx).await;
        results.push(result);
    }
    // ...
}
```

Where `HookContext` is:

```rust
pub struct HookContext {
    pub workspace: WorkspaceInfo,
    pub session_id: Option<String>,
}
```

(No `model` field needed since hooks don't call the LLM directly.)

### 4. Pipe context to stdin when `stdin_context` is true

In `execute_action()`, when the action is a command with `stdin_context: true`:

```rust
HookAction::Command { command, stdin_context, .. } => {
    let output = if *stdin_context {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = child.stdin.take() {
            let ctx_json = serde_json::to_string(ctx)?;
            let _ = stdin.write_all(ctx_json.as_bytes());
            // stdin dropped here → pipe closes
        }

        child.wait_with_output()?
    } else {
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()?
    };
    // ...
}
```

This mirrors `status.rs`'s `poll_command_with_stdin()` exactly.

### 5. Wire `HookContext` into the caller

Wherever `HookEngine::fire()` is called (likely in the agent loop, `crates/agent/src/agent.rs` or similar), construct the `HookContext` from the current session state and pass it along.

## Example: crustytts Hook Config After This Change

```json
{
  "hooks": {
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "crustytts",
            "stdin_context": true
          }
        ]
      }
    ]
  }
}
```

The crustytts binary reads the workspace `current_dir` from the stdin JSON, locates the CherryPi log at a known path (`$HOME/.local/share/cherrypi/logs/chat.jsonl`), filters entries by that directory, summarizes, and speaks — no shell substitution, no hardcoded paths.

## Backward Compatibility

- The change is fully additive: existing hooks without `stdin_context` continue to work exactly as before (the default is `false`)
- `HookAction::Command` gains an optional field — existing pattern matches on `Command { command, plugin_root }` will need updating, but they'll still compile if the field has a `..` or is explicitly matched
- `fire()` gains a parameter — all call sites must be updated, but the parameter can default to an empty context for backward compatibility

## Open Questions

1. **Should `HookContext` reuse `StatusContext` from `status.rs`, or be its own type?** Reusing would reduce duplication but adds `ModelInfo` which hooks don't need. A dedicated `HookContext` is cleaner.

2. **Should the timeout from `HookEngine.timeout` apply to stdin piped commands?** Yes — the same `Duration` should govern both, enforced via `Command::timeout` or a `tokio::time::timeout` wrapper.

3. **Should `stdin_context` be per-action or per-rule?** Per-action (as proposed) gives the most granular control — a single rule could have some commands that get context and others that don't.

## Related Files

| File | What changes |
|---|---|
| `crates/plugins/src/hooks.rs` | Add `stdin_context` field, context parameter, stdin piping |
| `crates/plugins/src/lib.rs` | Export `HookContext` if added as a public type |
| `crates/plugins/src/status.rs` | Reference implementation for `poll_command_with_stdin()` |
| `crates/agent/src/agent.rs` | Wire `HookContext` when calling `fire()` |
| `crates/core/src/event.rs` | No changes needed (events stay the same) |
