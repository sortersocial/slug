# Releasing `slugsocial` (Rust CLI) for `npx`/`uvx`

Repo: `https://github.com/sortersocial/slug`

The `packages/npm` and `packages/pypi` packages are **thin shims**. They do **not** implement CLI parsing; they exec the Rust `slugsocial` binary that is bundled via:

- **npm optionalDependencies** (per-platform packages)
- **PyPI platform wheels** (binary included in wheel)

## Release artifacts you need to build

You still need one Rust binary per platform/arch:

- `darwin-arm64`
- `darwin-x64`
- `linux-arm64`
- `linux-x64`
- `win32-arm64`
- `win32-x64`

## Build commands (examples)

Build locally:

```bash
cargo build --release -p slugsocial
```

Cross compile is up to you (GitHub Actions recommended).

## Shim versioning

- `packages/npm/package.json` version should match the Rust CLI release tag (`vX.Y.Z`).
- `packages/pypi/pyproject.toml` version should match as well.


