# Releasing `slugsocial` (Rust CLI) for `npx`/`uvx` shims

The `packages/npm` and `packages/pypi` packages are **thin shims**. They do **not** implement CLI parsing; they download and exec the Rust `slugsocial` binary.

## Environment variables (shim)

- `SLUGSOCIAL_BIN`: absolute path to an existing `slugsocial` binary (skips download)
- `SLUGSOCIAL_RELEASE_BASE`: GitHub Releases download base, e.g. `https://github.com/<org>/<repo>/releases/download`
- `SLUGSOCIAL_TAG`: release tag (defaults vary by shim; recommended: `vX.Y.Z`)

## Required GitHub release assets

For each release tag `vX.Y.Z`, publish **one file per platform/arch**:

- `slugsocial-darwin-x64`
- `slugsocial-darwin-arm64`
- `slugsocial-linux-x64`
- `slugsocial-linux-arm64`
- `slugsocial-windows-x64.exe`
- `slugsocial-windows-arm64.exe`

## Build commands (examples)

Build locally:

```bash
cargo build --release -p slugsocial
```

Cross compile is up to you (GitHub Actions recommended).

## Shim versioning

- `packages/npm/package.json` version should match the Rust CLI release tag (`vX.Y.Z`).
- `packages/pypi/pyproject.toml` version should match as well.


