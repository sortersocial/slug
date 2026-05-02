# Plan: `ItemId` + `RouteContext` (identity vs hrefs)

This document is for **the next agent** to continue the refactor without re-deriving context from chat. It supersedes ad-hoc notes: treat it as the checklist of record until the work lands and this file is deleted or trimmed.

## Goal

- **Identity** (what lives in the reducer graph, votes, indexes) becomes a **structural `ItemId` enum** in `slug-types`, not a canonical `String` / `CanonicalItemUrl` newtype.
- **Presentation** (tilde / dash display, breadcrumbs) derives from `ItemId` via explicit methods, not string stripping.
- **Routing** (browser `href`s for public vs room) goes through **`RouteContext`** (started in `server/src/html/routing.rs`) so Maud/handlers do not stitch `/r/…` vs `/~` ad hoc.

**Non-goals for v1 of the migration:** backward-compatible JSONL or dual-read of old canonical strings in the event log (project has accepted breaking changes). If you reintroduce compat, document it here.

## Current state (as of this plan)

- **`CanonicalItemUrl`** (`types/src/paths.rs`): newtype around `String`; `parse` / `parent` / `display_path` / `tilde_tail` / etc. Reducer `ContentState`, `VoteData`, ranking, RPC, search, garden, breadcrumbs all use it or `String` keys derived from it.
- **`ThreadNav`** (`server/src/html/forum/nav.rs`): encodes scope prefixes for threads and garden URLs; **`RouteContext`** now wraps `ThreadNav` (`server/src/html/routing.rs`, re-exported from `server/src/html/mod.rs`) but **most HTML still takes `&ThreadNav` directly** — migration incomplete.
- **URL normalization** lives in `types/src/url_normalize.rs` + `canonicalize_item` / `finalize_external_identity_url` in `paths.rs` (YouTube, sorted query params, room path `room_route_segment` in `paths.rs`).
- **Room HTTP paths** are `/r/{short}{slug}` (fused segment); wire **`room_id`** remains `short/slug` for RPC/events.

## Target architecture

### `ItemId` (types)

Suggested shape (adjust after profiling `Ord` / `Hash` / serde size):

```text
ItemId::Root                      — tilde ontology root (today `SLUG_TILDE_ONTOLOGY_ROOT`)
ItemId::Local { segments }        — slug.social ~/… path as Vec<String> (lowercase segments, non-empty for non-root)
ItemId::External { url: Url }    — normalized `url::Url` (crate `url` already in `slug-types`)
```

**API surface (minimum):**

- `ItemId::parse(&str) -> Option<ItemId>` — single entry from DSL / user input / legacy wire (internally may call `canonicalize_item` + structured split).
- `ItemId::to_wire_url(&self) -> String` — only for **external** boundaries if needed (HTTP fetch, rare assertions); avoid using as the primary key once maps use `ItemId`.
- `parent`, `display_path`, `tilde_tail` / `tilde_http_tail`, `tilde_segments`, `last_segment`, `normalized_storage` — port from `CanonicalItemUrl`.
- **`Ord` + `Hash` + `Eq`** stable for `BTreeSet` / `HashMap` (see `write_actor` scope-rank snapshots).
- **`Serialize` / `Deserialize`** — decide **tagged JSON** for any persisted or API-carried structs (e.g. `VoteData` in tests). If RPC must stay stringy for clients, use a **DTO layer** that converts `ItemId` ↔ wire at the boundary only.

**Remove:** `CanonicalItemUrl` type and all `path_types::CanonicalItemUrl` / `slug_types::paths::CanonicalItemUrl` exports once call sites are migrated. **`Borrow<str>`** on the old newtype goes away; update `nav!` / any code that assumed map keys borrowed as `str`.

### `RouteContext` (server HTML)

- **File:** `server/src/html/routing.rs` — **`RouteContext(ThreadNav)`** with `item_href`, `item_href_raw`, `thread_url`, `garden_root_url`, `room_url`, `From`/`Into` `ThreadNav`.
- **Direction:** new code and refactored Maud should take **`&RouteContext`** (or owned where appropriate) instead of `&ThreadNav` when building links. Long term, **`item_href(&ItemId)`** should not parse strings — it should pattern-match `ItemId` and append tilde tail or `/-/…` external tail using the same rules as today’s `ThreadNav::garden_item_url`.

