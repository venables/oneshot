#!/usr/bin/env node
// postinstall: best-effort fetch of the prebuilt binary for this platform.
//
// STUB: there is no published release yet, so this is intentionally a no-op
// that always succeeds. bin/claude-p.js falls back to a local `cargo build`
// (target/release|debug) and prints a clear error if no binary is found.
//
// When releases exist, download prebuilt/<platform>-<arch>/claude-p here.
// Set CLAUDE_P_SKIP_DOWNLOAD=1 to skip (e.g. monorepo bootstraps / CI).
"use strict";

if (process.env.CLAUDE_P_SKIP_DOWNLOAD === "1") {
  process.exit(0);
}

// TODO: fetch from GitHub releases once published. No-op for now.
process.exit(0);
