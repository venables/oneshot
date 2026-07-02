# anyagent

One non-interactive interface in front of any coding agent.

## Use

```bash
anyagent "your prompt here"
anyagent --output-format json "summarize this" < diff.txt
anyagent --output-format stream-json "audit src/" | jq .
anyagent --model opus "explain quicksort to a 10-year-old"
anyagent --harness claude "which harness am I?"
```

If no prompt argument is given, the prompt is read from stdin.

## Commands

```bash
anyagent "<prompt>"                 # sugar for `run` with defaults
anyagent run [flags] -- "<prompt>"  # explicit run
anyagent list harnesses             # installed + implemented/reserved + version
anyagent list models [--harness X]  # best-effort model discovery
anyagent capabilities [--harness X] # per-harness perms->enforcement, network, outputs
anyagent --help | --version
```

`run`/`list`/`capabilities` are recognised only as the first argument (like
git); any other first token is treated as a prompt, so a prompt starting with
one of those words can be forced with `anyagent run -- "run the tests"`.
`capabilities` is what lets an orchestrator stop hardcoding harness knowledge:

```
$ anyagent capabilities --harness codex
harness: codex
perms:
  read-only        os-sandbox
  workspace-write  os-sandbox
  full             none
network-control: yes (sandbox blocks network)
output-modes: text, json, stream-json
```

## Harnesses

`-A` / `--agent <name|path>` (alias `-H` / `--harness`) selects which agent CLI
to drive. Implemented today:

- **`claude`** (default) — `claude -p` print mode, a plain subprocess.
  Authoritative metadata: model, usage, and cost come straight from claude's
  own JSON envelope.
- **`codex`** — the natively non-interactive `codex exec`, a plain subprocess.

