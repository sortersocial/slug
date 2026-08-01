# Sorting GitHub issues with slug — investigation

What it would take to: load a GitHub issues URL, pull the repo’s issues into the garden via the API, and sort them with slug’s ranking.

## Target flow

1. Someone opens (or an agent hits) a GitHub issues URL in slug, e.g.  
   `/-/https://github.com/sortersocial/slug/issues`
2. Slug fetches **all** (or as many as practical) issues from that repo via the GitHub API.
3. Issues become garden children under that parent.
4. Sorting happens through existing pairwise votes + `GetGardenRank` (CLI/API), not a separate “GitHub sort” algorithm.

## What already works

| Piece | Status | Where |
|-------|--------|--------|
| External GitHub URLs as garden items | Done | `ItemId` / `-/https://github.com/…` |
| On-demand import of issue children | Done (button) | `GitHubResolver::list_issues` → `resolve_github_children` |
| Browser trigger | Done | `HtmlUiAction::ResolveExternal` / `POST /ui` |
| Import as durable system ingest | Done | `WriteCmd::SystemIngest`, principal `system:github-resolver` |
| Issue cards in item / compare UI | Done | `slug-github-card` fence + `try_render_github_import_markup` |
| Pairwise vote pool on children | Done | “vote on children” → `/vote?pool=…` |
| Rank via API / CLI | Done | `GetGardenRank`, `npx slugsocial … garden rank` |
| Next-pair via API | Done | `GetPair` with `parent_path` |

**Manual path today**

1. Open `/-/https://github.com/{owner}/{repo}/issues` (must be logged in for the resolver button).
2. Click **Load / refresh children from GitHub**.
3. Click **vote on children** (or use CLI `garden pair` + `forum post` votes).
4. Read sorted order with `GetGardenRank` / `garden rank --json` for parent  
   `https://github.com/{owner}/{repo}/issues`.

Repo root (`…/{owner}/{repo}`) only imports **structural** children (`issues`, `pulls`, `commits`, `releases`). Issues themselves are only listed when the parent is the **`/issues`** segment.

## Gaps vs the ask

### 1. “When a GitHub URL is loaded” — not automatic

Import is **button-gated**, not part of the GET page load. Visiting the URL with empty children shows the resolver panel; nothing hits the GitHub API until `ResolveExternal`.

The original extensible-URLs plan (#134 / former `PLAN.md`) recommended **on-demand refresh on navigate**: if children are missing or last sync is older than N minutes, kick off a background resolve; render cached state immediately; refresh/SSE later. That path was never shipped — only the explicit button + 15s RAM cooldown (`GITHUB_RESOLVER_COOLDOWN_MS`).

**To ship auto-load on visit**

- In the external garden GET handler (or a small helper it calls), detect GitHub scopes that `can_resolve_children`.
- If no children (or stale per cooldown / last import ts), spawn `resolve_github_children` (same write path as today).
- Keep the page fast: either await with a short timeout + status morph, or fire-and-forget + “refresh when ready” (SSE already exists for threads).
- Auth: today resolve requires a session. Auto-load on anonymous GET needs a product call — system principal only, or still require login.
- Avoid stampeding: reuse `resolver_runs` (or persist last sync from `#import:…` ingest timestamps).

Rough surface area: `server/src/html/garden/` (item/render/routes) + small glue around `resolve_github_children`; extend `test/browser_github_resolver.clj`.

### 2. “Grab all issues” — capped and open-only

Current fetch:

```text
GET /repos/{owner}/{repo}/issues?state=open&per_page=100
pages 1..=GITHUB_MAX_PAGES  (3)
```

So at most **~300 open** issues; PRs filtered out via `pull_request` key; **closed issues never imported**.

**To grab all (or “enough”)**

| Change | Notes |
|--------|--------|
| Raise / remove `GITHUB_MAX_PAGES` | Follow GitHub `Link` headers until exhausted; guard with max items + timeout |
| `state=all` (or separate open/closed) | Closed issues are useful for historical ranking; UI may want filters |
| Prefer Search API for huge repos | `GET /search/issues?q=repo:o/r+is:issue` — different rate limits & pagination |
| Optional `SLUG_GITHUB_TOKEN` | Required in practice for large repos (60 req/hr unauthenticated vs 5k with PAT) |
| ETag / `If-None-Match` | Planned in #134; not implemented — helps refresh without burning quota |

Re-import already appends a new system ingest; latest body wins in `item_bodies`. Duplicate child edges are fine (set). Closed→reopen or title edits show up on refresh; **removed** GitHub issues are not pruned from the garden today (open question).

### 3. “Sort via API” — rank yes, import no

Sorting/ranking for an already-imported issues parent is fully API-capable:

```json
{"GetGardenRank": {"room": "public", "parent_path": "https://github.com/o/r/issues"}}
{"GetPair": {"room": "public", "parent_path": "https://github.com/o/r/issues"}}
{"Post": {"room": "public", "thread_tag": "…", "text": "… vote DSL …"}}
```

There is **no** `RpcCommand` / CLI verb to trigger GitHub resolve. Agents and scripts cannot “load URL → import issues → vote → rank” without driving the browser `POST /ui` path.

**To close the loop for API clients**

Add something like:

```text
RpcCommand::ResolveExternal { room, item_path, mode: children|siblings }
CLI: npx slugsocial public garden resolve -- <github-url>
```

Implementation can call the same `resolve_github_children` used by the HTML action (shared core; wire shapes stay separate per `agents.md`).

Optional convenience (later): a single “bootstrap” RPC that resolve + returns `GetGardenRank` / `GetPair` in one batch — not required if resolve is its own command and clients already batch RPCs.

### 4. Smaller related holes

- `ExternalResolver::fetch_body` for GitHub still returns “not implemented”; leaf issue pages rely on list-time cards, not a dedicated issue GET.
- Org listing uses `/users/{owner}/repos` (works for users; orgs may want `/orgs/{org}/repos`).
- Intermediate parents without bodies remain thin navigation nodes (acceptable per original plan).

## Suggested build order

1. **`ResolveExternal` RPC + CLI** — unlocks agent/API sorting without the browser; reuses existing resolver. Smallest high-leverage change.
2. **Pagination / “all issues”** — raise page cap or Link-follow; decide `state=open` vs `all`; document token requirement.
3. **Auto-resolve on GET** when children empty/stale — matches “when a GitHub URL is loaded”; needs auth + cooldown product choices.
4. **Polish** — ETag caching, `fetch_body` for issue leaves, SSE refresh after background import, prune/archive policy for vanished issues.

## Effort / risk (technical)

- **RPC + CLI resolve:** localized — `types` enum, `rpc.rs`, `cli`, one integration test, reuse `resolve_github_children`. Low risk.
- **Full pagination + `state=all`:** mostly `github.rs` + mock server in `browser_github_resolver.clj`; watch ingest size (one big DSL doc per refresh) and rate limits. Medium risk for large repos.
- **Auto on page load:** touches garden GET path, auth, cooldown, and UX for empty→populated; highest product ambiguity (anonymous vs logged-in, sync vs async). Medium–high risk.

## Non-goals for a first cut

- Replacing pairwise slug ranking with GitHub’s own sort (`created`, `comments`, reactions).
- Webhooks / periodic background sync (plan phases 7b/7c).
- Pushing ranked order back onto GitHub.

## Bottom line

Slug can already **import** GitHub issues (on button) and **sort** them via the existing garden rank API once they are children. What’s missing for the stated product is mainly: **auto-import on load**, **complete issue fetch**, and an **RPC/CLI resolve** so “sort via API” works end-to-end without the browser.
