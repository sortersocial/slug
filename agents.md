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
- The home page (`GET /`) is the public **thread index**: private rooms when signed in, bump-ordered public threads, new-thread slot, `public_post_stats` header (no `delegate` → human, `delegate` set → AI; `system:…` and redacted/private omitted). Links to **`/~`** (garden). The `npx slugsocial` splash remains the static embedded `GUIDE.sorter` (no network).
- The old thread-index path **`GET /t`** 308s to **`/`** (same for `/t/`). `/t/:tag` thread pages are unchanged. Live feed SSE prefixes for the public bump list are `/` and `/t/:tag`. Brand / home / forum-index links point at `/`.
- Forum and garden item bodies use **`~/…` linkification** (`linkify_slugs_with_prefix` in `server/src/html/mod.rs`). Tilde refs emit **leaf hrefs and leaf display** (`~/x/luke` → `/~/luke` and `~/luke`). When **`item_bodies`** is in scope, matching ontology links get a native **`title`** tooltip with a **truncated body preview** (hover in the browser).

- **Thread pagination and the SSE push path are page-scoped.** Thread pages (`/t/:tag?offset=N`) are **fixed windows aligned to `PAGE_SIZE` boundaries** (`server/src/html/forum/paginator.rs`): the latest page grows by appending until full, so existing posts never shift; arbitrary `?offset=` values snap to the containing page. Live pushes after a post/redact/graduate (`broadcast_web_refresh` in `server/src/api/write_actor.rs` → `thread_region_page_morphs` in `server/src/html/forum/feed.rs`) morph `#thread-feed-region` only behind **client-side page-offset guards** (`JsBuilder::if_page_offset_*`): the latest page, the page before it (its paginator gains the live `newer →` link at rollover), and — for redactions — the page containing the changed post. Viewers reading older pages are never overwritten with the latest posts. The poster's own `POST /ui` response (`post_success_response` in `server/src/api/ui_html.rs`) morphs in place only on the latest page and otherwise redirects to it.

---

## `eval` on the frontend (core constraint)

**Client-side `eval` of same-origin responses is intentional** for this UI model: the server emits short scripts that patch the DOM. Do not “fix” this by switching morph responses to JSON without an explicit redesign.

Strict **CSP** that blocks `eval` would break the current app. Other projects may use **HMAC + nonces** around eval-like behavior; this codebase does not do **server-side eval** of user code (DSL is parsed in Rust, not `eval`’d on the server).

---

## Command surfaces: `HtmlUiAction` vs `RpcCommand` vs MCP

- **`RpcCommand` / `POST /api/v0/rpc`** (`types/src/lib.rs`, `server/src/api/rpc.rs`): **Bearer-authenticated** JSON API for CLI, automation, and programmatic clients. Durable effects go through here (append to event log, then `apply_event`).

