"use strict";
/**
 * npm/npx sometimes skip optionalDependencies (e.g. omit=optional, or exec cache).
 * This postinstall pulls the correct @sortersocial/slugsocial-* for this OS/arch
 * into the same node_modules tree as slugsocial.
 */
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");

function platformPkgName() {
  const p = process.platform;
  const a = process.arch;
  const plat =
    p === "darwin" ? "darwin" : p === "linux" ? "linux" : null;
  const arch = a === "x64" ? "x64" : a === "arm64" ? "arm64" : null;
  if (!plat || !arch) {
    return null;
  }
  return `@sortersocial/slugsocial-${plat}-${arch}`;
}

function main() {
  const pkgRoot = path.join(__dirname, "..");
  const pkgJsonPath = path.join(pkgRoot, "package.json");
  const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, "utf8"));
  const name = platformPkgName();
  if (!name) {
    return;
  }
  try {
    require.resolve(`${name}/package.json`, { paths: [pkgRoot] });
    return;
  } catch (_) {
    /* fall through */
  }

  const version = pkg.version;
  if (!version) {
    return;
  }

  const nm = path.join(pkgRoot, "..");
  // Parent of node_modules (works for npx cache and normal installs).
  const installRoot = path.dirname(nm);
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const args = [
    "install",
    "--no-fund",
    "--no-audit",
    "--no-package-lock",
    "--include=optional",
    "--prefix",
    installRoot,
    `${name}@${version}`,
  ];
  const r = spawnSync(npm, args, {
    stdio: "inherit",
    env: process.env,
  });
  if (r.status !== 0) {
    console.warn(
      `slugsocial: postinstall could not install ${name}@${version} (exit ${r.status}). ` +
        `Try: npm install ${name}@${version}`,
    );
  }
}

main();
