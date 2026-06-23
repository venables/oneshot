# anyagent — Specification

A Rust CLI that emulates `claude -p` (print mode) by driving the `claude`
binary in **interactive mode** under a PTY, submitting the prompt as a
positional argument, and capturing the final assistant message via a `Stop`
hook. Stdout matches what `claude -p` would emit for the same prompt and flags.

## 1. Premise

Print mode may be unavailable or unreliable in a given environment. The
remaining option for non-interactive use is to run `claude` interactively and
look like a real terminal:

1. A real PTY is required — Ink (claude's TUI runtime) bails on non-TTY stdin.
2. The terminal must answer DA1 / DA2 / XTVERSION / cursor-position /
   window-size probes during Ink startup, or the UI hangs.
3. We need a reliable "turn finished" signal, not screen-scraping.

We solve (1) with `portable-pty`, (2) with a small stateful ANSI responder
(`dec.rs`), and (3) with a `Stop` hook.

### Departure from the original

The Zig original typed the prompt into the TUI, which forced three timing
heuristics (wait-for-Ink-quiescence, Enter-debounce, bracketed-paste
handling). We pass the prompt as a **positional argument** so interactive mode
auto-submits it. The driver therefore has _no_ input-timing machinery; only
the `Stop` hook is load-bearing (`SessionStart` is just a readiness/debug
signal and a source of `transcript_path` for streaming).

## 2. Architecture

```
argv -> hook harness (FIFO + relay script + --settings)
     -> pty::spawn  claude "<prompt>" --settings <json>   [positional prompt]
        |                                   |
   main thread                         pump thread
   - poll FIFO for hook events         - read PTY master
   - on SessionStart: mark ready       - feed dec responder, write replies
   - on Stop: capture payload          - dismiss trust dialog (pre-session)
   - (stream) tail transcript -> stdout- detect child EOF
   - summarize, teardown
```

### Modules (`src/`)

| File                 | Responsibility                                                              |
| -------------------- | --------------------------------------------------------------------------- |
| `main.rs`            | CLI entry; stdin prompt; adapter dispatch; format dispatch; exit codes.     |
| `args.rs`            | Argparse; rejects `--settings`; forwards unknown flags.                     |
| `harness.rs`         | `--harness` selection; known names + custom path.                           |
| `adapters/mod.rs`    | `Adapter` trait; `for_harness` dispatch; shared `RunOutcome`/`DriverError`. |
| `adapters/claude.rs` | Claude protocol: pump thread, FIFO poll, streaming, teardown.               |
| `dec.rs`             | Stateful DEC/XTerm query responder (carry buffer across reads).             |
| `hook.rs`            | Temp dir + FIFO + relay script + inline `--settings` JSON; payload parse.   |
| `pty.rs`             | PTY spawn (execs argv directly — no `sh -c`).                               |
| `stream.rs`          | `read_at`-based transcript tailer (holds back torn lines).                  |
| `transcript.rs`      | Session JSONL parser → final text + usage + flags.                          |
| `emit.rs`            | text / json / stream-json formatters.                                       |
| `signals.rs`         | SIGINT/SIGTERM → flag; lets the loop tear down and exit 130.                |

Adapters live in `src/adapters/`: each backend agent CLI implements the
`Adapter` trait in its own module, so adding a harness is "drop a file in
`adapters/` and wire it into `for_harness`". The Claude protocol (PTY + Stop
hook) is one such adapter; harnesses with a real non-interactive mode (codex,
opencode) will be plain subprocess adapters with no PTY/hook machinery.

### 2.1 Concurrency

One pump thread owns both halves of the PTY master, so DEC responses and the
trust-dialog Enter are written from the same thread that reads — no
cross-thread re-entrancy and no mutex on the write path (the original needed
one because the PTY library owned the reader thread). The main thread owns the
FIFO and the child handle. Shared state is two atomics (`exited`,
`session_started`) and a diagnostics tail behind a mutex.

### 2.2 Completion + transcript race

