#!/usr/bin/env node
// Ultra-thin shim: exec the Rust `slugsocial` binary bundled via platform packages.
//
// npm will download exactly one optionalDependency for this platform:
// - @sortersocial/slugsocial-darwin-arm64
// - @sortersocial/slugsocial-darwin-x64
// - @sortersocial/slugsocial-linux-arm64
// - @sortersocial/slugsocial-linux-x64
// - @sortersocial/slugsocial-win32-arm64
// - @sortersocial/slugsocial-win32-x64

const path = require("node:path");
const childProcess = require("node:child_process");

function die(msg) {
  console.error(msg);
  process.exit(1);
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

function resolvePlatformPackage() {
  const { plat, arch } = platformTriple();
  const name = `@sortersocial/slugsocial-${plat}-${arch}`;
  try {
    const pkgJson = require.resolve(`${name}/package.json`);
    return path.dirname(pkgJson);
  } catch {
    die(
      [
        `missing platform package ${name}.`,
        `This usually means npm didn't install optionalDependencies for your platform.`,
        `Try: npm_config_optional=true npx slugsocial --help`,
      ].join("\n")
    );
  }
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


