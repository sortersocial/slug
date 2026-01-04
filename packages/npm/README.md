# `npx slugsocial`

Ultra-thin shim that **downloads** (once) and **execs** the Rust `slugsocial` binary.

## Usage

```bash
# Option A: point at an existing local build
export SLUGSOCIAL_BIN="/path/to/slugsocial"

npx slugsocial healthz
```

```bash
# Option B: download from GitHub Releases (recommended)
export SLUGSOCIAL_RELEASE_BASE="https://github.com/<org>/<repo>/releases/download"
export SLUGSOCIAL_TAG="v0.0.1"

npx slugsocial --help
```

## Publish checklist (npm)

- Update `packages/npm/package.json` `repository.url`.
- Ensure `bin/slugsocial.js` is executable (`chmod +x`).
- Bump version.
- Publish from `packages/npm`:

```bash
npm publish --access public
```


