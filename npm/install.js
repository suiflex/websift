#!/usr/bin/env node
// postinstall: download the prebuilt `websift` binary from the GitHub Release whose tag matches
// this package's version, and drop it in vendor/.
//
// ponytail: shells out to the system `tar` (bsdtar reads both .tar.gz and .zip on macOS, Linux,
// and Windows 10+) rather than taking on an archive dependency.
"use strict";

const fs = require("fs");
const path = require("path");
const https = require("https");
const { execFileSync } = require("child_process");

const REPO = "suiflex/websift";

// platform/arch -> { target triple, archive extension, binary name }. Pure, so `--selftest` can
// check it without touching the network.
function resolveTarget(platform, arch) {
  const map = {
    "darwin:arm64": ["aarch64-apple-darwin", "tar.gz", "websift"],
    "darwin:x64": ["x86_64-apple-darwin", "tar.gz", "websift"],
    "linux:x64": ["x86_64-unknown-linux-gnu", "tar.gz", "websift"],
    "linux:arm64": ["aarch64-unknown-linux-gnu", "tar.gz", "websift"],
    "win32:x64": ["x86_64-pc-windows-msvc", "zip", "websift.exe"],
    "win32:arm64": ["aarch64-pc-windows-msvc", "zip", "websift.exe"],
  };
  const hit = map[`${platform}:${arch}`];
  if (!hit) {
    throw new Error(`unsupported platform ${platform}/${arch}`);
  }
  const [target, ext, bin] = hit;
  return { target, ext, bin };
}

// Release assets carry the tag as well as the triple, and `websift update` resolves them by this
// exact string. Kept pure alongside resolveTarget so both are covered by --selftest.
function assetName(version, target, ext) {
  return `websift-v${version}-${target}.${ext}`;
}

function download(url, dest, redirects = 0) {
  return new Promise((resolve, reject) => {
    if (redirects > 5) return reject(new Error("too many redirects"));
    https
      .get(url, { headers: { "User-Agent": "websift-npm-installer" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          return resolve(download(res.headers.location, dest, redirects + 1));
        }
        if (res.statusCode !== 200) {
          res.resume();
          return reject(new Error(`download failed: HTTP ${res.statusCode} for ${url}`));
        }
        const out = fs.createWriteStream(dest);
        res.pipe(out);
        out.on("finish", () => out.close(resolve));
        out.on("error", reject);
      })
      .on("error", reject);
  });
}

async function main() {
  const version = require("./package.json").version;
  const { target, ext, bin } = resolveTarget(process.platform, process.arch);
  const asset = assetName(version, target, ext);
  const url = `https://github.com/${REPO}/releases/download/v${version}/${asset}`;

  const vendor = path.join(__dirname, "vendor");
  fs.mkdirSync(vendor, { recursive: true });
  const archive = path.join(vendor, asset);

  process.stdout.write(`websift: downloading ${asset}\n`);
  await download(url, archive);

  execFileSync("tar", ["-xf", archive, "-C", vendor], { stdio: "inherit" });
  fs.unlinkSync(archive);

  const binPath = path.join(vendor, bin);
  if (!fs.existsSync(binPath)) {
    throw new Error(`extracted archive did not contain ${bin}`);
  }
  if (process.platform !== "win32") fs.chmodSync(binPath, 0o755);
  process.stdout.write(`websift: installed ${bin}\n`);
}

// `node install.js --selftest` exercises the pure mapping with no network.
if (process.argv.includes("--selftest")) {
  const assert = require("assert");
  assert.strictEqual(resolveTarget("darwin", "arm64").target, "aarch64-apple-darwin");
  assert.strictEqual(resolveTarget("linux", "arm64").target, "aarch64-unknown-linux-gnu");
  assert.strictEqual(resolveTarget("linux", "x64").ext, "tar.gz");
  assert.strictEqual(resolveTarget("win32", "x64").bin, "websift.exe");
  assert.strictEqual(resolveTarget("win32", "arm64").ext, "zip");
  assert.throws(() => resolveTarget("sunos", "sparc"));
  // Must match what the release workflow uploads, byte for byte.
  assert.strictEqual(
    assetName("0.2.2", "x86_64-apple-darwin", "tar.gz"),
    "websift-v0.2.2-x86_64-apple-darwin.tar.gz"
  );
  assert.strictEqual(
    assetName("1.0.0", "x86_64-pc-windows-msvc", "zip"),
    "websift-v1.0.0-x86_64-pc-windows-msvc.zip"
  );
  console.log("selftest ok");
  process.exit(0);
}

main().catch((err) => {
  console.error(`websift: install failed: ${err.message}`);
  process.exit(1);
});
