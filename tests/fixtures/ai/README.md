# Recordings of the `claude` CLI's stream-json protocol

Recorded from the real CLI, version **2.1.314**, with
`scripts/claude_wire_capture.py` and then sanitised (`--sanitize`: paths,
account fields and the lists that describe the recording machine's plugins).
They are what `src/ai/` was written against, and what its tests replay.

Each line is `{"t": seconds since spawn, "dir": "out" | "in", "msg": {...}}`:
`out` is a line of the CLI's stdout, `in` a line the host wrote to its stdin.
`scripts/fake_claude.py` plays a file in this format back, so the same
recordings drive `tests/ai_client.rs`.

The CLI was started the way saturn starts it:

```
claude -p --input-format stream-json --output-format stream-json \
       --include-partial-messages --verbose --permission-prompt-tool stdio \
       --strict-mcp-config --mcp-config '{"mcpServers":{"saturn":{"type":"sdk","name":"saturn"}}}' \
       --model haiku
```

| File | What it shows |
|---|---|
| `basic.jsonl` | `initialize` handshake, in-band MCP `initialize` / `tools/list`, two text turns in one process, a turn with a base64 PNG block |
| `permission.jsonl` | `can_use_tool` for Bash, answered allow, then deny |
| `always.jsonl` | allow with `updatedPermissions` (a session rule): the same call does not ask again |
| `mcp.jsonl` | in-band `tools/call` returning text and an image, after its own `can_use_tool` |
| `interrupt.jsonl` | `interrupt` mid-stream, `set_model`, then another turn in the same process |
| `interrupt_pending.jsonl` | `interrupt` with a permission request open: `control_cancel_request` |
| `loggedout.jsonl` | a turn without a login (empty `CLAUDE_CONFIG_DIR`) |
| `auth_status*.json` | `claude auth status --json`, logged in (exit 0) and logged out (exit 1) |

What the recordings established:

- Everything works in-band from a plain host; no fallback (stdio MCP shim,
  `--permission-prompt-tool mcp__…`) was needed.
- Nothing is printed until the host writes. `system/init` comes at the start
  of every turn, not at process start, so the reply to the `initialize`
  control request is the only early sign of life. The in-band MCP handshake
  happens at start either way.
- The model reported by `initialize` (`models[0].resolvedModel`) is the
  account default, not necessarily the session's: user settings can override
  it. Only `system/init` (and the `set_model` reply) name the real one.
- An interrupted turn ends with `result` `subtype: error_during_execution`,
  `terminal_reason: aborted_streaming` or `aborted_tools`, and the process
  stays usable.
- `result.usage` sums the turn's API requests; `usage.iterations` holds the
  last one. `total_cost_usd` is the session's running total.
- Without a login the turn "succeeds" with `is_error: true`,
  `terminal_reason: api_error` and the text `Not logged in · Please run /login`,
  and the process then exits with status 1.
