# Agent notes: running and testing this repo

This repository is **Slug Social** (Rust workspace + Clojure/Babashka integration tests). There is no top-level `README.md`; design and product context live in `plan.md` / `plan2.md` and `ideas/`.

## What ships here

| Piece | Path / crate | Role |
|--------|----------------|------|
| HTTP server | `server/` (`slugsocial-server`) | Axum app, event-sourced JSONL log, HTML + API |
| CLI | `cli/` (`slugsocial`) | Talks to the server over HTTP (`SLUG_SERVER`, default production URL) |
| Shared types | `types/` (`slug-types`) | Rust types used by server and CLI |
| Integration harness | `test/*.clj`, `deps.edn` | End-to-end checks (mock OAuth, grants, invites, browser SSE via Playwright) |
| Dev tasks | `bb.edn` | `bb dev`, `bb watch`, `bb test`, release helpers |

**Rust toolchain:** `rust-toolchain.toml` pins **1.88.0** (required; e.g. `time` needs edition 2024 support). **mise** (`mise.toml`) documents Rust **1.88.0** and Babashka **1.12.217** for local parity.

## Prerequisites

- **Rust** 1.88+ (use `rustup` and the pinned toolchain, or `mise install`).
- **Java** (JVM) for Clojure CLI.
- **Clojure CLI** (`clojure` / `clj` with `clojure -M` support).  
  **Important:** On Debian/Ubuntu, the distro package named `clojure` is often the *language* launcher, not the official **Clojure CLI**; `clojure -M` will fail. Install from [Clojure’s install instructions](https://clojure.org/guides/install_clojure) (e.g. the `linux-install.sh` release from the `clojure/brew-install` repo) so `/usr/local/bin/clojure` takes precedence over `/usr/bin/clojure`.
- **Babashka** (`bb`) for `bb.edn` tasks (`mise` documents a version).
- **OpenSSL dev headers** (e.g. `libssl-dev`) if you build in minimal Docker/CI images; the `Dockerfile` installs them for release builds.

First Clojure test run downloads Maven/Clojars dependencies and Playwright-related JARs; allow network access once.

## Build

```bash
cargo build --workspace
```

Release binary for deployment:

```bash
cargo build --release -p slugsocial-server
```

## Run the server locally

Defaults (from `server/src/main.rs`): **`SLUG_DATA_DIR=/data`**, **`SLUG_EVENT_LOG=$SLUG_DATA_DIR/events.jsonl`**, **`PORT=8080`**. For local dev, override `SLUG_DATA_DIR` to a writable directory (repo root `dev-data/` is used by `bb dev`).

```bash
mkdir -p dev-data
SLUG_DATA_DIR=./dev-data PORT=8080 cargo run -p slugsocial-server
```

Smoke check:

```bash
curl -s http://127.0.0.1:8080/healthz   # expect: ok
```

**OAuth / Google:** `server/src/api/auth.rs` reads `SLUG_PUBLIC_URL` (default `http://127.0.0.1:8080`), `SLUG_GOOGLE_CLIENT_ID`, `SLUG_GOOGLE_CLIENT_SECRET`, and optional `SLUG_GOOGLE_AUTH_URL` / `SLUG_GOOGLE_TOKEN_URL`. Defaults of `dev` let you bring the server up without real credentials; real login needs proper Google OAuth app settings and public URL.

**Note:** `bb dev` / `bb watch` set `SLUG_KEYS=dev:dev`; there is **no** `SLUG_KEYS` reference in the Rust server sources today—treat that as harmless for local scripts or legacy; integration tests use `SLUG_KEYS=test:test` similarly.

## CLI against a local server

```bash
SLUG_SERVER=http://127.0.0.1:8080 cargo run -p slugsocial -- --help
```

Published clients also live under `packages/npm` and `packages/pypi` (see those READMEs for packaging).

## Tests

**Rust (fast, required for routine work):**

```bash
cargo test --all
```

**Full repo script (Rust + Clojure integration):**

```bash
./TEST.sh
# same as:
#   cargo test --all
#   ./scripts/clj-test.sh
```

`./scripts/clj-test.sh` runs `clojure -M -m test.runner` and `clojure -M -m test.runner browser-sse` (Playwright-backed browser check). Ensure the official Clojure CLI is on `PATH` as described above.

**Babashka orchestration** (subset overlapping integration; see `bb.edn`):

```bash
bb test
```

## Docker

The root `Dockerfile` builds `slugsocial-server` and runs it with `SLUG_DATA_DIR=/data`, `SLUG_EVENT_LOG=/data/events.jsonl`, `PORT=8080`. Mount a volume on `/data` for persistence.

## Verification in this environment (2026-04-10)

- `cargo build --workspace` succeeded.
- `cargo test --all` succeeded (server unit + integration tests).
- After installing the **official** Clojure CLI, `./scripts/clj-test.sh` completed successfully (integration, auth v3, grants, invites, browser SSE).

## Pointers for code changes

- App wiring: `server/src/lib.rs` (`create_app`), routes and API under `server/src/api/`.
- Event log: `server/src/event_log.rs`; reducer: `server/src/reducer.rs`.
- Access control direction: `plan.md` (OAuth-only users, private threads, grants, invites).