`Stop` can fire a few ms before the assistant line is flushed. For `text` we
use the payload's `last_assistant_message` directly (no wait). For
`json`/`stream-json` we retry `parse_file` (≤40 × 50 ms) until non-empty,
falling back to the payload message.

### 2.3 Teardown

We kill the child's **process group** (it is a PTY session leader): SIGTERM,
≤300 ms grace, then SIGKILL, then reap. The temp dir/FIFO are removed by the
`HookHarness` `Drop`. SIGINT/SIGTERM set a flag the loop observes, so Ctrl-C
does not orphan the child.

### 2.4 Workspace-trust dialog

Detected by CSI-stripped substring match ("trust" + "folder") on pre-session
output only; dismissed with Enter. Gating to before `SessionStart` ensures a
later assistant message can never trigger a stray keystroke. (`--dangerously-
skip-permissions` does not suppress this dialog.)

## 3. Output fidelity

| Format        | Stdout                                                             |
| ------------- | ------------------------------------------------------------------ |
| `text`        | Final assistant message + `\n`.                                    |
| `json`        | `{answer, metadata}` — the answer plus the authoritative envelope. |
| `stream-json` | Transcript JSONL lines live, then the trailing `result` object.    |

### 3.1 Metadata side channel

`--meta-file <path>` writes the authoritative run metadata (`meta.rs`) as a
JSON object, distinct from the answer on stdout: `harness`, `harness_version`
(best-effort), `model_requested`, `model_resolved`, `duration_ms`,
`exit_status`, `session_id`, `num_turns`, `total_cost_usd`, `usage`.
`model_resolved` is read from the transcript's assistant events
(`message.model`) — the launcher's truth, not the agent's self-report — and is
`"unknown"` rather than an echo when the harness never exposed it. Requesting a
meta file forces the transcript read even for `text` output, so the resolved
model is always authoritative.

### 3.2 Exit codes

A stable API: `0` ok · `10` agent-error · `20` timeout · `30`
harness-not-found · `31` invalid-model · `32` enforcement-unsupported · `130`
interrupted · `2` internal. The `exit_status` metadata field carries the
matching label. (`31`/`32` are reserved for Phases 6/4.)

## 4. Public surface

`Adapter::run(opts, stream_out) -> Result<RunOutcome, DriverError>`, dispatched
via `adapters::for_harness`. CLI flags map onto `Options` (see `args.rs`).
`-H`/`--harness` chooses the backend. Implemented: `claude` (PTY + Stop hook)
and `codex` (`codex exec`, a plain subprocess reading the `--json` event
stream); a custom path is driven as a claude-compatible binary.
`ANYAGENT_CLAUDE_BIN` overrides the `claude` binary. The codex adapter reads
`model_resolved` from codex's session rollout file (`turn_context.payload.model`)
since the event stream omits it.

## 5. Test plan

1. `dec` — recorded VT bytes → expected replies, incl. split-across-reads.
2. `transcript` — fixtures → final message + usage totals.
3. `hook` — settings JSON shape; FIFO/script lifecycle; payload extraction.
4. `args` — every flag and rejection.
5. `emit` — golden text/json shapes.
6. `stream` — tailer line-buffering across appends.
7. `tests/integration.rs` — real `claude`, gated on `ANYAGENT_E2E=1`.

## 6. Non-goals

- Windows (no Unix PTY).
- Per-token streaming (print-mode-only).
- Tool-approval prompting (use `--dangerously-skip-permissions`/`--allowedTools`).

## 7. Risks

| Risk                                | Mitigation                                                    |
| ----------------------------------- | ------------------------------------------------------------- |
| Hook payload schema change          | Parse defensively; fall back to transcript / payload message. |
| New Ink startup probe               | Add a case to `dec::DecResponder::respond`.                   |
| Wrapper injects `--settings` (cmux) | `ANYAGENT_CLAUDE_BIN` to bypass.                              |
| Child outlives parent               | Process-group SIGTERM→SIGKILL; SIGINT handler.                |
