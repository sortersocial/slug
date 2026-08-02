# Sorting GitHub issues with slug — investigation

What it would take to: load a GitHub issues URL, pull the repo’s issues into the garden via the API, and sort them with slug’s ranking.

## Target flow

1. Someone opens (or an agent hits) a GitHub issues URL in slug, e.g.  
   `/-/https://github.com/sortersocial/slug/issues`
2. Slug fetches **all open** issues from that repo via the GitHub API (paginated).
3. **Each open issue is its own system ingest** in the import thread.
4. On refresh, issues no longer open on GitHub are **redacted** (deleted from the garden).
5. Sorting happens through existing pairwise votes + `GetGardenRank` (CLI/API).

## What works now

| Piece | Status | Where |
|-------|--------|--------|
| External GitHub URLs as garden items | Done | `ItemId` / `-/https://github.com/…` |
| Import all open issues (paginated) | Done | `list_issues` + `GITHUB_MAX_ISSUE_PAGES` |
| One post per open issue | Done | `resolve_github_issues` → `SystemIngest` per child |
| Delete closed/stale issues on refresh | Done | `SystemRedact` of prior system posts |
| Browser trigger | Done | `HtmlUiAction::ResolveExternal` / `POST /ui` |
| Issue cards in item / compare UI | Done | `slug-github-card` fence |
| Pairwise vote pool on children | Done | “vote on children” → `/vote?pool=…` |
| Rank via API / CLI | Done | `GetGardenRank`, `garden rank` |

**Manual path today**

1. Open `/-/https://github.com/{owner}/{repo}/issues` (logged in).
2. Click **Load / refresh children from GitHub**.
3. Vote on children (or CLI `garden pair` + votes).
4. Read order with `GetGardenRank` / `garden rank` for parent  
   `https://github.com/{owner}/{repo}/issues`.

## Remaining gaps

1. **Auto-import on GET** — still button-gated, not automatic when the URL is loaded.
2. **RPC/CLI resolve** — no `ResolveExternal` RPC; agents must use the browser UI (or paste DSL).
3. **Auth / rate limits** — large repos need `SLUG_GITHUB_TOKEN` (unauthenticated GitHub API is 60 req/hr).

## Why one post per issue

Redacting an ingest rebuilds garden projection without that post’s items. Closed issues are removed by redacting their system post—not by mutating a bulk multi-issue ingest.
