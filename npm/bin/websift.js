#!/usr/bin/env node
// Thin launcher: exec the vendored `websift` binary fetched by install.js, forwarding args and
// stdio and propagating its exit code. stdio must pass through untouched — `websift mcp` speaks
// JSON-RPC over stdin and stdout.
"use strict";

const path = require("path");
const fs = require("fs");
const { spawnSync } = require("child_process");

const bin = process.platform === "win32" ? "websift.exe" : "websift";
const binPath = path.join(__dirname, "..", "vendor", bin);

if (!fs.existsSync(binPath)) {
  console.error(
    "websift: binary not found. Reinstall with `npm i -g @suiflex/websift` (postinstall fetches it)."
  );
  process.exit(1);
}

const res = spawnSync(binPath, process.argv.slice(2), { stdio: "inherit" });
if (res.error) {
  console.error(`websift: ${res.error.message}`);
  process.exit(1);
}
process.exit(res.status ?? 0);
