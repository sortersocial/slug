# Agent / contributor notes

This file is **maintainer-facing**. Keep it aligned with the code: when you change durability (log vs RAM), `POST /ui` behavior, or command surfaces, **update the relevant section here** so it does not drift.

---

## Browser UI contract

The web app is **not** a SPA with a JSON API for every interaction. Many mutations and UI updates use this loop:

1. **Request:** `POST /ui` with `Content-Type: application/x-www-form-urlencoded`. A hidden field **`__rpc__`** holds **compact JSON** describing the action (see `HtmlUiAction` in `server/src/html/ui_action.rs`). Optional `{"$form": "field"}` holes are filled from other form fields (`server/src/form_template.rs`).

2. **Response:** `Content-Type: text/javascript; charset=utf-8`. Body is executable JavaScript, usually calls to **`Idiomorph.morph`** on DOM nodes, built server-side by **`JsBuilder`** (`server/src/html/mod.rs`).

3. **Client:** After `fetch`, the response body is **`eval`’d** (global submit interceptor and several `onclick` handlers in `server/src/html/mod.rs`, `server/src/html/forum.rs`, editor in `server/src/html/editor.rs`). **Idiomorph** is loaded from the layout (`server/src/html/mod.rs`).

**Implications:**

- **End-to-end tests** that only assert HTTP status bodies miss DOM updates. Morph paths are covered by **Playwright / Spel** tests under `test/browser_*.clj` and `clojure -M -m test.runner …` (see `scripts/clj-test.sh`).
- Shareable URLs are normal GET routes (e.g. `/t/:tag`, thread post views). **Expand/collapse** and similar controls are **actions**, not bookmarkable GET endpoints; they use the same `POST /ui` + `__rpc__` pattern where applicable.

---

## `eval` on the frontend (core constraint)

**Client-side `eval` of same-origin responses is intentional** for this UI model: the server emits short scripts that patch the DOM. Do not “fix” this by switching morph responses to JSON without an explicit redesign.

Strict **CSP** that blocks `eval` would break the current app. Other projects may use **HMAC + nonces** around eval-like behavior; this codebase does not do **server-side eval** of user code (DSL is parsed in Rust, not `eval`’d on the server).

---

## Command surfaces: `HtmlUiAction` vs `RpcCommand`

- **`RpcCommand` / `POST /api/v0/rpc`** (`types/src/lib.rs`, `server/src/api/rpc.rs`): **Bearer-authenticated** JSON API for CLI, automation, and programmatic clients. Durable effects go through here (append to event log, then `apply_event`).

- **`HtmlUiAction` / `POST /ui`** (`server/src/html/ui_action.rs`, `server/src/api/ui_html.rs`): **Browser session** (cookie) UI commands. Payload is `__rpc__` + form fields. Responses are **JS morphs**, not JSON result objects. Used for posting from forms, checks, redact button, thread expand/collapse, new-thread slots, etc.

**Rule of thumb:** New **CLI or API** verbs → `RpcCommand`. New **in-page morph or form-driven** behavior that only makes sense in the browser → `HtmlUiAction`. If both need the same operation, implement the real work once (e.g. call shared RPC helpers from `post_ui_html`) and keep the wire shapes separate.

---

## Durability matrix (JSONL vs RAM)

**Authoritative store for replay:** `events.jsonl` (path from `SLUG_EVENT_LOG` or `{SLUG_DATA_DIR}/events.jsonl`). Boot loads and reapplies events in order (`server/src/main.rs`, `server/src/event_log.rs`, `ReducerState::apply_event` in `server/src/reducer.rs`).

| Data | Durability | Where |
|------|------------|--------|
| Ingests, grants, rooms, identity tokens, agent binds, redactions, etc. | **JSONL** | Appended in `server/src/api/rpc.rs`, `server/src/api/auth.rs` (and related paths) before updating `ReducerState` |
| **`RoomMintInvite` links** | **RAM only** | `AppState.invites` — not appended as `InviteMinted` today; **lost on restart** (`server/src/state.rs`, `server/src/api/rpc.rs`). Event types `InviteMinted` / `InviteRedeemed` exist for replay and a possible future persisted mint (`server/src/reducer.rs`). |
| **OAuth / pending sessions** | **RAM only** | `AppState.pending_sessions` (`server/src/state.rs`, `server/src/api/auth.rs`) |
| **Reducer projection** | **Derived** | Rebuilt from log on startup; not separately persisted |

If you add a new ephemeral map or start persisting something that was RAM-only, **update this table and the code comments** (`server/src/state.rs` is a good anchor).
