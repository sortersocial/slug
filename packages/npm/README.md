# `npx slugsocial`

Ultra-thin shim that **execs the Rust `slugsocial` binary bundled via platform packages**.

## Usage

Install/run:

```bash
npx slugsocial --help
```

## Publish checklist (npm)

- Publish **platform packages** first (one per OS/arch), each containing `bin/slugsocial` (or `.exe`).
- Then publish the root `slugsocial` package which has `optionalDependencies` on those platform packages.

Repo: `sortersocial/slug` (`https://github.com/sortersocial/slug`)

Packages to publish:
- `@sortersocial/slugsocial-darwin-arm64` (from `packages/npm/platforms/darwin-arm64`)
- `@sortersocial/slugsocial-darwin-x64`
- `@sortersocial/slugsocial-linux-arm64`
- `@sortersocial/slugsocial-linux-x64`
- `slugsocial` (from `packages/npm`)