### Axum / garden routes

- **No** single catch-all route (explicit decision): keep the existing router layout in `server/src/lib.rs`.
- Room routes stay **`/r/:room_key/...`** with `room_key` fused; parsing via `slug_types::room_id_from_route_segment` / `room_route_segment` in `paths.rs`.

## Phased execution (recommended order)

### Phase 0 — Preconditions (quick)

1. Read **`AGENTS.md`** (UI contract, durability matrix, `RpcCommand` vs `HtmlUiAction`).
2. Run **`cargo test --workspace`** and **`./scripts/clj-test.sh`** on clean `main` before large diffs; repeat after each phase.

### Phase 1 — `ItemId` in `slug-types` (no server yet)

1. Add **`ItemId`** (new file e.g. `types/src/item_id.rs` **or** inline at bottom of `paths.rs` — see **Module cycle** below).
2. Implement **`ItemId::parse`** using existing **`canonicalize_item`** + normalization; port **`CanonicalItemUrl`** methods to **`ItemId`** with tests ported from `paths.rs` `#[cfg(test)] mod tests`.
3. **`GardenItemUrl::from_stored(&ItemId, room_wire)`** (and thread helpers) — build absolute hrefs from structure, not from re-parsing a canonical string.
4. **`TildeHttpPathTail::to_item_id`** (rename from `to_canonical`) / **`tilde_http_path_to_item_id`**.
5. **`TildeOntologyPath::from_stored(&ItemId)`**.
6. Export **`ItemId`** from **`types/src/lib.rs`**; update **`server/src/path_types.rs`** re-exports.
7. **Delete `CanonicalItemUrl`** and fix all **in-crate** references in `types` only until `cargo test` passes for `slug-types`.

**Module cycle trap:** `item_id.rs` must not `use crate::paths::{...}` if `paths.rs` also imports `ItemId` for `GardenItemUrl` in the same module. **Fix one of:**

- **A)** Put `ItemId` **inside `paths.rs`** below `canonicalize_item` / helpers (simplest, large file), or  
- **B)** Split **`canonicalize_item`** (+ dash host helpers + `finalize_external_identity_url`) into **`types/src/item_wire.rs`**, then `paths.rs` + `item_id.rs` both depend on `item_wire` only (cleaner, more files).

### Phase 2 — Reducer + ranking (server core)

1. **`server/src/reducer.rs`**: `ContentState` / `GroupState` / **`VoteData`** — replace **`CanonicalItemUrl`** with **`ItemId`** on all maps, sets, deques, vectors.
2. **`apply_vote`**: normalize `a`/`b` via **`ItemId::parse`** or **`ItemId`**-aware logic (remove string round-trip).
3. **`apply_ingest_to_content`**: **`dsl`** still yields strings for item titles in statements; normalize to **`ItemId`** at ingest boundary via **`ItemId::parse`** once per item.
4. **`server/src/ranking.rs`**, **`server/src/scope_rank.rs`**, **`server/src/api/write_actor.rs`** (including **`BTreeSet`** ordering), **`server/src/api/validate.rs`**, **`server/src/api/helpers.rs`** — propagate **`ItemId`**.
5. **`server/tests/basic.rs`** and any reducer tests constructing **`VoteData`** — use **`ItemId::parse(...).unwrap()`** or helpers.

### Phase 3 — RPC + search + external resolver

1. **`server/src/api/rpc.rs`**: rank/pair/matchup/search payloads; today many paths use **`GardenItemUrl::from_storage_str(item.as_str(), …)`** — switch to **`ItemId`** + **`GardenItemUrl::from_stored(&item_id, …)`** (or equivalent).
2. **`server/src/html/search.rs`**: scoring uses item path strings — derive from **`ItemId::display_path`** / **`to_wire_url`** only at the scoring boundary if needed.
3. **`server/src/external_resolver.rs`**: take **`&ItemId`** or **`ItemId::external_url()`** instead of **`&CanonicalItemUrl`**.

### Phase 4 — HTML / Maud

