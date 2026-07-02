# TODO

Tracking known gaps and future work. The spike works end-to-end (text / json /
stream-json validated against the real `claude` binary), but several things are
deliberately incomplete.

## Known limitations (from the spike)

- [ ] **Flag passthrough fidelity.** Unknown flags that take a _value_ only
      forward the flag token, not the value (`src/args.rs`). Workaround today:
      `--flag=value` or text after `--`. Fix: track arity, or port the original's
      explicit allowlist.
- [ ] **Real prebuilt-binary download.** `scripts/install.js` is a no-op stub.
      Needs a GitHub release + CI matrix building `aarch64/x86_64 × macos/linux`,
      then a real fetch keyed on `process.platform`/`process.arch`.
- [ ] **E2E flakiness.** Occasionally one of the three integration tests fails
      with a non-zero exit when real sessions run back-to-back (transient API /
      startup hiccup, not a code bug). Consider a bounded retry in the test
      harness, and/or `--test-threads=1` spacing.

## Feature parity with the original

- [ ] **Session reuse:** `--resume <id>`, `--continue`, `--session-id <uuid>`.
- [ ] **More forwarded flags** with explicit handling (so values survive):
      `--allowedTools`, `--disallowedTools`, `--system-prompt`,
      `--append-system-prompt`, `--permission-mode`, `--fallback-model`,
      `--add-dir` (repeatable), `--mcp-config` (repeatable), `--setting-sources`,
      `--verbose`, `--max-turns`.
- [ ] **`--input-file <path>`** for multiline prompts (stdin already works).
- [ ] **Golden-output tests** asserting byte-for-byte parity with `claude -p`
      for text and json (capture fixtures from the real CLI).

## Robustness / polish

- [ ] **Replace the 10ms main-loop sleep with `poll()`** on the FIFO fd (woken
      by a self-pipe/eventfd when the pump thread queues nothing — here only the
      FIFO matters since the pump owns PTY writes). Lower latency, no spin.
- [ ] **Forward SIGINT to the child** as well as flagging it, so Ctrl-C
      interrupts an in-flight turn promptly rather than only at teardown.
- [ ] **Trust-dialog handling:** investigate a config-based pre-trust (without
      polluting `~/.claude`) so we can drop the screen-scrape entirely; failing
      that, tighten the matcher to the exact dialog string.
- [ ] **Resilience to hook-payload schema drift:** add a regression fixture and
      keep the transcript fallback paths covered.
- [ ] **`--add-dir` / cwd semantics:** confirm the child's working directory
      matches user expectation (observed claude reporting `cwd: $HOME` in one run).

## Out of scope (documented non-goals)

- Windows (needs a Unix PTY; `portable-pty` supports ConPTY but the hook/FIFO
  plumbing is Unix-only today).
- Per-token streaming within a single assistant message (print-mode only, via
  `--include-partial-messages`).
- Tool-approval prompting (use `--dangerously-skip-permissions` /
  `--allowedTools`).

## Notes

- On machines where `claude` on PATH is a wrapper that injects its own
  `--settings` (e.g. cmux), set `CLAUDE_P_CLAUDE_BIN=/path/to/real/claude`.
