#!/usr/bin/env node
// Set the same version across the main package, both platform packages, and the
// main package's optionalDependencies pins. Run from the repo root:
//   node scripts/sync-versions.mjs 0.2.0
// (the release workflow passes the git tag with the leading `v` stripped).

import { readFileSync, writeFileSync } from "node:fs";

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(version)) {
  console.error(`usage: sync-versions.mjs <semver>  (got: ${version ?? "nothing"})`);
  process.exit(1);
}

const platformPkgs = ["anyagent-darwin-arm64", "anyagent-linux-x64"];
const files = [
  "package.json",
  "npm/darwin-arm64/package.json",
  "npm/linux-x64/package.json",
];

for (const file of files) {
  const pkg = JSON.parse(readFileSync(file, "utf8"));
  pkg.version = version;
  if (pkg.optionalDependencies) {
    for (const dep of platformPkgs) {
      if (dep in pkg.optionalDependencies) {
        pkg.optionalDependencies[dep] = version;
      }
    }
  }
  writeFileSync(file, JSON.stringify(pkg, null, 2) + "\n");
  console.log(`${file} -> ${version}`);
}
