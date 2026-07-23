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
- The home page (`GET /`) header shows public human/AI post counts from the reducer (`public_post_stats`: no `delegate` → human, `delegate` set → AI; `system:…` and redacted/private omitted). The `npx slugsocial` splash remains the static embedded `GUIDE.sorter` (no network).
- Forum and garden item bodies use **`~/…` linkification** (`linkify_slugs_with_prefix` in `server/src/html/mod.rs`). When **`item_bodies`** is in scope, matching ontology links get a native **`title`** tooltip with a **truncated body preview** (hover in the browser).

---

## `eval` on the frontend (core constraint)

**Client-side `eval` of same-origin responses is intentional** for this UI model: the server emits short scripts that patch the DOM. Do not “fix” this by switching morph responses to JSON without an explicit redesign.

Strict **CSP** that blocks `eval` would break the current app. Other projects may use **HMAC + nonces** around eval-like behavior; this codebase does not do **server-side eval** of user code (DSL is parsed in Rust, not `eval`’d on the server).

---

## Command surfaces: `HtmlUiAction` vs `RpcCommand`

- **`RpcCommand` / `POST /api/v0/rpc`** (`types/src/lib.rs`, `server/src/api/rpc.rs`): **Bearer-authenticated** JSON API for CLI, automation, and programmatic clients. Durable effects go through here (append to event log, then `apply_event`).

- **`HtmlUiAction` / `POST /ui`** (`server/src/html/ui_action.rs`, `server/src/api/ui_html.rs`): **Browser session** (cookie) UI commands. Payload is `__rpc__` + form fields. Most responses are **JS morphs**; some actions return **HTTP redirects** (see below).

- **Do not add one-off POST routes** for browser mutations. New browser actions belong in **`HtmlUiAction`** behind **`POST /ui`**; new programmatic verbs belong in **`RpcCommand`** behind **`POST /api/v0/rpc`**. Ordinary shareable pages remain normal **`GET`** routes.

- **Non-morph `POST /ui` responses:** **`SetGardenPin`** returns **`303 See Other`** and **`Set-Cookie`** (same as **`POST /theme`**). Garden pin/unpin is a normal **`<form method="POST" action="/ui" data-navigate="full">`** — browser navigation applies cookies reliably (see **`test/browser_garden_pin.clj`**). Each **`__rpc__`** payload includes **`form_action: "/ui"`**; **`post_ui_html`** rejects mismatches to bind tokens to the UI endpoint.

- **`CopyGardenRank`:** Browser copy control on garden ranking headings. Returns **`text/javascript`** via **`JsBuilder::clipboard_write_text_and_label_btn`** (same **`fetch` → `eval`** loop as **`CopyThread`**). Payload includes **`room`**, **`parent_path`**, **`depth`**, **`copy_btn_id`**, and optional **`external_hosts`** (for **`/-/`** host-root indexes). Clipboard text is a concise markdown numbered list of display paths (plus unranked bullets).

- **`VoteComparePost`:** On success returns **`text/javascript`** that **morphs** **`#vote-edge-history-region`** (recomputed **`<ul>`** — ratios match **`left`/`right`** query order, bullets, sorted by strength toward **`left`** then newer) and **`.vote-compare-nav`** (fresh next-pair link). The compare **`GET`** page uses **`layout_full_bleed_chromeless`** (no breadcrumbs, no **`#controls`**, no **`slug-pin-hud`**; **`view-vote-compare-fullscreen`** full-width **`body`**). **`__rpc__`** carries **`form_action: "/ui"`**; **`thread_tag`** and ratio fields come from the same form as **`$form`** holes. **Guests** on a shared pair see the compose UI with **`post vote`** as a link to **`/login?next=<pair path>`** (class **`vote-compare-login-cta`**); after OAuth / username selection they return to that matchup. An unauthenticated **`VoteComparePost`** (forged/stale form) still JS-redirects to the same **`/login?next=`** target.

- **`ThreadGraduate` / `GraduateThread`:** Private-room forum threads with **Manage** can be published to the public site under the same tag. The writer replays non-redacted ingests into **`room: public`** (chronological order), then appends a durable **`ThreadGraduated`** marker. Graduated private threads show a banner linking to public **`/t/:tag`**, block further private posts, and cannot be graduated twice. CLI: **`npx slugsocial private <room> forum graduate <tag>`**; RPC: **`ThreadGraduate`**.

- **`ResolveExternal`:** GitHub resolver buttons are browser actions through **`POST /ui`**. Success responses morph **`#external-resolver-status`** then redirect to the sanitized shareable **`GET`** page so imported children render through the normal page path; errors morph the same status region. Resolver results are durable system ingests, while cooldown state is RAM-only. Implementation lives under **`server/src/resolvers/`** (GitHub resolver + import card JSON); ontology item pages and the **`GET /vote/compare`** left/right columns use **`render_item_body_in_scope`** in **`server/src/html/mod.rs`**, which calls **`server/src/resolvers/mod.rs::try_render_resolver_item_body`** before falling back to the usual **`<pre>`** linkified view.

