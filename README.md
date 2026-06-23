# anyagent

> **Use at your own risk, educational purposes.** This drives interactive agent
> CLIs in a way they aren't designed for. Prefer a supported print mode (e.g.
> `claude -p`) when it works for you; reach for this only when it doesn't.

One non-interactive interface in front of any coding agent. Today it wraps
`claude`; the goal is a single, uniform pipe over other agents (codex, …) as
they're added.

For the `claude` backend it's a drop-in replacement for `claude -p`: it runs the
interactive `claude` TUI inside a real PTY, submits your prompt, and captures the
final assistant message via a `Stop` hook. Output on stdout matches `claude -p`
for the same prompt.

This is a Rust reimplementation of the Zig project
[smithersai/claude-p](https://github.com/smithersai/claude-p), with one core
simplification: **the prompt is passed as a positional argument** (`claude
"prompt"` auto-submits in interactive mode), so there is no keystroke-typing,
no "wait for the UI to settle" heuristic, and no Enter-debounce. The only
thing written back to the PTY is the terminal-probe responses Ink needs at
startup, plus a single Enter to dismiss the workspace-trust dialog if it
appears.

## Use

```bash
anyagent "your prompt here"
anyagent --output-format json "summarize this" < diff.txt
anyagent --output-format stream-json "audit src/" | jq .
anyagent --model opus "explain quicksort to a 10-year-old"
anyagent --harness claude "which harness am I?"
```

If no prompt argument is given, the prompt is read from stdin.

## Harnesses

`-H` / `--harness <name|path>` selects which agent CLI to drive. Implemented
today: `claude` (interactive TUI under a PTY + Stop hook) and `codex` (the
natively non-interactive `codex exec`, driven as a plain subprocess — no
PTY/hook). `opencode`, `gemini`, and `pi` are recognised and reserved
(selecting one fails fast until it's wired up). A value that isn't a known name
is treated as a path to a **claude-compatible** binary and driven with the
Claude protocol — handy for a fork or a wrapper shim. The default is `claude`.

## How it works

1. Spawns `claude "<prompt>" --settings '<inline-json>'` on a real PTY
   (`openpty`/`forkpty` via `portable-pty`). The prompt is a positional arg,
   so interactive mode submits it immediately.
2. A small ANSI responder answers the DA1 / DA2 / DSR / XTVERSION / window-size
   queries Ink issues at startup (it is _stateful_ across reads, so a query
   split across a PTY read boundary is still answered). Without these the TUI
   hangs.
3. Registers `SessionStart` and `Stop` hooks via `--settings` — never touches
   your `~/.claude/` config. A relay script appends the hook payload to a
   per-run FIFO the driver polls.
4. On `Stop`, reads the final assistant message (from the payload's
   `last_assistant_message` for text, or the transcript JSONL for json /
   stream-json), prints it, and tears the child's process group down.

## Flags

```
--harness <name|path> | -H                claude (default) | codex | … | /path
--output-format <text|json|stream-json>   default: text
--model <name>
--dangerously-skip-permissions
--cwd <path>                               working directory for the child
--meta-file <path>                         write the run-metadata envelope here
--timeout <seconds>                        wrapper wall-time cap (default 300)
--cols <n> / --rows <n>                    PTY size (default 120x40)
--debug | -d                               wrapper debug traces on stderr
--                                         end-of-options; rest is the prompt
```

Unrecognised flags are forwarded to `claude`. `-p`/`--print` is accepted but
ignored — anyagent already emulates print mode, so the flag is redundant, and
swallowing it lets callers that invoke `claude -p "..."` point at anyagent
unchanged. A user-supplied `--settings` is rejected (we inject our own settings
for the Stop hook).

> **Note:** `--flag=value` works for any flag, and common claude value-flags
> (`--allowedTools`, `--system-prompt`, `--add-dir`, `--resume`, …) forward
> with their values. A _space-separated_ value for an _unrecognised_ flag is
> the one remaining gap — pass it as `--flag=value`.

## Output contract

- **stdout** carries only the agent's answer (`text`), or `{answer, metadata}`
  (`--output-format json`).
- **`--meta-file <path>`** writes the authoritative run-metadata envelope to a
  side channel, distinct from the answer:

  ```json
  {
    "harness": "claude", "harness_version": null,
    "model_requested": "opus", "model_resolved": "claude-opus-4-8",
    "duration_ms": 84213, "exit_status": "ok",
    "session_id": "…", "num_turns": 1, "total_cost_usd": 0.04,
    "usage": { "input_tokens": 1200, "output_tokens": 800, … }
  }
  ```

  `model_resolved` is read from the transcript (the launcher's truth), not the
  agent's self-report; it is `"unknown"` when the harness never exposed it.

## Exit codes

Exit codes are a stable API orchestrators can branch on.

| Code  | `exit_status`             | Meaning                                                |
| ----- | ------------------------- | ------------------------------------------------------ |
| `0`   | `ok`                      | Success.                                               |
| `10`  | `agent-error`             | Assistant errored, or no message recoverable.          |
| `20`  | `timeout`                 | Timed out (before or after the UI came up).            |
| `30`  | `harness-not-found`       | The selected harness has no adapter.                   |
| `31`  | `invalid-model`           | Unknown model for the harness (reserved).              |
| `32`  | `enforcement-unsupported` | Harness can't meet `--require-enforcement` (reserved). |
| `130` | `interrupted`             | Interrupted (SIGINT/SIGTERM).                          |
| `2`   | `internal`                | Wrapper internal error (spawn/PTY/IO).                 |

## Caveats

- **macOS / Linux only** (no Windows; needs a Unix PTY).
- **Requires `claude` on `$PATH`** (or set `ANYAGENT_CLAUDE_BIN`, below).
- **Per-message streaming, not per-token.** `stream-json` emits transcript
  lines as `claude` flushes them, then a trailing `result` envelope.
  Per-token streaming needs `claude -p --include-partial-messages`, which is
  print-mode only.
- **API instability.** `claude` is not designed to be driven this way. A
  release that changes the hook payload or adds a new startup terminal probe
  can break this; failures surface rather than hide.

### `ANYAGENT_CLAUDE_BIN`

If `claude` on your `PATH` is a wrapper that injects its own `--settings`
(e.g. the **cmux** shim), it will clobber ours and no hooks fire. Point
directly at the real binary:

```bash
ANYAGENT_CLAUDE_BIN=/path/to/real/claude anyagent "say hi"
```

Equivalently, point `--harness` straight at the real binary:
`anyagent --harness /path/to/real/claude "say hi"`.

## Build & test

```bash
cargo build --release          # binary at target/release/anyagent
cargo test                     # unit tests (hermetic, no claude needed)

# End-to-end against the real claude binary:
ANYAGENT_E2E=1 ANYAGENT_CLAUDE_BIN=/path/to/claude \
  cargo test --test integration -- --test-threads=1
```

## Packaging (npm)

The npm package ships the compiled binary directly — `bin` points at
`bin/anyagent`, so `npm install` symlinks it with no Node shim in the path.
Before publishing, build and copy the binary into place:

```bash
cargo build --release
install -m 755 target/release/anyagent bin/anyagent   # gitignored; shipped via "files"
npm publish
```

> A single tarball contains one binary, so it is **platform-specific** (it
> matches the machine it was built on). To cover macOS + Linux × x64 + arm64,
> publish one package per target, or move to per-platform `optionalDependencies`
> (which reintroduces a small launcher). The `os`/`cpu` fields gate installs but
> do not make one binary portable.

## License

MIT.
