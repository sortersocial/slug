# ChatGPT marketplace MCP app for slug

Research note (2026-08-26). How OpenAI's ChatGPT / Codex plugin directory works, and what it would take for slug.social to ship as a first-party product integration there.

This is **not** the 2023 GPT Store / ChatGPT Plugins beta. That stack was wound down. The current product is:

- **Apps in ChatGPT** (preview since Oct 2025; Business / Enterprise / Edu later)
- Built with the **Apps SDK**, which is MCP plus optional UI
- **Submitted and published as plugins** into a **universal Plugins Directory** shared by ChatGPT and Codex

Canonical docs:

- [Apps SDK](https://developers.openai.com/apps-sdk)
- [Build an MCP server](https://developers.openai.com/apps-sdk/build/mcp-server)
- [Authenticate users](https://developers.openai.com/plugins/build/auth)
- [Submit plugins](https://developers.openai.com/plugins/deploy/submission)
- [MCP review requirements](https://developers.openai.com/plugins/deploy/app-review)
- [Connect and test](https://developers.openai.com/plugins/deploy/connect-chatgpt)
- [Company-knowledge `search` / `fetch`](https://developers.openai.com/api/docs/mcp)
- [App guidelines](https://developers.openai.com/apps-sdk/app-submission-guidelines)

---

## What a marketplace listing actually is

A public ChatGPT "app" is a **plugin** that can contain:

1. **An MCP server** (live tools, auth, optional UI) — this is the product-integration path.
2. **Skills** (`SKILL.md` plus files) — reusable workflows, either uploaded or imported from the MCP server at scan time.
3. **Both**.

Users find it in the Plugins Directory (search by name, or a direct listing URL). Enhanced homepage placement is OpenAI-selected; you cannot request it. After approval you still have to hit **Publish** — review does not auto-list.

ChatGPT also converts approved apps into **Codex plugins**, so one submission covers both hosts.

There are three shapes you can submit:

| Shape | When to use for slug |
| --- | --- |
| MCP-only | First ship. Tools wrap garden / forum / search. UI optional. |
| Skills-only | Weak fit. Slug needs live rankings and writes, not a static checklist. |
| Skills + MCP | Best later. Import `GUIDE.sorter` as a skill that teaches the model how to vote, then call tools. |

Most listings use a **universal MCP URL**: one HTTPS endpoint for every user. Template URLs (`https://{workspace}.example.com/mcp`) are only for trusted partners with per-tenant hosts. Slug is a single public site plus private rooms behind one origin, so **universal `https://slug.social/mcp`** is the right choice.

---

## Runtime architecture

```
ChatGPT / Codex
    │  streamable HTTP (usually POST https://slug.social/mcp)
    │  Authorization: Bearer <oauth access token>
    ▼
slug MCP server  (resource server)
    │  tools/list, tools/call, optional resources, optional skills/list
    ▼
existing reducer / RpcCommand / events.jsonl
```

The MCP server is **not** a second product. It is another command surface next to:

- `POST /api/v0/rpc` (`RpcCommand`) — CLI / automation
- `POST /ui` (`HtmlUiAction`) — browser morph UI

Same durability, same authz, different wire format.

### Transport

Public plugins must speak **MCP streamable HTTP** on a stable HTTPS origin. Typical path is `/mcp`. Local / ngrok / Secure MCP Tunnel is fine for developer-mode testing and **not** acceptable for submission.

ChatGPT presents an OpenAI-managed **mTLS client cert** (`SAN dnsName = mtls.prod.connectors.openai.com`). You can require that chain to prove the caller is ChatGPT. That authenticates the **host**, not the **user**. User identity is OAuth 2.1.

If you IP-allowlist, use OpenAI's published connector egress ranges and refresh them automatically. An allowlist does not replace auth.

### What the server advertises

On `initialize`:

- `name` / `version` (stable product name, e.g. `slug-social`)
- `instructions` — cross-tool guidance, keep the important part in the first 512 characters (required sequences, public-vs-private rooms, "ask the human before voting")
- tools with title, description, input schema, output schema, annotations, `securitySchemes`
- optional UI resources
- optional `io.modelcontextprotocol/skills` extension

On each tool result:

- `structuredContent` — data the model will chain on (ids, ranks, URLs)
- `content` — short text the model can quote
- `_meta` — host-only (widget state, `mcp/www_authenticate`). Hidden from the model. Not a place to put secrets.

Do not leak bearer tokens, session ids, or debug payloads in tool results. Reviewers reject undisclosed PII.

### Tool annotations (reviewers check these against real behavior)

| Annotation | Slug meaning |
| --- | --- |
| `readOnlyHint: true` | Fetch / list / rank / search / `check` dry-run. No event log append. |
| `readOnlyHint: false` | `Post`, `PostRedact`, room grant/revoke/delete, graduate, invite mint. |
| `openWorldHint: true` | Anything that changes **public** internet-visible state: public forum post, public garden vote, thread graduate onto the public site. |
| `openWorldHint: false` | Private-room writes only. |
| `destructiveHint: true` | `PostRedact`, `RoomDelete`, `RoomRevoke`. Irreversible or hard to undo. |

A public `forum post` is both a write and an open-world action. Annotate it that way. A justification that says "functionally read-only" will not override `readOnlyHint: false`.

### Company knowledge / Deep Research

If you implement the standard **`search`** and **`fetch`** tools (and mark other reads `readOnlyHint: true`), ChatGPT can treat the plugin as a company-knowledge source.

Required shapes:

- `search({ query })` → `{ results: [{ id, title, url }] }`
- `fetch({ id })` → `{ id, title, text, url, metadata? }`

Citations only appear when `url` is a non-empty absolute URL. Slug already has shareable GET URLs (`/t/:tag`, garden item paths, `/-/https://…`). Use those as `url`. Keep internal ids in `id`.

Slug already has `RpcCommand::Search` plus item/thread/post fetch. Mapping that pair onto the company-knowledge schema is the highest-leverage read surface.

---

## Optional UI (widgets)

Custom UI is **optional**. Tools must work without it (Codex and text-only clients will not render a widget).

If you add UI:

- Register an MCP Apps resource (HTML/JS/CSS bundle).
- Point selected tools at it with `_meta.ui.resourceUri` (legacy alias: `_meta["openai/outputTemplate"]`).
- The bundle runs in a **sandboxed iframe**. Talk to the host over the MCP Apps JSON-RPC `postMessage` bridge (`ui/initialize`, `ui/notifications/tool-result`, `tools/call`, `ui/message`).
- Prefer the standard bridge. Use `window.openai` only for ChatGPT-only extras (files, `requestModal`, Instant Checkout).
- Declare a **CSP** that lists every origin the iframe fetches.

**Do not reuse the current browser UI.** The web app is `POST /ui` + `eval` of server-emitted morph JS (`agents.md`). ChatGPT's iframe CSP will not allow that, and it is the wrong contract anyway. Widgets must be self-contained HTML that consume `structuredContent`.

Recommended split (OpenAI's own guidance):

1. **Data tools** return ranks / pairs / thread items. No widget attached.
2. **Render tools** (`render_rank_widget`, `render_pair_widget`) take ids and attach the UI resource.

Natural slug widgets later, not in v1:

- ranked list for a garden parent
- pairwise compare card (the fullscreen `/vote/compare` idea, as an inline/fullscreen widget)
- thread excerpt with cite-able post URLs

v1 should ship **MCP-only, no UI**. Screenshots are only for plugins that have UI; empty screenshot slots are required if you have none.

---

## Authentication (the hard part)

Slug today:

- Human principal from **Google OAuth**
- Durable credential is `slug_<token_id>_<secret>` (cookie `slug_session` or `Authorization: Bearer`)
- CLI agents add a **delegate** `uuid:rig:provider/model` on write
- Pending sessions are **RAM-only**
- Tokens live in the event log

ChatGPT will **not** send a `slug_…` key that the user pasted. For any private data or write tool it runs **OAuth 2.1 authorization-code + PKCE (S256)** as specified by MCP authorization. ChatGPT is the OAuth **client**. Your MCP endpoint is the **resource server**. Token minting belongs on an **authorization server**.

### What ChatGPT expects

1. **Protected resource metadata** on the MCP host:

   `GET https://slug.social/.well-known/oauth-protected-resource`

   ```json
   {
     "resource": "https://slug.social/mcp",
     "authorization_servers": ["https://slug.social"],
     "scopes_supported": ["slug.read", "slug.write"]
   }
   ```

2. **AS metadata** at `/.well-known/oauth-authorization-server` or OIDC `/.well-known/openid-configuration`:

   - `authorization_endpoint`, `token_endpoint`
   - `code_challenge_methods_supported` **must include `S256`** (hard fail otherwise)
   - `token_endpoint_auth_methods_supported` intersecting ChatGPT's CIMD (`none` and/or `private_key_jwt`)
   - optional `client_id_metadata_document_supported: true` (preferred)
   - optional `registration_endpoint` (DCR fallback)
   - optional `authorization_response_iss_parameter_supported: true` plus `iss` on every auth response → ChatGPT uses the stable redirect `https://chatgpt.com/connector_platform_oauth_redirect`

3. **Echo `resource`** from authorize + token requests into the access token `aud` (or equivalent). The MCP server must verify `iss`, `aud`, `exp`, scopes on every call.

4. **Per-tool `securitySchemes`**:

   - `{ type: "noauth" }` — public garden/forum reads
   - `{ type: "oauth2", scopes: ["slug.write"] }` — posts, votes, redacts, rooms
   - both — optional linking (anonymous public read, login unlocks writes)

5. **Runtime challenge**. Metadata alone is not enough. An unauthenticated write must return an error result with:

   `_meta["mcp/www_authenticate"]` containing `error` and `error_description`, pointing at the resource metadata URL.

   That pair is what pops ChatGPT's linking UI.

6. **Workspace domain restrictions** (Enterprise): AS must advertise `openid` + `email`, and a UserInfo endpoint that returns `email` + `email_verified: true`.

7. **Reviewer demo account**: no MFA, no email/SMS step, no private network. They will reject otherwise.

### Client registration

Prefer **CIMD** (Client ID Metadata Documents). ChatGPT sends an HTTPS URL as `client_id` (`https://chatgpt.com/oauth/client.json` or a callback-id-specific document). Your AS fetches it, allowlists the redirect, and treats that URL as the client id. No per-connection client explosion.

DCR (`registration_endpoint`) still works; ChatGPT registers once per connection. Harder to administer.

ChatGPT does **not** do client-credentials, service-account, or "paste your API key" for published plugins.

### What this means for slug's existing Google login

Google is an identity provider for **humans logging into slug**. ChatGPT needs slug (or a hosted IdP in front of slug) to be an **OAuth 2.1 authorization server** that ChatGPT can talk to.

Two workable designs:

**A. Slug becomes a thin AS (recommended if we want first-party control)**

- `authorization_endpoint` reuses the existing Google login + username-choose flow, then issues a **short-lived JWT** (or the existing `slug_…` token encoded as a JWT) with `aud=https://slug.social/mcp`.
- `token_endpoint` does PKCE verify + refresh.
- Publish CIMD support with `token_endpoint_auth_methods_supported: ["none"]` (public client + PKCE) or `private_key_jwt`.
- MCP handlers call the same `verify_token` / principal resolution the RPC layer uses.

This is real protocol work (discovery docs, PKCE, refresh, `resource`/`aud`, CIMD fetch). OpenAI explicitly recommends **not** writing an AS from scratch if you can avoid it.

**B. Put Auth0 / similar in front**

- Auth0 already has MCP + CIMD guides.
- After Google (or Auth0 social) login, a slug-side hook still has to mint or bind a slug principal + durable token.
- Faster protocol compliance, extra vendor, still need a stable mapping from IdP subject → slug username.

**Do not** try to have ChatGPT complete slug's current `/auth/login` redirect and then scrape a `slug_…` cookie. That is not the MCP client contract.

### Identity / delegates in ChatGPT

`GUIDE.sorter` says writes from non-browser clients need `--delegate '<uuid>:<rig>:<provider/model>'`, and the human principal comes from OAuth.

In ChatGPT:

- Human principal = the linked slug user (from OAuth).
- Delegate should be a **server-assigned** binding for that ChatGPT user + conversation/host, e.g. `<uuid>:chatgpt:openai/<model>`, stored the same way CLI binds are stored.
- Do not ask the model to invent or remember a UUID. That is impersonation-prone (the guide already forbids writing the UUID into shared memory).
- Browser-style human posts (no delegate) are also valid if we treat ChatGPT as "the human is present and confirming." That matches the "ask your human before voting" rule better than a silent agent bind.

Recommendation: **v1 writes are human-principal posts** (like the website). Add ChatGPT-as-rig delegates only if we want agent continuity across chats.

---

## How users install and use it

### Developer mode (before submission)

1. ChatGPT → Settings → Security and login → Developer mode.
2. [chatgpt.com/plugins](https://chatgpt.com/plugins) → plus → name, description, MCP URL `https://…/mcp` (or a Secure MCP Tunnel id).
3. Review discovered tools.
4. New chat → enable the connection from the tools / More menu → prompt.
5. After metadata changes: open the connection → **Refresh** (published plugins do **not** live-refresh metadata; they use the reviewed snapshot).

Also useful: `npx @modelcontextprotocol/inspector` against streamable HTTP, and API Playground → Tools → Add → MCP Server.

### Public directory (after publish)

User searches "slug" (or opens the listing URL) → install → first write tool triggers OAuth linking → ChatGPT calls tools when the prompt matches descriptions / starter prompts.

Discovery quality depends on tool descriptions, server `instructions`, and starter prompts. Treat those as product copy.

---

## Submission and review

Prerequisites:

- OpenAI Platform org with **identity verification** (individual or business). Mismatch with the public listing name is a reject.
- Role permission **Apps Management = Write** (`api.apps.write`).
- Global-residency project (EU-residency projects cannot submit MCP plugins today).
- Public production MCP URL, not a tunnel.
- Domain verification: portal token at `https://slug.social/.well-known/openai-apps-challenge` (exact token only, not JSON). Parent host is allowed if the MCP host is a subdomain.
- Privacy policy, terms, support, website URLs that match the publisher.
- **5 positive + 3 negative** test cases with expected tool, result shape, and fixture data.
- Starter prompts.
- If OAuth: reviewer credentials that work without MFA.

Flow:

1. Create plugin → "With MCP".
2. Paste universal MCP URL, auth details, CSP (if UI).
3. **Scan Tools** — dashboard snapshots tools, schemas, annotations, `securitySchemes`, `_meta`, UI resources, `instructions`, and any MCP-exported skills.
4. Fill listing + attestations → Submit for review.
5. Wait (no expedite).
6. On approval, **Publish**. Then it is searchable in the directory.

Metadata is a **versioned snapshot**. Changing tool names, schemas, annotations, instructions, or UI resource URIs requires scan → new version → review → publish. Live result payloads can change without a resubmit if the published contract stays compatible. Changing scheme/host/port of the MCP origin requires a **new plugin**, not a new version.

Common rejects that slug should pre-empt:

- Cannot reach `/mcp` or reviewer cannot log in.
- Test cases don't match actual tool selection / output.
- Tool results include tokens, internal ids, or PII not in the privacy policy.
- `readOnlyHint` / `openWorldHint` / `destructiveHint` don't match behavior.
- App is an **unofficial connector** to a third party. Slug's GitHub resolver is a first-party feature of slug, but a ChatGPT app whose *primary* job is "talk to GitHub through us" would be rejected. Keep GitHub import out of the v1 tool list.

---

## Mapping slug onto tools

Do **not** expose raw `RpcCommand` as one mega-tool. OpenAI wants one tool per user goal.

### v1 — public read + authenticated write

| Tool | Goal | Auth | Annotations | Existing code |
| --- | --- | --- | --- | --- |
| `whoami` | Linked human + bound delegates | oauth2 | read-only | bearer + `agent_bindings` |
| `search` | Find items, threads, posts | noauth + oauth | read-only | `RpcCommand::Search` (`room` optional) |
| `fetch` | Open one search hit by id | noauth + oauth | read-only | `GetGardenItem` / `GetForumThread` + post id |
| `list_threads` | What's circulating | noauth + oauth | read-only | `ListForumThreads` |
| `get_thread` | Read a thread page | noauth + oauth | read-only | `GetForumThread` |
| `get_rank` | Ranked children under a path | noauth + oauth | read-only | `GetGardenRank` / `GetGlobalRank` |
| `get_item` | Item body + related threads | noauth + oauth | read-only | `GetGardenItem` |
| `get_pair` | Next comparison in a scope | noauth (pair is public) | read-only | `GetPair` |
| `check_sorter` | Dry-run a `.sorter` doc | noauth + oauth | read-only | `Check` |
| `list_rooms` | Private rooms the human can access | oauth2 `slug.read` | read-only | `RoomList` |
| `read_room` | Open one private room (members, threads, recent posts) | oauth2 `slug.read` | read-only | `RoomAudit` + `ListForumThreads` + `GetFeed` |
| `get_feed` | Activity since this delegate (or principal) last posted | oauth2 `slug.read` | read-only | `GetFeed` |
| `get_matchup` | Per-item win/loss history + thread behind each vote | noauth + oauth | read-only | `GetMatchup` |
| `identity_start` / `identity_poll` | Mint a conversation-bound delegate (`uuid:rig:provider/model`) | noauth + oauth | write (session) | pending-session / in-process mint |
| `create_room` | Create a private room + optional members | oauth2 | write, not open-world | `RoomCreate` + `RoomGrant` |
| `grant_room` | Add a member | oauth2 | write, not open-world | `RoomGrant` |
| `audit_room` | List room members | oauth2 | read-only | `RoomAudit` |
| `post_sorter` | Publish a comparison / definition | oauth2 | write, **open-world** if `room=public` | `Post` (`delegate` required) |
| `redact_post` | Tombstone own post | oauth2 | write, destructive | `PostRedact` |

Return absolute `https://slug.social/…` URLs on every structured object so the model can cite and the user can open the real site.

Private rooms are first-class authenticated reads: `list_rooms`, `read_room`, `get_feed` (`oauth2` + `slug.read`). Other room-scoped reads list `slug.read` before `noauth` so linked clients send the bearer. `create_room` is write + `openWorldHint: false`. Invite mint and graduate stay out of the tool list.

### Skills (v1.5)

Import a static skill from the MCP server (`skills/list` + `resources/read`, SEP-2640 subset):

- `name`: `slug-compare`
- Teaches: get a pair → ask the human → write a `.sorter` doc (item bodies, `{ reason }`, `3:1` / `>` / `=`) → `check_sorter` → `post_sorter` on a thread tag
- Source of truth is already `cli/GUIDE.sorter`

Scan Tools snapshots skills; live edits do not update the published plugin until you scan + resubmit.

Limits: 5 skills, 100 files each, 256 KiB `SKILL.md`, 5 MiB per skill.

### What not to expose in v1

- GitHub resolver buttons (`ResolveExternal`) — third-party API, review risk, not the core loop
- Room admin / invite mint (RAM-only invites also make a poor reviewer story)
- `CopyGardenRank` / HUD / theme — browser-only
- Raw event log / health internals

---

## Implementation sketch (when we build it)

Keep MCP **inside `slugsocial-server`**, not a Node sidecar. One process, one event log, one principal verifier. Official examples are TypeScript/Python; the protocol is HTTP + JSON-RPC and Rust can speak it (or we vendor a small streamable-HTTP handler).

New routes on the existing Axum app:

- `POST /mcp` (and GET/DELETE as the transport requires) — streamable HTTP
- `GET /.well-known/oauth-protected-resource`
- `GET /.well-known/oauth-authorization-server` (if slug is the AS)
- `GET/POST` authorize + token (if slug is the AS)
- `GET /.well-known/openai-apps-challenge` — static token from env for the portal

Handlers should call the same functions `handle_rpc_batch` already uses, then wrap `RpcResult` as `structuredContent`. Do not add a parallel write path.

Server `instructions` (draft):

> Slug is a garden (path-addressed ontology + pairwise rank centrality) and a forum (bump-ordered threads). Public reads work anonymously. Private rooms require the linked human. Before posting, call `whoami`, `get_pair` or `get_item`, ask the human, draft a `.sorter` document, `check_sorter`, then `post_sorter` with a required `delegate` (`uuid:rig:provider/model`). Do not invent a UUID. Cite the `url` fields. Every post read exposes `actor` and `delegate`.

Deploy: same Fly app (`slug.social`). No new origin — changing origin later means a new plugin listing.

---

## Fit and risks

**Why this is a good product surface**

- Slug is already designed for "human + model write a comparison together." ChatGPT is that loop without `npx slugsocial`.
- Public garden/forum can start **anonymous read**, which is the easy half of MCP.
- Shareable URLs already exist for citations.
- The Plugins Directory is how a non-CLI audience finds the site.

**Why it is not a weekend wrapper**

- OAuth 2.1 AS (or Auth0) is new infrastructure. Current Google login + `slug_…` bearer is the wrong client protocol.
- Review is a real gate: verified identity, privacy policy, 8 test cases, hint accuracy, no MFA demo account.
- The morph/`eval` web UI cannot be the widget.
- Public posts are open-world writes; ChatGPT will confirm them. That is correct and must be designed for.
- Official policy forbids unofficial third-party connectors. Keep the app about **slug**, not GitHub.

**Monetization**

Not relevant yet. Instant Checkout / Agentic Commerce is beta for selected marketplaces. Digital goods must use external checkout on your own domain. Slug has no checkout today.

---

## Suggested sequence

1. Add `/mcp` with the v1 **read** tools (`search`, `fetch`, `get_rank`, `get_item`, `get_pair`, `list_threads`, `get_thread`, `check_sorter`). No auth. Hit it with MCP Inspector.
2. Connect in ChatGPT developer mode against production or a public preview URL.
3. Add OAuth 2.1 (Auth0 or slug-as-AS) and `post_sorter` / `redact_post`.
4. Write privacy/terms pages + reviewer account.
5. Optional: import `slug-compare` skill from `GUIDE.sorter`.
6. Submit universal plugin. Widgets only after the text tools feel good.

## Implementation status (this branch)

v1 is in `server/src/mcp/`:

- `POST /mcp` — JSON-RPC `initialize`, `tools/list`, `tools/call`
- Read tools + `post_sorter` / `redact_post` via `dispatch_rpc`
- `post_sorter` requires `delegate`; server binds it to the linked human
- Private rooms: `create_room(name, visibility=private, members)`, `list_rooms`, `grant_room`, `audit_room`, `room_id` on reads/writes
- `whoami` returns the linked username and bound delegates
- Search/fetch/thread/post results expose `actor` + `delegate`
- OAuth 2.1 + PKCE at `/oauth/authorize` + `/oauth/token` (Google login, access token is `slug_…`)
- Well-known metadata + `/.well-known/openai-apps-challenge`

Still later: ChatGPT developer-mode connect, reviewer account, privacy/terms, optional `slug-compare` skill, widgets. Signing is mandatory at the MCP tool boundary; website/RPC human posts may still omit delegate.
