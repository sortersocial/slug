# AGENTS.md

## Cursor Cloud specific instructions

### Overview

slug.social is an event-sourced social ranking platform (Rust/Axum server + Rust CLI). No external database — all state lives in an append-only JSONL file on disk.

### Required system dependencies

- **Rust 1.88+** — pinned by `time 0.3.47` (edition 2024). The VM image ships 1.83; the update script installs 1.88 via `rustup`.
- **pkg-config + libssl-dev** — needed for OpenSSL (reqwest crate).
- **Babashka (`bb`)** — Clojure task runner used for dev server, integration tests, and releases.

### Key commands

| Task | Command |
|---|---|
| Dev server (port 8080) | `bb dev` |
| Hot-reload dev server | `bb watch` (requires `cargo-watch`) |
| Unit + integration tests (Rust) | `cargo test --all` |
| End-to-end tests (Babashka) | `bb test` |
| Full test suite | `bash TEST.sh` (runs both above) |
| Release build | `cargo build --release --all` |
| Available bb tasks | `bb tasks` |

### Gotchas

- The `bb test` suite builds **release** binaries (`cargo build --release`) and spawns server + CLI processes from `target/release/`. A debug-only build is not enough for e2e tests.
- Auth requires OAuth. The dev server (`bb dev`) has no real Google OAuth configured. Tests use a mock Google OAuth server defined in `test/oauth.bb`. To post content via the API you need a bearer token from the OAuth flow — the `SLUG_KEYS=dev:dev` env var is passed to the server but auth still requires `slug_<id>_<secret>` tokens issued through OAuth.
- The `Check` RPC endpoint (`[{"Check":{...}}]`) does **not** require auth and can be used for dry-run ranking verification.
- The RPC batch endpoint (`POST /api/v0/rpc`) expects a JSON **array** (transparent `Vec<RpcCommand>`), not a single object.
- Server tests in `server/tests/tree_state_blob.rs` and `server/tests/tree_toggle_js.rs` have `return;` early-exit guards — they compile but skip their bodies intentionally.
