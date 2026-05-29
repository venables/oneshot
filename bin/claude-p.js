#!/usr/bin/env node
// npm shim: resolve the platform binary and exec it, forwarding argv, stdio,
// exit code, and signals. The Rust binary is the source of truth.
"use strict";

const { spawnSync } = require("node:child_process");
const path = require("node:path");
const fs = require("node:fs");

function resolveBinary() {
  const name = "claude-p";
  // Published layout: prebuilt/<platform>-<arch>/claude-p
  const prebuilt = path.join(
    __dirname,
    "..",
    "prebuilt",
    `${process.platform}-${process.arch}`,
    name,
  );
  if (fs.existsSync(prebuilt)) return prebuilt;
  // Dev fallback: a local cargo build.
  for (const profile of ["release", "debug"]) {
    const dev = path.join(__dirname, "..", "target", profile, name);
    if (fs.existsSync(dev)) return dev;
  }
  return null;
}

const binary = resolveBinary();
if (!binary) {
  process.stderr.write(
    "claude-p: no binary for this platform.\n" +
      "  Build from source: `cargo build --release`\n" +
      "  Or reinstall to fetch a prebuilt binary.\n",
  );
  process.exit(2);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  process.stderr.write(`claude-p: ${result.error.message}\n`);
  process.exit(2);
}
if (result.signal) {
  // Re-raise the signal so the parent sees the same termination cause.
  process.kill(process.pid, result.signal);
}
process.exit(result.status === null ? 2 : result.status);