1. **`ThreadNav::garden_item_url`**: overload or replace with **`garden_item_href(&self, item: &ItemId)`** (no `CanonicalItemUrl::parse` inside).
2. **`RouteContext`**: extend **`item_href(&ItemId)`**; migrate call sites from **`ThreadNav`** to **`RouteContext`** where only link-building is needed (keep **`ThreadNav`** where scope / auth helpers need the full struct).
3. **`server/src/html/garden.rs`**, **`breadcrumb_path.rs`**, **`forum/*`**, **`editor.rs`**: replace **`CanonicalItemUrl`** with **`ItemId`**; breadcrumbs should walk **`ItemId::parent`** without string `rsplit`.
4. **`types` JSON types** (`RankRow`, etc.): decide whether **`GardenItemUrl`** stays string for JSON or becomes a structured field; keep **one** wire format for the public API.

### Phase 5 — Cleanup + docs

1. Remove dead **`canonical_path`** / **`breadcrumb_path`** string logic if fully superseded.
2. Update **`AGENTS.md`** if durability, `POST /ui`, or command surfaces change.
3. Delete or shrink **`plan.md`** when done.

## File / symbol checklist (non-exhaustive — grep-driven)

Run periodically:

```bash
rg "CanonicalItemUrl" -g'*.rs'
rg "path_types::CanonicalItemUrl" -g'*.rs'
rg "tilde_http_path_to_canonical" -g'*.rs'
```

**High-touch files (from prior exploration):**

| Area | Files |
|------|--------|
| Types | `types/src/paths.rs`, `types/src/lib.rs`, `types/src/url_normalize.rs`, (optional) `types/src/item_id.rs`, `types/src/item_wire.rs` |
| Server re-exports | `server/src/path_types.rs`, `server/src/canonical_path.rs` |
| Reducer / ingest | `server/src/reducer.rs`, `server/src/dsl.rs` (parse output types if changed) |
| Ranking | `server/src/ranking.rs`, `server/src/scope_rank.rs` |
| Writer / RPC | `server/src/api/write_actor.rs`, `server/src/api/rpc.rs`, `server/src/api/helpers.rs`, `server/src/api/validate.rs` |
| HTML | `server/src/html/garden.rs`, `server/src/html/breadcrumb_path.rs`, `server/src/html/forum/nav.rs`, `server/src/html/routing.rs`, `server/src/html/search.rs`, `server/src/html/editor.rs`, `server/src/html/forum/ingest.rs`, … |
| Tests | `server/tests/basic.rs`, `server/tests/integration.rs`, `types/src/paths.rs` tests, Clojure under `test/` if URLs/assertions mention canonical shapes |

## Events / JSONL

- **`Ingest`** events store **`raw` DSL** only — no change required for item identity inside the event.
- If any future event type stores item ids as strings, migrate to **structured `ItemId` serde** or accept string only at the event boundary with immediate parse into **`ItemId`** on `apply_event`.

## `nav!` macro (`server/src/paths.rs`)

- Macros use **`keypath($key)`** with **`.clone()`** — **`ItemId`** must be **`Clone`** (already for enums). Remove any reliance on **`Borrow<str>`** for map keys.

## Testing gate

After each phase:

```bash
cargo test --workspace
./scripts/clj-test.sh
```

## Risks / gotchas

1. **`Ord` on `ItemId`**: must match prior **`CanonicalItemUrl`** / `String` ordering wherever **`BTreeSet`** is used (e.g. deterministic scope-rank snapshots in **`write_actor`**).
2. **External `ItemId`**: **`Url`** equality / hashing — normalization is already centralized in **`url_normalize`**; ensure **`ItemId::parse`** always inserts normalized **`Url`** into **`External`**.
3. **Fake parent URLs** in garden (e.g. **`https://.`** for external root ranking): find all **`parse("https://.")`** style hacks and express as **`ItemId`** or a dedicated sentinel.
4. **Serde**: tests and any RPC clients that snapshot JSON may need expectation updates if **`VoteData`** shape changes.

## Optional follow-ups (not blocking `ItemId`)

- More **domain normalizers** in **`url_normalize.rs`** (e.g. `music.youtube.com`, Spotify, etc.).
- **Room wire** vs **HTTP segment** helpers already in **`paths.rs`** (`ROOM_SHORT_ID_LEN`, `room_route_segment`, `room_id_from_route_segment`).

---

**End state criteria:** `rg CanonicalItemUrl` returns nothing; reducer maps use **`ItemId`**; HTML link generation for items goes through **`RouteContext` + `ItemId`**; tests and Kaocha green.