`opencode`, `gemini`, and `pi` are recognised and reserved (selecting one fails
fast until it's wired up). A value that isn't a known name is treated as a path
to a **claude-compatible** binary and driven via `claude -p` — handy for a fork
or a wrapper shim. The default is `claude`.

Passing **`--pty`** drives the agent's interactive TUI under a PTY instead of
its native non-interactive mode — a fallback for environments where the latter
(e.g. `claude -p`) is unavailable. It can't expose model/usage (see Caveats), so
prefer the native drive.

## How it works

The default **`claude`** harness simply runs `claude -p --output-format json`
and parses the result envelope (answer, usage, cost, and the `modelUsage` key
that gives the authoritative model). codex similarly runs `codex exec --json`.
Neither needs a PTY.

The **`--pty`** fallback is the original mechanism — driving the interactive TUI
under a PTY, for environments where `claude -p` doesn't work:

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
--agent <name|path> | -A                  claude (default) | codex | … | /path
                                          (alias: --harness | -H)
--output-format <text|json|stream-json>   default: text
--model <name>
--dangerously-skip-permissions
--perms <read-only|workspace-write|full>   permission tier (by intent)
--network <none|restricted|full>           network tier (by intent)
--require-enforcement <os-sandbox|any>     demand an enforcement class (else exit 32)
--cwd <path>                               working directory for the child
--meta-file <path>                         write the run-metadata envelope here
--timeout <seconds>                        wrapper wall-time cap (default 300)
--pty                                      drive the interactive TUI under a PTY
                                           (when native non-interactive mode is unavailable)
--cols <n> / --rows <n>                    PTY size (with --pty; default 120x40)
--debug | -d                               wrapper debug traces on stderr
--                                         end-of-options; rest is the prompt
```

Unrecognised flags are forwarded to `claude`. `-p`/`--print` is accepted but
ignored — anyagent already emulates print mode, so the flag is redundant, and
swallowing it lets callers that invoke `claude -p "..."` point at anyagent
unchanged. A user-supplied `--settings` is rejected (we inject our own settings
for the Stop hook).

`--model default` is the explicit way to ask for the harness's own default
(reported as `model_requested: "default"`); any other value passes through and
the harness validates it live. When the harness rejects a model, exit is `31`
with its own message — e.g. codex's _"The 'x' model is not supported…"_.

> **Note:** `--flag=value` works for any flag, and common claude value-flags
> (`--allowedTools`, `--system-prompt`, `--add-dir`, `--resume`, …) forward
> with their values. A _space-separated_ value for an _unrecognised_ flag is
> the one remaining gap — pass it as `--flag=value`.

## Permissions & enforcement

`--perms` requests a permission tier _by intent_; each harness maps it to its
native mechanism, and the metadata reports the **enforcement class** actually
achieved — honestly, instead of a uniform-looking flag that lies.

| intent            | codex (`codex exec`)                     | claude                               |
| ----------------- | ---------------------------------------- | ------------------------------------ |
| `read-only`       | `--sandbox read-only` (os-sandbox)       | `--disallowedTools …` (agent-policy) |
| `workspace-write` | `--sandbox workspace-write` (os-sandbox) | bypassPermissions (none)             |
| `full`            | `--sandbox danger-full-access` (none)    | bypassPermissions (none)             |

`--require-enforcement os-sandbox` makes the difference enforceable: it fails
fast (exit 32) when the harness can't meet the demand, before anything runs.

```bash
anyagent --harness claude --perms read-only --require-enforcement os-sandbox "…"
# anyagent: claude can only enforce read-only via agent-policy, not os-sandbox
# (exit 32)
```

v1 is **report + passthrough**: it maps to native flags and reports the truth;
it does not yet _add_ a sandbox to a harness that lacks one. `--network none`
is OS-enforced only where the harness's sandbox already blocks network (codex
read-only / workspace-write).

## Output contract

- **stdout** carries only the agent's answer (`text`), or `{answer, metadata}`
  (`--output-format json`).
- **`--meta-file <path>`** writes the authoritative run-metadata envelope to a
  side channel, distinct from the answer:

  ```json
  {
    "harness": "claude", "drive": "print", "harness_version": null,
    "model_requested": "opus", "model_resolved": "claude-opus-4-8",
    "duration_ms": 84213, "exit_status": "ok",
    "session_id": "…", "num_turns": 1, "total_cost_usd": 0.04,
    "usage": { "input_tokens": 1200, "output_tokens": 800, … }
  }
  ```

  `model_resolved` is read from the transcript (the launcher's truth), not the
  agent's self-report; it is `"unknown"` when the harness never exposed it.
  `drive` is adapter-provided — `"print"` (claude native), `"exec"` (codex), or
  `"pty"` for the `--pty` fallback (`"unknown"` when no adapter ran) — so a
  `"pty"` run's `unknown`/0 model+usage reads as a mode limitation, not missing
  data (and it never claims `"pty"` for a harness with no PTY drive).

## Exit codes

Exit codes are a stable API orchestrators can branch on.

| Code  | `exit_status`             | Meaning                                       |
| ----- | ------------------------- | --------------------------------------------- |
| `0`   | `ok`                      | Success.                                      |
| `10`  | `agent-error`             | Assistant errored, or no message recoverable. |
| `20`  | `timeout`                 | Timed out (before or after the UI came up).   |
| `30`  | `harness-not-found`       | The selected harness has no adapter.          |
| `31`  | `invalid-model`           | Harness rejected the requested model.         |
| `32`  | `enforcement-unsupported` | Harness can't meet `--require-enforcement`.   |
| `130` | `interrupted`             | Interrupted (SIGINT/SIGTERM).                 |
| `2`   | `internal`                | Wrapper internal error (spawn/PTY/IO).        |

## Caveats

- **macOS / Linux only** (no Windows; needs a Unix PTY).
- **Requires `claude` on `$PATH`** (or set `ANYAGENT_CLAUDE_BIN`, below).
- **`--pty` can't report model/usage.** claude writes its transcript only
  in print mode or on a clean TUI exit — not while the PTY session is alive, and
  the Stop payload omits both — so a `--pty` run honestly reports
  `model_resolved: "unknown"` and usage `0`. Use the default (native) drive
  for authoritative metadata.
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

**Not published to npm yet** — `package.json` is marked `"private": true` to
prevent a broken publish. The reason: a Rust binary is platform-specific, but
the package declares `os: [darwin, linux]` × `cpu: [x64, arm64]` while shipping a
single `bin/anyagent`. Whichever machine runs `npm publish` bakes in that one
target, and the static `os`/`cpu` fields still let mismatched platforms install
it and get a binary that won't run. There is also no CI to build the matrix.

Install from source in the meantime:

```bash
cargo install --path .
# or: cargo build --release && use ./target/release/anyagent
```

To publish for real, pick one and drop `"private"`:

- **Per-platform `optionalDependencies`** (esbuild/swc model): a CI matrix builds
  all four targets and publishes one package each (correct `os`/`cpu` + its
  binary); the main package resolves the right one via a tiny launcher. Correct
  and `npx`-friendly.
- **`postinstall` downloader**: one package whose postinstall fetches the
  matching prebuilt binary from a GitHub Release. Simpler to publish, needs
  network at install time.

Either way, add a `prepublishOnly` script that builds and copies the binary so a
publish can't ship an empty `bin/`.

## License

MIT.