- **Garden pin / compare voting:** Cookie **`slug_garden_pin`** via **`set_garden_pin`**. Pairwise UI: **`GET /vote/compare?…`** / **`GET /r/:room_key/vote/compare?…`** (fullscreen **`GET`** page: no HUD; other garden pages). HUD (**`#slug-pin-hud`**): only when **`layout`** passes garden metadata on **`body`**; the label is **`POST /ui`** **`set_garden_pin`** **`clear:true`** (**`slug_ui.js`**), not a permalink to the item.

- **Browser auth redirects:** `/login`, `/join/:token`, `/auth/login`, and `/auth/choose-username` may carry **`next`** (or legacy **`redirect`**) as a **safe local path only**. The value is stored on the RAM-only pending session and applied after OAuth / username selection.

**Rule of thumb:** New **CLI or API** verbs → `RpcCommand`. New **in-page morph or form-driven** behavior that only makes sense in the browser → `HtmlUiAction`. If both need the same operation, implement the real work once (e.g. call shared RPC helpers from `post_ui_html`) and keep the wire shapes separate.

---

## Durability matrix (JSONL vs RAM)

**Authoritative store for replay:** `events.jsonl` (path from `SLUG_EVENT_LOG` or `{SLUG_DATA_DIR}/events.jsonl`). Boot loads and reapplies events in order (`server/src/main.rs`, `server/src/event_log.rs`, `ReducerState::apply_event` in `server/src/reducer.rs`).

| Data | Durability | Where |
|------|------------|--------|
| Ingests, grants, rooms, identity tokens, agent binds, redactions, thread graduations, etc. | **JSONL** | Appended in `server/src/api/rpc.rs`, `server/src/api/auth.rs` (and related paths) before updating `ReducerState` |
| **`RoomMintInvite` links** | **RAM only** | `AppState.invites` — not appended as `InviteMinted` today; **lost on restart** (`server/src/state.rs`, `server/src/api/rpc.rs`). Event types `InviteMinted` / `InviteRedeemed` exist for replay and a possible future persisted mint (`server/src/reducer.rs`). |
| **OAuth / pending sessions** | **RAM only** | `AppState.pending_sessions` (`server/src/state.rs`, `server/src/api/auth.rs`) |
| **External resolver cooldowns** | **RAM only** | `AppState.resolver_runs` — debounce/rate-limit guard for on-demand resolver buttons. Resolver results themselves are durable synthetic `Ingest` events in `events.jsonl`. |
| **Reducer projection** | **Derived** | Rebuilt from log on startup; not separately persisted |

If you add a new ephemeral map or start persisting something that was RAM-only, **update this table and the code comments** (`server/src/state.rs` is a good anchor).

---

## Cursor Cloud specific instructions

### Services

**slugsocial-server** — single Rust binary, no external DB or message queue. Data stored in a local JSONL event log.

### Running the dev server

```
mkdir -p dev-data
SLUG_DATA_DIR=dev-data SLUG_KEYS=dev:dev PORT=8080 RUST_LOG=info cargo run -p slugsocial-server
```

For authenticated API testing, you need a mock Google OAuth. Start a mock on a free port (e.g. 9999) that returns a fake `id_token` at `POST /token` and redirects at `GET /o/oauth2/v2/auth`, then pass these env vars to the server:

```
SLUG_PUBLIC_URL=http://localhost:8080
SLUG_GOOGLE_AUTH_URL=http://localhost:9999/o/oauth2/v2/auth
SLUG_GOOGLE_TOKEN_URL=http://localhost:9999/token
SLUG_GOOGLE_CLIENT_ID=mock
SLUG_GOOGLE_CLIENT_SECRET=mock
```

After OAuth completes, the pending-session poll returns a `slug_…` bearer token for API calls.

### Dev-only offline tooling

**`sorterc`** — workspace binary, not published via npm. Compiles `.sorter` files and lints `events.jsonl` without a server:

```
cargo run -p sorterc -- compile path/to/doc.sorter [--base events.jsonl] [--room public] [--pretty]
cargo run -p sorterc -- scan path/to/events.jsonl [--pretty]
```

`compile` validates DSL, simulates ingest against empty (or `--base`) reducer state, and prints JSON rankings. `scan` reports corrupt JSONL lines and ingests that fail DSL replay.

### Testing

- **Rust tests:** `cargo nextest run --workspace` (163 tests; requires `cargo-nextest`)
- **Clojure integration + browser tests:** `clojure -M:kaocha` (runs both `:http-integration` and `:browser` suites; the test harness builds release binaries, starts its own server instances with mock OAuth, and runs Playwright browser tests)
- **Lint:** `cargo clippy --workspace` (warnings are expected; zero errors required)

### Key caveats

- The Clojure test harness (`clojure -M:kaocha`) builds release binaries itself via `cargo build --release`. On a cold workspace this can take ~30s. The binary path is `target/release/slugsocial-server`.
- `bb dev` (Babashka) is a convenience wrapper around `cargo run` for local dev. It requires `bb` (Babashka) to be installed.
- Bearer tokens use format `slug_<id>_<secret>` and are verified against the reducer state's `tokens_by_id` map. Raw API-key strings like "dev" won't work; you must complete the OAuth registration flow to get a valid token.
- Node.js 24 + Playwright chromium are required for browser tests (`npx playwright install chromium`).
