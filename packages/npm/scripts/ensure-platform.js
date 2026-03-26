"use strict";
/**
 * npm/npx sometimes skip optionalDependencies (omit=optional) or lifecycle scripts
 * (npm exec). We run this from postinstall when possible, and from the bin shim
 * before spawning the native binary so `npx slugsocial` always works.
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

/**
 * @param {string} pkgRoot absolute path to slugsocial package root (…/node_modules/slugsocial)
 * @returns {boolean} true if the platform package is available after any install attempt
 */
function ensurePlatformInstalled(pkgRoot) {
  const name = platformPkgName();
  if (!name) {
    return false;
  }
  try {
    require.resolve(`${name}/package.json`, { paths: [pkgRoot] });
    return true;
  } catch (_) {
    /* continue */
  }

  const pkgJsonPath = path.join(pkgRoot, "package.json");
  if (!fs.existsSync(pkgJsonPath)) {
    return false;
  }
  const pkg = JSON.parse(fs.readFileSync(pkgJsonPath, "utf8"));
  const version = pkg.version;
  if (!version) {
    return false;
  }

  const nm = path.join(pkgRoot, "..");
  const installRoot = path.dirname(nm);
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const args = [
    "install",
    "--no-fund",
    "--no-audit",
    "--no-package-lock",
    "--prefer-online",
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
    return false;
  }
  try {
    require.resolve(`${name}/package.json`, { paths: [pkgRoot] });
    return true;
  } catch (_) {
    return false;
  }
}

function main() {
  const pkgRoot = path.join(__dirname, "..");
  if (!ensurePlatformInstalled(pkgRoot)) {
    const name = platformPkgName();
    const pkg = JSON.parse(
      fs.readFileSync(path.join(pkgRoot, "package.json"), "utf8"),
    );
    console.warn(
      `slugsocial: postinstall could not install ${name}@${pkg.version}. ` +
        `The CLI will retry on first run.`,
    );
  }
}

if (require.main === module) {
  main();
}

module.exports = { ensurePlatformInstalled, platformPkgName };
