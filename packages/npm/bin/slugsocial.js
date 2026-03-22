#!/usr/bin/env node
// Ultra-thin shim: exec the Rust `slugsocial` binary bundled via platform packages.
//
// Binary comes from optionalDependencies for this platform (darwin/linux × arm64/x64).
// postinstall also installs the platform package if npm skipped optional deps (npx / omit=optional).

const fs = require("node:fs");
const path = require("node:path");
const childProcess = require("node:child_process");

function die(msg) {
  console.error(msg);
  process.exit(1);
}

function platformTriple() {
  const p = process.platform;
  const a = process.arch;

  // Package names use darwin/linux only (see optionalDependencies).
  const plat = p === "darwin" ? "darwin" : p === "linux" ? "linux" : null;
  const arch = a === "x64" ? "x64" : a === "arm64" ? "arm64" : null;
  if (!plat || !arch) {
    die(`unsupported platform/arch: ${p}/${a}`);
  }
  return { plat, arch };
}

function resolvePlatformPackage() {
  const { plat, arch } = platformTriple();
  const name = `@sortersocial/slugsocial-${plat}-${arch}`;
  const pkgRoot = path.join(__dirname, "..");
  try {
    const pkgJson = require.resolve(`${name}/package.json`, { paths: [pkgRoot] });
    return path.dirname(pkgJson);
  } catch (_) {
    /* optionalDependency missing or hoisted oddly */
  }
  const sibling = path.join(
    pkgRoot,
    "..",
    "@sortersocial",
    `slugsocial-${plat}-${arch}`,
    "package.json",
  );
  if (fs.existsSync(sibling)) {
    return path.dirname(sibling);
  }
  die(
    [
      `missing platform package ${name}.`,
      `Reinstall with optional deps, e.g. npm install slugsocial --include=optional`,
      `or: npm install ${name}@<same version as slugsocial>`,
    ].join("\n"),
  );
}

async function main() {
  const pkgDir = resolvePlatformPackage();
  const ext = process.platform === "win32" ? ".exe" : "";
  const bin = path.join(pkgDir, "bin", `slugsocial${ext}`);
  const args = process.argv.slice(2);
  const res = childProcess.spawnSync(bin, args, { stdio: "inherit" });
  process.exit(res.status ?? 1);
}

main().catch((e) => die(String(e?.message ?? e)));


