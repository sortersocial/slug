# Extensible URLs — Design Plan

*Addresses [#134](https://github.com/sortersocial/slug/issues/134)*

---

## Problem Statement

External URLs (`-/host/path`) already exist as first-class garden items: they have `ItemId::Web` identity, `parent()`/child edges, breadcrumbs, and rankings. But the system only knows about external items that a human manually declared via the DSL. There is no mechanism to:

1. **Automatically discover children** of an external URL (e.g. list the current issues under `github.com/org/repo/issues`).
2. **Navigate the URL hierarchy** the way you can navigate `~/` — clicking into `github.com/org` should show repos, clicking a repo should show its structure (issues, pulls, commits, etc.).
3. **Display the external page itself** when visiting a leaf item (currently shows "This is an external scope." with a disabled agent button).
4. **Keep external children in sync** as the upstream source changes (new issues opened, repos created, etc.).

The goal: URL structure becomes a votable/explorable ontology identical in UX to `~/`, with per-domain resolvers that can populate children automatically.

---

## Current Architecture (relevant subsystems)

### Item identity pipeline

```
User input ("~/a/b", "-/github.com/org/repo", "https://example.com/foo")
  → canonicalize_item()         [types/src/item_wire.rs]
      → normalize_http_identity_url()  [types/src/url_normalize.rs]
  → ItemId::parse()             [types/src/item_id.rs]
  → ItemId { Root | Local | Web | Opaque }
```

**Key properties of `ItemId::Web`:**
- Storage form is a normalized `https://…` string.
- `parent()` strips the last path segment (host-only items have no parent).
- `display_path()` renders as `-/host/path`.
- Parent→child edges are built by `add_child_edge` in the reducer when items are ingested.

### URL normalization (`url_normalize.rs`)

Currently handles:
- Query-pair sorting (lexicographic by lowercase key, then value).
- YouTube family: `youtu.be`, `/embed/`, `/v/`, `/shorts/`, `m.youtube.com` → canonical `www.youtube.com/watch?v=ID` or `www.youtube.com/shorts/ID`.
- `host_preserves_dash_path_case` — YouTube hosts keep case (video IDs are case-sensitive); all other hosts lowercase path segments.

### External resolver (`external_resolver.rs`)

A trait stub:
```rust
pub trait ExternalResolver: Send + Sync {
    fn domain_match(&self) -> &'static str;
    fn normalize(&self, path: &str) -> String;
    async fn fetch_body(&self, item: &ItemId) -> Result<String, String>;
}
```

Only `DefaultExternalResolver` exists (returns "external fetch not implemented").

### Garden rendering (`html/garden.rs`, `html/breadcrumb_path.rs`)

- `ExternalOntologyPath::from_input` parses `/-/*path` into segments for breadcrumbs.
- `render_scope_view` shows the item body (if any), children rankings, and vote history.
- When an external item has no body: shows "This is an external scope." + disabled "Kick off an Agent Run to import and rank items" button.
- Breadcrumbs split by `/` from the `https://` storage form — each segment is clickable.

### DSL parsing (`dsl.rs`)

Items can be referenced as:
- `~/path/segments` — slug ontology.
- `-/host/path/segments` — external items.
- `https://...` / `http://...` — full URLs.

The `-/` lexer accepts: alphanumeric, `_-/.?=&%:#+@~` — broad enough for query strings and fragments.

### Thread/ingest model

Items live in the garden; threads provide the temporal context. An `Ingest` event contains raw DSL text in a `thread_tag`. The DSL is parsed during `apply_ingest_to_content`, which creates items, registers bodies, records votes, and builds `item_children` edges.

---

## Design

### 1. Ingesting external URLs into threads

**Mechanism:** External URLs are already valid DSL item references. A user (or automated agent) can write:

```
-/github.com/sortersocial/slug/issues
  The issues list for the slug repo.
```

This already works today — it creates an `ItemId::Web("https://github.com/sortersocial/slug/issues")` with a body, registers parent edges up through `github.com/sortersocial/slug` → `github.com/sortersocial` → `github.com`, and makes it browsable at `/-/github.com/sortersocial/slug/issues`.

**What needs to change for auto-population:** When someone navigates to (or explicitly requests) an external scope, the system should be able to auto-populate its children. This is the job of domain-specific resolvers.

### 2. Domain resolver system

Extend the existing `ExternalResolver` trait into a **registry of domain resolvers**.

```rust
pub trait DomainResolver: Send + Sync {
    /// Host patterns this resolver handles (e.g. "github.com").
    fn matches_host(&self, host: &str) -> bool;

    /// Given a parent external URL, discover its direct children.
    /// Returns (child_url, title, optional_body) tuples.
    async fn list_children(&self, parent: &ItemId) -> Result<Vec<ResolvedChild>, ResolverError>;

    /// Fetch/compute a body for a single item (e.g. issue description, README excerpt).
    async fn fetch_body(&self, item: &ItemId) -> Result<String, ResolverError>;

    /// Domain-specific URL normalization beyond the generic pipeline.
    fn normalize(&self, url: &str) -> Option<String>;
}

pub struct ResolvedChild {
    pub url: String,      // canonical URL
    pub title: String,    // display title
    pub body: Option<String>,
}
```

**Registry:** `AppState` holds a `Vec<Arc<dyn DomainResolver>>`. On boot, register configured resolvers (initially just GitHub). Resolver lookup: find first where `matches_host(item_host)` returns true; fall back to `DefaultResolver` (which can still do generic things like fetching `<title>` tags).

**Synthetic ingests:** When a resolver returns children, the server creates synthetic `Ingest` events attributed to a system principal (e.g. `@system:resolver`). These go through the normal `write_actor` → JSONL → reducer pipeline so they are durable, replayable, and show up in thread feeds.

**Thread assignment:** Resolver-created items should land in a thread. Candidates:
- **Option A:** `#import/<host>` (e.g. `#import/github.com`) — groups all resolver activity by domain.
- **Option B:** `#import/<parent_path>` (e.g. `#import/github.com/org/repo/issues`) — groups by the scope that was resolved.
- **Recommendation: Option B.** It's more specific and gives users a thread to follow for a particular external scope. Thread tag format: `import:<display_path>` (e.g. `import:-/github.com/org/repo/issues`). The `:` separates the thread namespace from user-created `#` threads while reusing the same `canonicalize_tag` pipeline.

### 3. URL canonicalization

The existing pipeline (`canonicalize_item` → `normalize_http_identity_url` → `ItemId::parse`) is already solid. Extend it:

#### 3a. Current canonicalization rules (keep)
- Lowercase host.
- Lowercase path segments (except case-sensitive hosts like YouTube).
- Sort query pairs by lowercase key.
- YouTube: `youtu.be/ID` → `youtube.com/watch?v=ID`, `/embed/ID` → `/watch?v=ID`, etc.
- Strip default ports (80/443).
- Trim trailing slashes (host-only).

#### 3b. New canonicalization rules (add)

**Fragment stripping:**
- By default, strip `#fragment` from URLs used as item identity. Fragments identify within-page positions, not distinct resources. `github.com/org/repo/issues/42` and `github.com/org/repo/issues/42#issuecomment-123` should resolve to the same item.
- Exception: some sites use fragments as primary routing (e.g. single-page apps). Resolver-specific `normalize` can preserve fragments where the domain requires it.
- Implementation: add `strip_fragment(u: &mut Url)` call in `normalize_http_identity_url`, before `sort_query_pairs`.

**Scheme normalization:**
- Already handled: `http://` and `https://` both pass through `canonicalize_item`. However, `http://example.com` and `https://example.com` produce different `ItemId::Web` values.
- Policy decision: **prefer `https://`**. In `normalize_http_identity_url`, if scheme is `http`, upgrade to `https` (with an opt-out list for known http-only sites if needed).
- This is debatable. Alternative: leave scheme as-is, since some sites genuinely differ. Start with scheme-preserving and let resolver `normalize()` handle specific cases.

**Trailing-path slash normalization:**
- Currently `strip_redundant_root_slash` only handles host-only URLs. Extend to strip trailing `/` from all paths: `github.com/org/repo/` → `github.com/org/repo`.
- Already partially handled in `canonicalize_item` which trims trailing `/` from each segment during construction.

**`www.` stripping:**
- Currently only done for YouTube. Consider generalizing: `www.example.com` → `example.com` for identity purposes.
- Risk: some sites serve different content at `www.` vs bare domain. Start with YouTube only; add to resolver `normalize()` per domain.

#### 3c. Additional URL equivalences to handle

| Input form | Canonical form | Notes |
|---|---|---|
| `youtu.be/ID` | `https://www.youtube.com/watch?v=ID` | Already handled |
| `youtube.com/embed/ID` | `https://www.youtube.com/watch?v=ID` | Already handled |
| `youtube.com/shorts/ID` | `https://www.youtube.com/shorts/ID` | Already handled (kept as shorts) |
| `m.youtube.com/watch?v=ID` | `https://www.youtube.com/watch?v=ID` | Already handled |
| `github.com/ORG/REPO` | `https://github.com/org/repo` | Path lowercased (already handled by generic lowercasing) |
| `github.com/ORG/REPO.git` | `https://github.com/org/repo` | Strip `.git` suffix — add to GitHub resolver `normalize()` |
| `x.com/user/status/123` | `https://x.com/user/status/123` | Preserve as-is (or normalize `twitter.com` → `x.com`) |
| `twitter.com/user/status/123` | `https://x.com/user/status/123` | Add Twitter→X rewrite in `url_normalize.rs` |
| `reddit.com/r/sub/comments/id/…` | normalize to canonical Reddit URL | Reddit resolver |
| URL with tracking params (`utm_*`, `fbclid`, etc.) | Strip known tracking params | Generic rule in `normalize_http_identity_url` |

### 4. Query parameters

Query parameters are tricky because they serve multiple purposes:

**Identity-bearing:** `youtube.com/watch?v=ID` — the `v` param is the resource identity. Stripping it destroys the reference. Same for search queries, filter params on some sites.

**Tracking/noise:** `?utm_source=…`, `?fbclid=…`, `?ref=…` — these should be stripped for canonical identity.

**Pagination/state:** `?page=2`, `?sort=newest` — debatable. In the URL-as-ontology model, `github.com/org/repo/issues?page=2` probably shouldn't be a separate item from `github.com/org/repo/issues`.

**Proposed policy:**

1. **Generic stripping of known tracking params** in `normalize_http_identity_url`:
   ```
   utm_source, utm_medium, utm_campaign, utm_term, utm_content,
   fbclid, gclid, ref, ref_src, ref_cta, ref_loc,
   si (YouTube share tracking)
   ```

2. **Generic stripping of pagination params** (when not identity-bearing):
   ```
   page, per_page, offset, limit, cursor, after, before
   ```
   This is aggressive — resolver `normalize()` can re-add them if a domain treats pagination as identity.

3. **Preserve all other query params** and sort them (already done).

4. **Per-domain overrides** via `DomainResolver::normalize()`:
   - GitHub: strip `?tab=…` on repo pages (just UI state).
   - YouTube: preserve `v`, `list`; strip `t` (timestamp), `si`, `pp`, `feature`.
   - Let each resolver declare which params are identity-bearing vs noise.

### 5. Breadcrumbs for URL structures

**Current state:** `ExternalOntologyPath` splits the stored `https://host/path` into segments for breadcrumbs. Each segment links to `/-/host`, `/-/host/seg1`, `/-/host/seg1/seg2`, etc. This already works.

**What needs to change:**

#### 5a. Query-string segments in breadcrumbs
When an item has identity-bearing query params (e.g. `youtube.com/watch?v=ID`), the breadcrumb trail should show:
```
slug.social / - / youtube.com / watch?v=ID
```
Not:
```
slug.social / - / youtube.com / watch
```
Because `youtube.com/watch` alone is not a meaningful parent.

**Approach:** `ExternalOntologyPath::from_item` should check if the last path segment's parent would lose identity-bearing query params. If so, treat `path?query` as an atomic leaf segment in the breadcrumb. This is resolver-aware: the resolver declares which params are identity-bearing.

Alternatively, keep breadcrumbs purely path-based and accept that some intermediate breadcrumb segments (like `youtube.com/watch`) are "empty" scopes — they'd just show "no items" if navigated to, which is fine.

**Recommendation:** Keep breadcrumbs path-based. Accept that some intermediate nodes are empty. This is simpler and consistent with how file paths work (not every directory has content). The intermediate nodes become useful if someone later wants to rank all YouTube videos, all YouTube shorts, etc.

#### 5b. Display names for segments
Currently breadcrumb segments show the raw path component (`issues`, `pulls`, `42`). For items with resolver-fetched metadata:
- Show the item title alongside the path: `issues / #42: Fix null pointer` or `issues / 42` with a tooltip.
- Implementation: `ExternalOntologyPath` can accept an optional `titles: HashMap<ItemId, String>` from the reducer's `item_bodies` (first line or truncated).
- This is a stretch goal. Path-only breadcrumbs are fine initially.

### 6. Viewing external pages

When visiting an external item (e.g. `/-/github.com/org/repo/issues/42`):

**Option A: iframe embed.**
Show the external page in an iframe below the garden header (breadcrumbs + rankings + vote controls). Simple to implement but:
- Many sites block iframing (`X-Frame-Options: DENY`, CSP `frame-ancestors`).
- GitHub, Reddit, Twitter/X all block iframes.
- Works for: personal sites, docs sites, some wikis.

**Option B: Resolver-fetched body.**
The domain resolver fetches a summary/body for the item and stores it as the item body. E.g. for a GitHub issue, fetch the issue title + description via the GitHub API and store it as markdown. The garden renders it like any other item body.

**Option C: Hybrid.**
Try iframe first; if blocked (detectable client-side via `onload` error or CSP violation), show the resolver body or a link.

**Recommendation: Option B as primary, Option A as fallback.**
- Resolver-fetched bodies are more reliable and integrate better with the garden aesthetic.
- For items without a resolver, or when the resolver has no body, show a clickable link to the external URL + an iframe attempt with a fallback message.
- Implementation: `render_scope_view` already has the `external_empty_body` branch. Replace the disabled button with: (1) a link to the external URL, (2) the resolver body if available, (3) an iframe with `sandbox` attribute as a best-effort embed.

### 7. Keeping external items in sync

Resolver-discovered children go stale. New issues appear, repos are created/archived, etc.

**Approaches:**

**7a. On-demand refresh (recommended for v1):**
When a user navigates to an external scope page, check if the last resolver sync was >N minutes ago. If stale, trigger a background resolver fetch. The page renders immediately with cached data; new items appear on refresh or via SSE push.

State needed: `last_resolved_at: HashMap<ItemId, i64>` in `ContentState` or a separate `ResolverCache` in `AppState`. Not persisted in JSONL — derived from timestamps of resolver-attributed ingests.

**7b. Periodic background sync (v2):**
A background tokio task periodically re-resolves "subscribed" external scopes. Users or the system mark scopes for periodic sync. More complex, but needed for dashboards.

**7c. Webhook-driven sync (v3):**
For GitHub specifically, register webhooks to receive push notifications on issue/PR events. The webhook handler creates synthetic ingests. This is the most responsive but requires infrastructure (webhook endpoint, secret management, per-repo registration).

**Recommendation:** Start with 7a (on-demand). It's simple, requires no external infrastructure, and provides a good UX. Add 7b/7c later.

### 8. GitHub resolver (first implementation)

```rust
pub struct GitHubResolver {
    client: reqwest::Client,
    token: Option<String>,  // PAT for higher rate limits
}
```

**URL structure mapping:**

| URL pattern | Resolver action |
|---|---|
| `github.com` | List nothing (too broad); or list orgs the token can see |
| `github.com/{org}` | List repos for org (`GET /orgs/{org}/repos`) |
| `github.com/{org}/{repo}` | List structural children: `issues`, `pulls`, `commits`, `releases`, `wiki` (hardcoded) |
| `github.com/{org}/{repo}/issues` | List open issues (`GET /repos/{org}/{repo}/issues`) |
| `github.com/{org}/{repo}/issues/{n}` | Fetch issue body (`GET /repos/{org}/{repo}/issues/{n}`) |
| `github.com/{org}/{repo}/pulls` | List open PRs |
| `github.com/{org}/{repo}/pulls/{n}` | Fetch PR body |

**Normalization:**
- Strip `.git` suffix.
- Lowercase org/repo (GitHub is case-insensitive for these).
- Strip `?tab=…`, `?q=…` (search state, not identity).
- Preserve issue/PR numbers.

**Rate limiting:**
- Unauthenticated: 60 req/hr. Authenticated (PAT): 5000 req/hr.
- Cache resolver results in memory with TTL. On-demand refresh respects rate limits.
- Use conditional requests (`If-None-Match` / `ETag`) where GitHub supports them to avoid counting against rate limits.

### 9. DSL extensions (none required)

The existing DSL syntax already handles external URLs:
```
-/github.com/org/repo/issues/42
  This issue tracks the extensible URLs feature.

{-/github.com/org/repo/issues/42 -/github.com/org/repo/issues/99 4:1}
  Issue 42 is more important than issue 99.
```

No DSL changes needed. The resolver just automates what users can already do manually.

### 10. `?depth=N` parameter (from issue comment)

The issue mentions `GitHub.com?depth=1` to "rank all repos against each other." This means: flatten the hierarchy to depth N and show all descendants at that depth as rankable siblings.

**Semantics:**
- `/-/github.com/org?depth=1` → show all repos under `org` (same as `/-/github.com/org` default).
- `/-/github.com/org?depth=2` → show all repos AND their structural children (issues, pulls, etc.) as a flat list.
- `/-/github.com?depth=1` → show all orgs as siblings (rankable against each other).

**Implementation:**
- Add `depth` query param to `render_scope_view`.
- `build_children_rankings` currently only looks at direct children. Add a `depth: usize` parameter that recursively collects descendants to the specified depth, then ranks them as if they were all siblings.
- Default `depth=1` (current behavior).
- This feature is orthogonal to resolvers and can be implemented independently.

---

## Implementation Order

### Phase 1: Foundation
1. **Fragment stripping** in `normalize_http_identity_url`.
2. **Tracking-param stripping** (generic `utm_*` etc.) in `normalize_http_identity_url`.
3. **`http` → `https` upgrade** (optional, evaluate risk).
4. **External item display improvements**: replace disabled button with link to external URL.

### Phase 2: Resolver infrastructure
5. **`DomainResolver` trait** and resolver registry in `AppState`.
6. **Synthetic ingest pipeline**: resolver results → `Ingest` events via `write_actor`.
7. **On-demand resolution trigger**: when navigating to an external scope with stale or missing children, kick off a resolver fetch.
8. **Thread assignment**: resolver ingests land in `import:<display_path>` threads.

### Phase 3: GitHub resolver
9. **`GitHubResolver`** implementation with org/repo/issues/PRs.
10. **GitHub-specific normalization** (`.git` stripping, case, tab params).
11. **Rate limiting and caching**.

### Phase 4: UX refinements
12. **Breadcrumb title enrichment** from resolver metadata.
13. **`?depth=N`** flattened rankings.
14. **iframe fallback** for items without resolver bodies.
15. **SSE push** for resolver-discovered items.

### Phase 5: More resolvers
16. **Twitter/X resolver** (normalize `twitter.com` → `x.com`).
17. **Reddit resolver**.
18. **Generic resolver** (fetch `<title>`, `<meta description>` via HTTP).

---

## Open Questions

1. **Should resolver-created items be mutable?** If a GitHub issue title changes, should the item body update? Current model: items are append-only via ingests. A body update would be a new ingest in the same thread, which is fine — latest body wins in `item_bodies` (HashMap, last write wins during replay).

2. **Authentication for resolvers.** GitHub PAT in env var (`SLUG_GITHUB_TOKEN`)? Per-user OAuth tokens stored in the reducer? Start with a single server-wide token; per-user auth is a bigger design.

3. **Resolver item attribution.** Synthetic ingests need a `principal`. Options: a system user (`system:github-resolver`), the user who triggered the navigation, or a dedicated bot account. System user is simplest and doesn't conflate resolver actions with user actions.

4. **Should intermediate path nodes be auto-created?** When resolving `github.com/org/repo/issues`, should `github.com/org/repo` and `github.com/org` be created as items too? `add_child_edge` already creates parent→child edges, but the intermediate nodes won't have bodies unless explicitly created. Creating them with minimal bodies ("GitHub organization", "GitHub repository") from the resolver would make the tree more navigable.

5. **Backpack.tf and other non-API sites.** The issue mentions `backpack.tf/item/1375476291`. Sites without APIs need the generic resolver (HTML scraping for title/description) or remain manual-only. The iframe approach is the fallback for these — show the page if it allows framing, otherwise just link to it.

6. **Conflict between URL hierarchy and site semantics.** `github.com/org/repo` has `issues` and `pulls` as children, but these aren't "under" the repo in the URL the way `/a/b` is under `/a`. They're structural facets. The URL hierarchy happens to encode this correctly for GitHub, but other sites may have URL structures that don't map cleanly to a navigable tree. Resolvers handle this by explicitly declaring children rather than relying purely on URL structure.
