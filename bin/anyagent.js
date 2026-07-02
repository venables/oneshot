#!/usr/bin/env node
"use strict";

// Thin launcher: resolve the prebuilt binary from the matching per-platform
// optionalDependency (anyagent-<platform>-<arch>) and hand off to it with the
// terminal's stdio inherited, so anyagent still sees a real TTY for its own PTY
// work. Only the arch/OS actually installed by npm is present on disk.

const { spawnSync } = require("node:child_process");
const path = require("node:path");

function binaryPath() {
  const pkg = `anyagent-${process.platform}-${process.arch}`;
  try {
    const pkgJson = require.resolve(`${pkg}/package.json`);
    return path.join(path.dirname(pkgJson), "bin", "anyagent");
  } catch {
    const supported = "darwin-arm64, linux-x64";
    throw new Error(
      `anyagent: no prebuilt binary for ${process.platform}-${process.arch} ` +
        `(supported: ${supported}). Install from source instead: ` +
        `cargo install --path .`,
    );
  }
}

let bin;
try {
  bin = binaryPath();
} catch (err) {
  console.error(err.message);
  process.exit(1);
}

const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  console.error(`anyagent: failed to run ${bin}: ${result.error.message}`);
  process.exit(1);
}
// Re-raise the child's terminating signal so the parent's exit reflects it;
// otherwise propagate the exit code.
if (result.signal) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status ?? 0);