- **ChatGPT / Codex MCP (`POST /mcp`)** (`server/src/mcp/`): Streamable-HTTP JSON-RPC for the Plugins Directory. Tools call `dispatch_rpc` (same handlers as `POST /api/v0/rpc`). Public reads are `noauth` (and also accept a bearer). Private-room reads and all writes require `Authorization: Bearer slug_…` (issued as the OAuth access token). `post_sorter` **requires** `delegate` (`uuid:rig:provider/model`); the writer binds that delegate to the linked human and rejects other humans. `create_room` only creates private rooms (`visibility` must be `"private"`) and can grant `members`. Read results for posts expose `actor` and `delegate`. Authenticated private-room reads are first-class MCP tools: `list_rooms`, `read_room`, and `get_feed` use `oauth2` + `slug.read`. `get_feed` accepts the same `delegate` as CLI `feed` (server-side cutoff = that delegate's last ingest). `get_matchup` is CLI `garden matchup`. `health` is `GET /healthz` (liveness). Garden `item_path` / `parent_path` queries accept a bare leaf (`ship-sets` = `~ship-sets`). Private `room_id` accepts `shortid/slug` or the bare 7-char shortid. `identity_start` / `identity_poll` mint a conversation delegate (linked callers get it immediately; unlinked callers get a Google URL). Dual-mode garden/forum reads list `slug.read` before `noauth` so linked clients send the bearer. OAuth 2.1 + PKCE lives at `/oauth/authorize` + `/oauth/token` and reuses Google login. Redirect allowlist is ChatGPT (`chatgpt.com` / `chat.openai.com`), Claude (`claude.ai`, including `https://claude.ai/api/mcp/auth_callback`), and loopback HTTP. Well-known docs are `/.well-known/oauth-protected-resource` and `/.well-known/oauth-authorization-server`. Domain verification token is `GET /.well-known/openai-apps-challenge` from `SLUG_OPENAI_APPS_CHALLENGE`. Do not add a parallel write path. Do not reuse `POST /ui` + `eval` morph as a ChatGPT widget. The website/RPC still allow human posts with no delegate; only the MCP tool requires it.

- **Forum thread tags on write:** `validate_thread_tag` (`types/src/paths.rs`) canonicalizes then rejects empty tags and any tag containing `/` (would break `/t/:tag` routing). The new-thread form already constrains the charset client-side (`pattern="[a-z0-9_\\-]{1,64}"`); server write paths (`WriteCmd::Post` / `SystemIngest`, and UI `PostIngest` / `CheckIngest` / `VoteComparePost`) enforce the slash rule. Read/replay still uses `canonicalize_tag` only so historical tags keep resolving. System import tags use `:` instead of `/` (e.g. `import:https:::github.com:org:repo`).

- **`HtmlUiAction` / `POST /ui`** (`server/src/html/ui_action.rs`, `server/src/api/ui_html.rs`): **Browser session** (cookie) UI commands. Payload is `__rpc__` + form fields. Most responses are **JS morphs**; some actions return **HTTP redirects** (see below).

- **Do not add one-off POST routes** for browser mutations. New browser actions belong in **`HtmlUiAction`** behind **`POST /ui`**; new programmatic verbs belong in **`RpcCommand`** behind **`POST /api/v0/rpc`**. Ordinary shareable pages remain normal **`GET`** routes.

- **Non-morph `POST /ui` responses:** **`SetGardenPin`** returns **`303 See Other`** and **`Set-Cookie`** (same as **`POST /theme`**). Garden pin/unpin is a normal **`<form method="POST" action="/ui" data-navigate="full">`** — browser navigation applies cookies reliably (see **`test/browser_garden_pin.clj`**). Each **`__rpc__`** payload includes **`form_action: "/ui"`**; **`post_ui_html`** rejects mismatches to bind tokens to the UI endpoint.

- **`CopyGardenRank`:** Browser copy control on garden ranking headings. Returns **`text/javascript`** via **`JsBuilder::clipboard_write_text_and_label_btn`** (same **`fetch` → `eval`** loop as **`CopyThread`**). Payload includes **`room`**, **`parent_path`**, **`depth`**, **`copy_btn_id`**, and optional **`external_hosts`** (for **`/-/`** host-root indexes). Clipboard text is a concise markdown numbered list of **leaf** display paths (plus unranked bullets).

- **`VoteComparePost`:** On success returns **`text/javascript`** that **morphs** **`#vote-edge-history-region`** (recomputed **`<ul>`** — ratios match **`left`/`right`** query order, bullets, sorted by strength toward **`left`** then newer) and **`.vote-compare-nav`** (fresh next-pair link). The compare **`GET`** pages (`/vote`, `/r/:room_key/vote`) use **`layout_full_bleed_chromeless`** (no breadcrumbs, no **`#controls`**, no **`slug-pin-hud`**; **`view-vote-compare-fullscreen`** full-width **`body`**). **`__rpc__`** carries **`form_action: "/ui"`**; **`thread_tag`** and ratio fields come from the same form as **`$form`** holes. Optional **`aspect`** (same `$form` hole) prefixes the posted DSL with `:{slug}` so the vote lands in that aspect group; omitted / empty is the canonical ranking. **Guests** on a shared pair see the compose UI with **`post vote`** as a link to **`/login?next=<pair path>`** (class **`vote-compare-login-cta`**); after OAuth / username selection they return to that matchup. An unauthenticated **`VoteComparePost`** (forged/stale form) still JS-redirects to the same **`/login?next=`** target.

- **Retired question pages:** `/q/:collection`, `/q/:collection/:aspect`, and the room-scoped twins are gone entirely — no routes serve them, so they 404 (no question index on `/` either). Pairwise judging lives at `/vote?pool=` / `/vote?left=&right=`; aspect groups still exist in the DSL/CLI and render on garden scope pages.

- **`ThreadGraduate` / `GraduateThread`:** Private-room forum threads with **Manage** can be published to the public site under the same tag. The writer replays non-redacted ingests into **`room: public`** (chronological order), then appends a durable **`ThreadGraduated`** marker. Graduated private threads show a banner linking to public **`/t/:tag`**, block further private posts, and cannot be graduated twice. CLI: **`npx slugsocial private <room> forum graduate <tag>`**; RPC: **`ThreadGraduate`**.

- **`ResolveExternal`:** Resolver buttons (GitHub, are.na) are browser actions through **`POST /ui`**. Success responses morph **`#external-resolver-status`** then redirect to the sanitized shareable **`GET`** page so imported children render through the normal page path; errors morph the same status region. Resolver results are durable system ingests, while cooldown state is RAM-only. Implementation lives under **`server/src/resolvers/`** (per-domain resolver + import card JSON); **`mod.rs::resolve_external_children`** dispatches on URL host and **`try_render_resolver_item_body`** tries each card renderer before falling back to the usual **`<pre>`** linkified view (called from **`render_item_body_in_scope`** in **`server/src/html/mod.rs`**, used by ontology item pages and the **`GET /vote/compare`** left/right columns). The resolver panel (**`external_resolver_controls`** in **`server/src/html/garden/external.rs`**) renders per-host ("GitHub resolver" / "Are.na resolver"); only GitHub items get a siblings button (are.na block URLs are not path-children of their channel).
  - **Import thread:** one forum thread per GitHub repo (`import:https:::github.com:{owner}:{repo}`), shared by repo root / issues / pulls / commits / releases imports. Are.na: one thread per channel (`import:https:::www.are.na:channel:{slug}`) or per user profile (`import:https:::www.are.na:{user}`).
  - **Issues:** each open issue is its own **`system:github-resolver`** ingest in that repo import thread (so a later redact removes that issue from the garden). Refresh pages **all** open issues from the GitHub API (up to the page safety cap), keeps still-open single-issue posts, and **`SystemRedact`s** posts for closed/missing issues (and multi-issue bulk posts, which are re-imported as singles). The `slug-github-card` fence payload is **base64(JSON)** so markdown fences in issue bodies cannot terminate the DSL toggle-fence; decode yields the real excerpt string for rich/markdown rendering.
  - **Are.na channels and users:** each channel entry (block or nested channel) is its own **`system:arena-resolver`** ingest in the channel import thread, with the same refresh semantics as issues: page **`GET /v3/channels/{slug}/contents`** (`per=100`, page cap 25), keep still-connected posts, redact removed ones. User profile URLs (`https://www.are.na/:user`) import the user's channels via **`GET /v3/users/{user}/contents`** (non-channel entries skipped). Legacy **`/:user/:channel`** URLs name the same channel as **`/channel/:slug`** (slugs are globally unique): the resolver collapses them onto the canonical channel item (**`canonical_arena_item`**), and the panel's `item_storage` / `next` point at the canonical page. Block URLs (`/block/:id`) are **not** path-children of the channel, so membership is an **explicit containment claim** (`block <: channel` with a leading `{ … }` explanation) in the same post as the block's `slug-arena-card` body; blocks keep cross-channel identity (one item, multiple scopes). Validation requires containment sides to have bodies, so a ghost parent (channel or user, never pasted with a body) gets a one-time card definition ingest before child posts (**`ensure_scope_body`**). Cards carry `image_url` / `source_url` and render an `<img>` for image blocks. Env: **`SLUG_ARENA_API_BASE_URL`** (default `https://api.are.na`, v3), optional **`SLUG_ARENA_TOKEN`**, **`SLUG_ARENA_RESOLVER_COOLDOWN_MS`**. Card CSS classes are resolver-agnostic (`import-card__*`, shared with GitHub cards).
  - **External URL wire form:** Canonical garden paths are **`/-/https://host/…`**. Legacy **`/-/host/…`** and collapsed **`/-/https:/host/…`** permanently redirect to the canonical form. Action `next` fields always use the canonical path.
  - **Tilde garden URLs / leaf identity:** Leaf pages are canonical (`GET /~/luke` renders the item/scope page). Nested `GET /~/x/luke` permanently redirects (308, query preserved) to the leaf — same precedent as `/-/` legacy redirects (`OntologyPath::nested_redirect_target`). Trailing slashes on the garden roots and leaf pages also 308 to the slash-free form (`/~/` → `/~`, `/~/luke/` → `/~/luke`; same for `/-/` and `/r/:room_key/~/` / `/r/:room_key/-/`). Room-scoped `/r/:room/~/*path` follows the same rule (`/r/…/~/x/luke` → `/r/…/~/luke`). **`GET /~`** is the root-electorate index (via the root scope view). `~/x/luke` and `~/y/luke` are the same item `~luke`. Breadcrumbs walk **containment** (strongest parent weight), not the typed URL path; other active scopes are listed as alternates on the item page.
- **Garden pin / compare voting:** Cookie **`slug_garden_pin`** via **`set_garden_pin`**. Pairwise UI: **`GET /vote/compare?…`** / **`GET /r/:room_key/vote/compare?…`** (fullscreen **`GET`** page: no HUD; other garden pages). HUD (**`#slug-pin-hud`**): only when **`layout`** passes garden metadata on **`body`**; the label is **`POST /ui`** **`set_garden_pin`** **`clear:true`** (**`slug_ui.js`**), not a permalink to the item.

- **Aspect sub-scopes:** A `:slug` / `:slug {prompt}` / bare `:` statement is parse-derived from ingest text (no event-schema change, no new write path). Votes under an aspect go to a ranking group keyed `(scope_item, aspect)` where `scope_item` is a shared active-membership scope of both vote items; the canonical `ranking_group` and `rank_position_cache` stay aspect-free. Garden parent/item pages (`render_scope_view`, public `~/`) and the `/try` editor preview list aspect sections **below** the canonical ranking when that scope has aspect votes (heading `:{slug}`, optional prompt). No `?aspect=` routes or default-aspect redirects. CLI `garden children --aspect` / `garden rank --aspect` and MCP `get_rank.aspect` are opt-in reads via `GetGardenRank.aspect`. Prompts live on `ContentState.aspect_prompts` per `(room scope, slug)`. `parent_path` in those reads resolves to its leaf item (`~/x/luke` → `~luke`).

- **Garden page flow and edge anchors:** `~/` item pages read top → bottom as **parents (↑ `member of` card) → body → children (↓ ranked table)**. A blue **flow gradient** on the left edge descends parents (full blue) → hero (mid) → children (grey) — see the `GARDEN HIERARCHY` block in each of `theme_default.css`, `theme_retro_craft.css`, and `theme_retro.css`; garden hierarchy CSS (including the structural flex that keeps per-edge vote/§ links off parent titles) must exist in every theme that styles ontology pages. Every containment edge has a fragment URL: `#edge-<child>-in-<parent>` on the child's page (each parent row links its scope vote pool as the vote home, plus a `§` permalink), and `#item-<child>` on ranked/unranked rows of the parent scope page (`aspect-<slug>-` prefixed in aspect tables). Weight footnotes show totals only (`containment N · border M`); path sugar is not distinguished in the UI.

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
| **External resolver cooldowns** | **RAM only** | `AppState.resolver_runs` — debounce/rate-limit guard for on-demand resolver buttons (`SLUG_GITHUB_RESOLVER_COOLDOWN_MS` / `SLUG_ARENA_RESOLVER_COOLDOWN_MS`, default 15s). Resolver results themselves are durable synthetic `Ingest` / `PostRedacted` events in `events.jsonl`. |
| **Rank-position memo** | **RAM only / derived** | `ContentState.rank_position_cache` — generation-keyed global ranks, component-local Rank Centrality scores, and per-parent scope ranks. Reuses one ingest’s “after” ordering as the next ingest’s “before”. Scores come from each connected component’s solve (never a whole-graph mix of disconnected clusters). Rebuilt lazily during event replay; never persisted. Canonical-only: aspect groups are not in this memo. |
| **Aspect groups / prompts** | **Derived** | `ContentState.aspect_groups` keyed `(scope_item, slug)` and `aspect_prompts` keyed by slug (per room `ContentState`). Rebuilt from DSL on ingest replay; never persisted. Membership in an aspect group is the scope item's active-member electorate. |
| **Containment / borders / fallen-border journal** | **Derived / RAM** | `ContentState.containment` `(child, parent) → {explicit, sugar}`, `borders` `(child, parent) → weight`, `members_by_scope` / `scopes_by_member`, `fallen_border_journal`. Rebuilt from DSL on ingest replay; never persisted. Accessors: `members_of`, `scopes_of`, `border_state`, `fallen_borders`. Item pages render memberships, suspended borders, and this item's journal from these maps. |
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

- **Rust tests:** `cargo nextest run --workspace` (requires `cargo-nextest`)
- **Clojure integration + browser tests:** `clojure -M:kaocha` (runs both `:http-integration` and `:browser` suites; the test harness builds release binaries, starts its own server instances with mock OAuth, and runs Playwright browser tests)
- **Lint:** `cargo clippy --workspace` (warnings are expected; zero errors required)

### Key caveats

- The Clojure test harness (`clojure -M:kaocha`) builds release binaries itself via `cargo build --release`. On a cold workspace this can take ~30s. The binary path is `target/release/slugsocial-server`.
- `bb dev` (Babashka) is a convenience wrapper around `cargo run` for local dev. It requires `bb` (Babashka) to be installed.
- Bearer tokens use format `slug_<id>_<secret>` and are verified against the reducer state's `tokens_by_id` map. Raw API-key strings like "dev" won't work; you must complete the OAuth registration flow to get a valid token.
- Node.js 24 + Playwright chromium are required for browser tests (`npx playwright install chromium`).
