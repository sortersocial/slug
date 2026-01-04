#!/usr/bin/env node
// Ultra-thin shim: download (once) and exec the Rust `slugsocial` binary.
//
// Env:
// - SLUGSOCIAL_BIN: path to an existing slugsocial binary (skips download)
// - SLUGSOCIAL_RELEASE_BASE: e.g. "https://github.com/<org>/<repo>/releases/download"
// - SLUGSOCIAL_TAG: release tag, e.g. "v0.0.1" (defaults to "v" + package.json version)

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const https = require("node:https");
const childProcess = require("node:child_process");

function die(msg) {
  console.error(msg);
  process.exit(1);
}

function env(name, fallback = undefined) {
  const v = process.env[name];
  if (!v || !v.trim()) return fallback;
  return v.trim();
}

function platformTriple() {
  const p = process.platform;
  const a = process.arch;

  const plat = p === "darwin" ? "darwin" : p === "linux" ? "linux" : p === "win32" ? "windows" : null;
  const arch = a === "x64" ? "x64" : a === "arm64" ? "arm64" : null;
  if (!plat || !arch) {
    die(`unsupported platform/arch: ${p}/${a}`);
  }
  return { plat, arch };
}

function defaultCacheDir() {
  // Simple, portable default.
  return path.join(os.homedir(), ".cache", "slugsocial");
}

function download(url, destPath) {
  return new Promise((resolve, reject) => {
    const tmpPath = `${destPath}.tmp`;
    const f = fs.createWriteStream(tmpPath, { mode: 0o755 });
    https
      .get(url, (res) => {
        if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          f.close();
          try { fs.unlinkSync(tmpPath); } catch {}
          return resolve(download(res.headers.location, destPath));
        }
        if (res.statusCode !== 200) {
          f.close();
          try { fs.unlinkSync(tmpPath); } catch {}
          return reject(new Error(`download failed: ${res.statusCode} ${url}`));
        }
        res.pipe(f);
        f.on("finish", () => {
          f.close(() => {
            // Basic corruption guard: reject HTML error pages or empty files.
            try {
              const head = fs.readFileSync(tmpPath, { encoding: null }).subarray(0, 64);
              const trimmed = Buffer.from(head).toString("utf8").trimStart();
              if (head.length === 0 || trimmed.startsWith("<")) {
                try { fs.unlinkSync(tmpPath); } catch {}
                return reject(new Error(`downloaded non-binary content from ${url}`));
              }
              fs.renameSync(tmpPath, destPath);
              resolve();
            } catch (e) {
              try { fs.unlinkSync(tmpPath); } catch {}
              reject(e);
            }
          });
        });
      })
      .on("error", (err) => {
        f.close();
        try { fs.unlinkSync(tmpPath); } catch {}
        reject(err);
      });
  });
}

async function ensureBinary() {
  const explicit = env("SLUGSOCIAL_BIN");
  if (explicit) return explicit;

  const pkg = require("../package.json");
  const tag = env("SLUGSOCIAL_TAG", `v${pkg.version}`);
  const base = env("SLUGSOCIAL_RELEASE_BASE", "https://github.com/your-org/your-repo/releases/download");

  const { plat, arch } = platformTriple();
  const ext = process.platform === "win32" ? ".exe" : "";
  const asset = `slugsocial-${plat}-${arch}${ext}`;
  const url = `${base}/${tag}/${asset}`;

  const cacheDir = defaultCacheDir();
  fs.mkdirSync(cacheDir, { recursive: true });
  const binPath = path.join(cacheDir, `${asset}-${tag}`);

  if (!fs.existsSync(binPath)) {
    await download(url, binPath);
    if (process.platform !== "win32") {
      fs.chmodSync(binPath, 0o755);
    }
  }
  return binPath;
}

async function main() {
  const bin = await ensureBinary();
  const args = process.argv.slice(2);
  const res = childProcess.spawnSync(bin, args, { stdio: "inherit" });
  process.exit(res.status ?? 1);
}

main().catch((e) => die(String(e?.message ?? e)));


