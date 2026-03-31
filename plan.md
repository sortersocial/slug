# Access Control Architecture v2

## Preamble

All prior access control code has been excised from the codebase: configured API keys, actor passkeys, private-namespace enforcement, and the X/Twitter bot. No legacy auth events exist in the event log. This is a clean slate.

This document specifies the access control system to be built. It covers identity, authentication, authorization, and the data model changes required. It is structured for consumption by an implementing conversation that has no prior context.

---

## 1. The Core Tension

Slug is a platform where humans think and AI agents write.

The human has the perspective — the taste, the experience, the stake in what gets ranked. The agent has the facility — it constructs the DSL, validates syntax, manages the mechanical act of submitting. Neither operates alone. Every ingest is a joint act: a human who authorized it and an agent who produced it.

The access control system must solve three problems simultaneously:

**Attribution without ambiguity.** Every piece of content must trace back to a human. Not "a human or a bot" — a human, always, with the bot identified as the instrument. The event log cannot contain orphan agent content. If you pull any thread, you find a person at the end of it. The ranking system's integrity depends on knowing that real humans are behind the preferences being expressed. An agent producing volume without human backing is spam, not signal.

**Delegation without ceremony.** The human should not have to re-authenticate every time they open a new chat, switch models, or start a fresh session. The agent should not need to understand the auth system to use it. The human's identity lives on the machine; the agent discovers it. The agent's own identity is ephemeral — generated fresh per session, bound to the human on first use. Authentication is infrastructure, not interaction.

**Boundaries without fragmentation.** The platform needs private space and shared space alongside the public commons. But these must not fracture the ontology. `~/languages/python` means the same thing everywhere — it's a concept, a node in the garden. What differs across spaces is not what things are, but what has been said about them, by whom, in what context. The item is global. The discourse around it is scoped.

---

## 2. Entities

### User (`@username`)

The human principal. The origin of all authority on the platform. Every piece of content traces back to a user.

- **DSL syntax**: `@tommy`
- **Canonical form**: `tommy` (strip one leading `@`, lowercase)
- **Format**: alphanumeric, hyphens, underscores. 1–32 characters. Lowercase.
- **Sigil mnemonic**: `@` means origin, inception — the human is the source.

### Agent (`@@uuid:rig:provider/model`)

An AI delegate acting on behalf of a user. Bound to exactly one user on first use (immutable thereafter). Inherits all permissions from its user. Ephemeral per chat session — a new conversation generates a new agent identity.

- **DSL syntax**: `@@7a3b9c2d-1234-5678-90ab-cdef12345678:claudeai:anthropic/claude-sonnet-4.5`
- **Canonical form**: `@7a3b9c2d-1234-5678-90ab-cdef12345678:claudeai:anthropic/claude-sonnet-4.5` (strip one leading `@`)
- **Format**: `<uuid-v4>:<rig-name>:<provider/model>`. UUID must be valid v4. Rig is freeform ASCII (e.g. `claudecode`, `claudeai`, `cursor`, `codex`). Model must contain `/`.
- **Sigil mnemonic**: `@@` means "of" — the agent is of the human.

### Thread

The primary content container and the permission boundary. A sequence of posts under a tag or identifier.

**Public thread:**

- **Identifier**: tag (e.g., `languages`)
- **URL**: `/t/languages`
- **Creation**: implicit on first ingest (a new `#tag` in a DSL document creates the thread)
- **Visibility**: public — readable by anyone, writable by any authenticated user

**Shared thread:**

- **Identifier**: `<short-id>/<slug>` (e.g., `1s813vu/notes`)
- **URL**: `/t/a7f2k9x/project-review`
- **Creation**: explicit, creator specifies initial member list
- **Owner**: the creating user
- **Members**: explicit set of users (including the owner)
- **Visibility**: shared — readable and writable only by members and their agents

**Note:** "Private" threads are just shared threads with a single member (the owner). There is no separate private visibility level.

**Short ID generation:** A random base-36 string, 7 characters long (~78 billion combinations). Generated server-side at thread creation time. On collision, redraw. The short ID is not human-meaningful — it's namespace isolation. The slug after the slash is the human-chosen name.

Why not sequential counters: `/t/0/notes`, `/t/1/notes` leaks cardinality. An attacker can probe sequential IDs and learn how many `notes` threads exist, even if each returns 404. Random short IDs reveal nothing about the population.

Why not word-pairs: word-pairs (`crimson-atlas/notes`) try to be human-friendly, but shared thread URLs aren't things you tell people over the phone — they're things you click in a link your friend sends you. The short ID is honest about what it is: a disambiguator.

### Post

A single ingest — one DSL document committed to a thread. Always carries both a principal (`@`) and a delegate (`@@`). Inherits visibility from its thread.

### Item (`~/path/to/item`)

An ontology node in the garden. A concept with a path and optional body text. The path namespace is global — `~/languages/python` is the same concept everywhere.

**Scoping rule**: items defined in public threads appear in the public garden (browsable, searchable). Items defined in shared threads are scoped to that thread — they do not appear in the public garden index.

### Vote

A pairwise comparison between two items within a post. Scoped to the thread the post belongs to.

**Ranking rule**: votes in public threads feed the public ranking. Votes in shared threads do not affect the public ranking. Per-thread or per-user ranking views are deferred.

---

## 3. Identity & Authentication

### Two Layers of Identity

The system has two identity layers with different lifetimes and storage models:


|              | Human identity                                              | Agent identity                  |
| ------------ | ----------------------------------------------------------- | ------------------------------- |
| **Lifetime** | Durable, persists across all sessions                       | Ephemeral, one per chat session |
| **Storage**  | `~/.config/slugsocial/token` on the machine                 | Chat context only               |
| **Scope**    | One per machine (all agents on this machine are this human) | One per conversation            |
| **Creation** | Once, at registration                                       | Every new session               |


One human per machine is correct. Every Claude session on that machine — Claude Code, claude.ai sandbox, Cursor, whatever — inherits the same human identity. The agent identity is the per-session differentiator. The human identity is the durable thing. The config file just means "this computer belongs to tommy."

### Bearer Tokens

The authentication credential is a bearer token: `slug_<random-base62-40chars>`. Stored in the event log as `SHA-256(token + salt)`. The raw token is returned exactly once at registration and never stored by the server.

The token is read from (in priority order):

1. `SLUG_TOKEN` environment variable (for CI, scripts, explicit override)
2. `~/.config/slugsocial/token` file (primary path for interactive use)

The CLI sends `Authorization: Bearer <token>` on every HTTP request. Stateless, file-driven.

Event-sourced because everything else is. Registration events, binding events, and ingest events all live in the same append-only JSONL log, replayed through the same reducer. A separate auth store would introduce a second source of truth.

### Registration & Login via OAuth Link

The primary authentication flow uses OAuth through a browser link. Registration and login are the same flow — first OAuth bind creates the user, subsequent ones authenticate and refresh the token.

**Flow:**

1. Agent runs `npx slugsocial identity --rig claudeai --model anthropic/claude-sonnet-4.5`
2. Server creates a pending session, generates a UUID for the agent, returns:
  - The agent identity (`@@uuid:rig:model`)
  - A login URL: `https://slug.social/auth/login?session=<pending-session-id>`
3. Agent presents the login URL to the human
4. Human clicks the link → Google OAuth screen
5. On OAuth success, server resolves the pending session:
  - If the Google account is new: prompts for username, creates user, emits `UserRegistered`
  - If the Google account is known: looks up existing user
6. Server binds the agent to the user (emits `AgentBound`), writes the bearer token to a response that the CLI captures
7. CLI writes the token to `~/.config/slugsocial/token`

After this, the human never touches auth again on this machine. Every subsequent CLI invocation reads the token from the config file.

**The link is the ceremony.** The agent never touches credentials. The human never pastes tokens. One click, one OAuth screen, done. This also means the agent doesn't need to understand auth to use it — it runs `identity`, shows a link, and proceeds once the binding completes.

### Fallback: Direct Token Registration

For power users, headless environments, or CI:

```
POST /api/v0/register  { "username": "tommy" }
→ 201 { "ok": true, "user": "@tommy", "token": "slug_Ax7b..." }
```

CLI equivalent:

```
npx slugsocial register --username tommy
→ Token: slug_Ax7b...
  Saved to ~/.config/slugsocial/token
```

This path skips OAuth entirely. The token is generated server-side and returned once. Useful for scripting and automation. Can coexist with OAuth — a user who registered via OAuth can also generate API tokens.

### Whoami

An agent in a fresh chat session needs to discover the human's identity. The `whoami` command reads the token from the config file (or `SLUG_TOKEN` env var) and queries the server.

```
GET /api/v0/whoami
Authorization: Bearer slug_Ax7b...
→ { "user": "@tommy", "agents_bound": 3 }
```

If no token is found, the CLI returns onboarding instructions designed to be consumed by the agent:

```
No token found. Ask your human:
  1. Do you have a slug.social account?
  2. If yes: run `npx slugsocial login` to authenticate.
  3. If no: run `npx slugsocial identity --rig <rig> --model <model>` to get started.
```

### Agent Identity & Implicit Binding

Each new chat session generates a fresh agent identity via `npx slugsocial identity --rig <name> --model <slug>`. This produces a new UUID. The agent remembers it within its session context.

If a valid token already exists on the machine (`~/.config/slugsocial/token`), the identity command skips the OAuth link and immediately binds the new agent to the existing user. The agent is ready to post.

If no token exists, the OAuth link flow kicks in (see above).

On the first ingest where a new `@@` identity appears with a human's bearer token, the server automatically binds that agent to the human by appending an `AgentBound` event if not already bound. No explicit registration step.

Binding is immutable: once an agent identity is bound to a human, it cannot be rebound. If a different human's token is used with an already-bound agent identity, the request is rejected.

### Authentication Flow (Per Ingest)

1. CLI reads token from `~/.config/slugsocial/token` (or `SLUG_TOKEN` env var), sends `Authorization: Bearer <token>`
2. Server hashes the token, looks up `token_index[hash]` → `@tommy`
3. Server parses the DSL document, extracts `@tommy` (principal) and `@@uuid:rig:model` (delegate)
4. Server verifies `@tommy` matches the token's identity
5. Server checks agent binding:
  - Unbound → append `AgentBound` event, proceed
  - Bound to `@tommy` → proceed
  - Bound to different user → reject (403)
6. Server checks thread permissions (see §4)
7. Proceed to existing validation and event persistence

---

## 4. Threads & Permissions

### Thread as Permission Boundary

Threads are the unit of access control. Every post belongs to a thread. Permissions attach to the thread, and posts inherit them.

A separate "room" entity (grouping multiple threads under one permission scope) was considered and deferred. It only earns its existence when organizations need to manage many threads under one access policy. The thread model doesn't preclude adding rooms later — they would be a grouping layer whose member set is inherited by child threads.

### Visibility Levels


| Level       | Read                   | Write                  | Thread creation          |
| ----------- | ---------------------- | ---------------------- | ------------------------ |
| **Public**  | Anyone                 | Any authenticated user | Implicit on first ingest |
| **Private** | Owner + their agents   | Owner + their agents   | Explicit (CLI/API)       |
| **Shared**  | Members + their agents | Members + their agents | Explicit (CLI/API)       |


### Fine-Grained Capabilities

Access to a shared thread is not binary (member or not). Each user holds a set of independent capabilities on each thread. No capability implies any other — all grants are explicit.

```rust
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum ThreadCapability {
    View,     // read the thread, see posts and rankings
    Vote,     // cast pairwise votes between existing items
    AddItem,  // define new items with bodies
    Manage,   // grant/revoke capabilities on other users
}
```

**Why Vote and AddItem are separate:** The use case is "come rank my curated list." You've built an ontology and want someone's opinion on the ranking, but you don't want them adding items that dilute your curated set. Grant `[View, Vote]`. They can vote between your items but can't introduce new ones.

**Prose rides along with any posting capability.** An ingest that contains only prose (no items, no votes) requires the user to hold at least `Vote` or `AddItem`. If a user holds only `View`, they cannot post at all.

**No implication chains.** `Manage` does not implicitly grant `Vote`. `Vote` does not implicitly grant `View`. To give someone full participation, you grant `[View, Vote, AddItem]` — all three, explicitly. The data model stores exactly what was granted; the permission check is a single `HashSet::contains` per capability. The CLI provides convenience bundles for common combinations (see §4 Thread Invitations below).

### Permission Check Matrix


| Action                         | Auth required | Check                                            |
| ------------------------------ | ------------- | ------------------------------------------------ |
| Read public thread             | No            | —                                                |
| Read shared thread             | Bearer token  | User has `View`                                  |
| Post to public thread          | Bearer token  | Any registered user                              |
| Post vote to shared thread     | Bearer token  | User has `Vote`                                  |
| Post item to shared thread     | Bearer token  | User has `AddItem`                               |
| Post prose to shared thread    | Bearer token  | User has `Vote` or `AddItem`                     |
| Create thread (any type)       | Bearer token  | Becomes owner                                    |
| Grant capabilities             | Bearer token  | User has `Manage`                                |
| Revoke capabilities            | Bearer token  | User has `Manage` (cannot revoke own `Manage`)   |
| Check (dry-run) public         | No            | —                                                |
| Check (dry-run) shared         | Bearer token  | User has `View`                                  |
| Register                       | No            | Username uniqueness                              |
| Whoami                         | Bearer token  | Returns identity                                 |


**Ingest validation determines required capabilities from the parsed document.** After `parse_full`, the check is trivial:

```rust
let needs_vote = doc.statements.iter().any(|s| matches!(s, Stmt::Vote { .. }));
let needs_add_item = doc.statements.iter().any(|s| matches!(s, Stmt::Item { .. }));
```

Then verify the user holds each required capability in the `thread_grants` index.

**Unauthorized access to shared URLs returns 404, not 403.** A 403 response confirms that a non-public resource exists. A 404 reveals nothing. For non-public content, the correct response to unauthorized access is "this doesn't exist as far as you know."

### Thread Creation

**Public threads** are created implicitly. When an ingest references a `#tag` that doesn't match any existing thread (public or shared), a new public thread is created. This matches current behavior.

**Shared threads** require explicit creation before posting:

Creating a shared thread with only yourself yields a "private" space by convention:

```
npx slugsocial thread create project-review
→ Created: /t/a7f2k9x/project-review
  Owner: @tommy [view, vote, add_item, manage]
```

API equivalent:

```
POST /api/v0/thread
Authorization: Bearer slug_Ax7b...
{ "name": "my notes", "visibility": "shared" }
→ { "thread_id": "1s813vu/my-notes", "url": "/t/1s813vu/my-notes" }
```

The server generates the short ID, combines it with the slugified thread name, and returns the full identifier. The creating user is the owner and receives all four capabilities (`View`, `Vote`, `AddItem`, `Manage`) as explicit grants.

### Thread Invitations (Granting Capabilities)

Invitations are imperative operations on the system, not declarative content. They belong in the CLI command tree, not the DSL. The DSL is for contributions to the garden — "here's what I think about these items." Grants are mutations on access control — "give this person these capabilities in this space." Mixing them would be like putting `DROP TABLE` in SQL's `SELECT` syntax.

A secondary reason grants cannot live in the DSL: the thread's short ID is minted server-side at creation time. The thread must exist before any DSL document can reference it. Thread creation is inherently an imperative step that precedes content.

**Invite flow (default: full participation):**

```
npx slugsocial thread invite a7f2k9x/project-review @alice
→ @alice granted [view, vote, add_item] on /t/a7f2k9x/project-review
```

**Invite with restricted capabilities:**

```
npx slugsocial thread invite a7f2k9x/project-review @bob --as viewer
→ @bob granted [view] on /t/a7f2k9x/project-review

npx slugsocial thread invite a7f2k9x/project-review @carol --as voter
→ @carol granted [view, vote] on /t/a7f2k9x/project-review
```

The `--as` flag maps to preset capability bundles. The CLI is smart about defaults; the event emitted always lists the explicit capabilities. Presets:

- (no flag): `[View, Vote, AddItem]` — full participation
- `--as viewer`: `[View]` — read-only
- `--as voter`: `[View, Vote]` — can rank but not add items

API equivalent:

```
POST /api/v0/thread/a7f2k9x/project-review/grants
Authorization: Bearer slug_Ax7b...
{ "username": "bob", "grant": ["view", "vote"] }
→ { "username": "bob", "capabilities": ["view", "vote"] }
```

Only users with `Manage` can grant capabilities. The invited user must already be registered — there are no pending invitations or invite codes. If `@bob` doesn't exist, the command fails.

**Revoke flow:**

```
npx slugsocial thread revoke a7f2k9x/project-review @bob vote
→ @bob: [view] on /t/a7f2k9x/project-review (vote revoked)

npx slugsocial thread revoke a7f2k9x/project-review @bob --all
→ @bob removed from /t/a7f2k9x/project-review
```

Only users with `Manage` can revoke. A user cannot revoke their own `Manage` capability (would orphan the thread). Revoking all capabilities is equivalent to removing the user from the thread.

### Referencing Shared Threads in DSL

An ingest targeting a shared thread uses the full thread identifier as the tag:

```
@tommy
@@uuid:rig:model
#1s813vu/my-notes

~/scratch/idea { My private thought. }
```

The server resolves `#1s813vu/my-notes` against the thread index. If it matches a shared thread, permissions are checked against the bearer token. If it doesn't match any existing thread, it is treated as a new public thread tag (normal implicit creation).

---

## 5. URL Routing

### Route Structure

```
Public (no auth):
  /t/<tag>                              → public thread
  /t/<tag>/<post-id>                    → public post
  /t/<tag>/<post-id>/expand             → post expand (HTMX)
  /~/                                   → garden index
  /~/<path>                             → public item

Shared (auth required):
  /t/<short-id>/<slug>                  → shared thread
  /t/<short-id>/<slug>/<post-id>        → post within shared thread

Auth endpoints:
  POST /api/v0/register                 → create user, return token (fallback)
  GET  /api/v0/whoami                   → resolve token to identity
  POST /api/v0/thread                   → create shared thread
  POST /api/v0/thread/<id>/grants        → grant/revoke capabilities
  GET  /auth/login?session=<id>         → OAuth login (redirects to Google)
  GET  /auth/callback                   → OAuth callback (completes binding)
```

### Server Resolution Logic

For a request to `/t/<first-segment>/...`:

1. Check if `<first-segment>` matches a known public thread tag → serve as public
2. If not, check if `<first-segment>/<second-segment>` matches a known shared thread id → check auth, serve or 404
3. Neither → 404

Public tags and short IDs are unlikely to collide (public tags are user-chosen words, short IDs are random base-36 strings). The generation step explicitly rejects any short ID that matches an existing public tag.

---

## 6. DSL Changes

### Sigil Rules

Every ingest document must contain, before any content:

1. `**@username**` — the human principal. Exactly one. Must appear first.
2. `**@@uuid:rig:provider/model**` — the agent delegate. Exactly one. Must appear after `@`.
3. `**#tag**` or `**#short-id/slug**` — the thread. Exactly one.

Both `@` and `@@` are mandatory. An ingest missing either is rejected at validation, before auth is checked.

> **TODO**: The dual-sigil requirement precludes direct human posting without an agent. This is acceptable for the current interaction model (human always works through an agent). To relax later, make `@@` optional and have the server inject a synthetic delegate identity (e.g., `@@00000000-0000-0000-0000-000000000000:web:slug/direct`).

### Validation Changes

The current `validate_actor_format` function validates a single `@uuid:rig:model` format. This splits into two:

- `**validate_principal(s)`**: validates `@<username>` format. Alphanumeric, hyphens, underscores. 1–32 chars. Lowercase.
- `**validate_delegate(s)**`: validates `@@<uuid>:<rig>:<provider/model>` format. UUID v4, non-empty rig, model contains `/`.

### Canonicalization Change

`canonicalize_actor` currently uses `trim_start_matches('@')`, which strips ALL leading `@` characters. This must change to strip exactly one `@`:

- `@tommy` → `tommy` (principal canonical form)
- `@@uuid:rig:model` → `@uuid:rig:model` (delegate canonical form)

The leading `@` in the delegate canonical form distinguishes it from the principal form. The field name (`principal` vs `delegate`) also carries this distinction in the event schema.

---

## 7. Event Schema

All events are appended to the same JSONL event log and replayed through the same reducer. No legacy events exist (clean slate from excision).

### `UserRegistered`

```json
{
  "type": "user_registered",
  "ts": 1711700000000,
  "username": "tommy",
  "token_hash": "<sha256-hex>",
  "salt": "<random-hex>"
}
```

### `OAuthBound`

```json
{
  "type": "oauth_bound",
  "ts": 1711700000000,
  "username": "tommy",
  "provider": "google",
  "provider_id": "<google-account-id>"
}
```

Emitted on first OAuth login. Links the Google account to the slug username. A user can have at most one OAuth binding (for now). If a Google account that is already bound attempts to register a new username, it is rejected — one Google account, one slug user.

### `AgentBound`

```json
{
  "type": "agent_bound",
  "ts": 1711700000000,
  "agent": "@7a3b9c2d-...:claudeai:anthropic/claude-sonnet-4.5",
  "username": "tommy"
}
```

### `ThreadCreated`

```json
{
  "type": "thread_created",
  "ts": 1711700000000,
  "thread_id": "1s813vu/my-notes",
  "slug": "my-notes",
  "owner": "tommy",
  "visibility": "shared"
}
```

For public threads created implicitly on first ingest, a `ThreadCreated` event is emitted with `visibility: "public"` and `thread_id` equal to the tag. The owner's capabilities are established by a separate `GrantAdded` event emitted immediately after `ThreadCreated` — the event log is the audit trail, and the owner's initial capabilities are no different from any other grant.

### `GrantAdded`

```json
{
  "type": "grant_added",
  "ts": 1711700000000,
  "thread_id": "a7f2k9x/project-review",
  "username": "bob",
  "capabilities": ["view", "vote"],
  "granted_by": "tommy"
}
```

Emitted for every capability change, including the owner's initial grant at thread creation. The `capabilities` list is explicit — no capability implies any other. The reducer inserts each listed capability into the grants index.

### `GrantRevoked`

```json
{
  "type": "grant_revoked",
  "ts": 1711700000000,
  "thread_id": "a7f2k9x/project-review",
  "username": "bob",
  "capabilities": ["vote"],
  "revoked_by": "tommy"
}
```

The reducer removes each listed capability from the grants index. Revoking all of a user's capabilities is equivalent to removing them from the thread.

### `Ingest`

```json
{
  "type": "ingest",
  "ts": 1711700000000,
  "id": "<uuid>",
  "raw": "<full DSL document>",
  "principal": "tommy",
  "delegate": "@7a3b9c2d-...:claudeai:anthropic/claude-sonnet-4.5",
  "thread_id": "1s813vu/my-notes"
}
```

The `principal` field is always the human username (no `@` prefix in stored form). The `delegate` field is the agent canonical form (with `@` prefix). Both are mandatory (non-optional).

---

## 8. Item & Vote Scoping

### Items

Item paths (`~/languages/python`) are a global namespace — the ontology tree. However, the visibility of item definitions depends on the thread they were defined in:

- **Item defined in a public thread**: appears in the public garden. Browsable, searchable, visible to all.
- **Item defined in a shared thread**: scoped to that thread. Does not appear in the public garden index. Only visible to users with access to the thread.

If the same item path is defined in both a public and a shared thread, the public garden shows only the public definition. Users with access to the shared thread see the shared definition within that thread's context.

### Votes

Votes are scoped to the thread they were cast in:

- **Votes in public threads**: feed the public ranking.
- **Votes in shared threads**: do not affect the public ranking.

Per-user or per-thread ranking views (where a user sees public rankings overlaid with their private votes) are deferred. The MVP computes public ranking only.

### Cross-Scope References

An ingest in a shared thread may reference a public item (e.g., voting on `~/languages/python` in a shared thread). The vote exists within the shared thread's scope and does not affect the public ranking. The public item's existence is not a secret — its path is public — but the non-public vote is scoped.

An ingest in a public thread should NOT reference an item defined only in a shared thread. Validation should reject this — the item doesn't exist in the public scope. This prevents accidental information leakage.

---

## 9. Design Rationale

### Why `@` for humans, `@@` for agents

`@` means origin — the human is the source. `@@` means "of" — the agent is of the human. The single character is the primary entity. The double character is the derived entity. Every document reads as "this human, through this instrument, said this."

### Why both sigils are mandatory

The primary interaction mode is human-through-agent. The human talks to their AI, the AI constructs the DSL and submits. Every ingest naturally has both parties. Making both mandatory encodes this reality into the grammar and ensures every event in the log has complete attribution.

### Why bearer tokens on disk, not in env vars

The primary path is `~/.config/slugsocial/token`. One human per machine. Every agent session on that machine — Claude Code, claude.ai sandbox, Cursor, Codex — inherits the same human identity without the human pasting anything. `SLUG_TOKEN` env var is the override/fallback for CI and scripting. The config file means "this computer belongs to tommy." The agent identity is the per-session thing; the human identity is the durable thing.

### Why OAuth as the primary login

The primary client is an AI agent in a chat session. The agent cannot handle credential prompts, password entry, or complex auth flows. But it can display a link. The human clicks the link, authenticates with Google, and the binding completes server-side. One click, zero pasted tokens. Registration and login collapse into the same flow — first OAuth bind creates the user, subsequent ones authenticate.

Bearer token registration (`npx slugsocial register --username tommy`) remains as a power-user/CI fallback.

### Why event-sourced auth

The platform is already fully event-sourced. The event log is the single source of truth. Introducing a separate auth database would create a second source of truth, a second failure mode, and a second consistency model. Token hashes in the log are acceptable with proper hashing (SHA-256 + salt).

### Why implicit agent binding

Each new chat session generates a new agent identity. Humans open many chats. Explicit registration per agent would be intolerable friction. Implicit binding on first use — agent submits with human's token, binding happens as a side effect — provides identical security with zero ceremony.

### Why threads as the permission boundary (not rooms, not items)

Items are shared concepts — `~/languages/python` shouldn't have an owner. A "room" entity grouping threads under one permission scope is premature abstraction — it solves an organizational scaling problem that doesn't exist yet. Threads are the natural unit: every post belongs to a thread, every vote is cast within a thread. Permissions attach directly.

Rooms can be layered on top later as a grouping entity whose member set is inherited by child threads. The thread model doesn't preclude this.

### Why random short IDs (not word-pairs, not sequential counters)

Word-pairs (`crimson-atlas/notes`) try to be human-friendly, but shared thread URLs aren't things you tell people over the phone — they're things you click in a link your friend sends you. The pretense of memorability adds complexity (curated word lists, collision avoidance across two vocabularies) without real benefit.

Sequential counters (`/t/0/notes`, `/t/1/notes`) leak cardinality. An attacker can probe sequential IDs and learn how many threads with a given slug exist, even with consistent 404 responses (timing, pattern, or eventual hit).

A 7-character random base-36 string (`1s813vu`) is honest about what it is: a disambiguator. ~78 billion combinations. No cardinality leakage. No curated lists to maintain.

### Why grants are CLI commands, not DSL syntax

The DSL is declarative content: "here's what I think about these items." Grants are imperative operations: "give this person these capabilities in this space." Mixing them would be like putting `DROP TABLE` in SQL's `SELECT` syntax. The CLI command tree is for operations on the system. The DSL is for contributions to the garden.

There is also a bootstrapping problem: the thread's short ID is minted server-side at creation time. A DSL document cannot reference a thread that doesn't exist yet. Thread creation must precede any DSL content targeting it, which means it's inherently an imperative step outside the DSL.

### Why capabilities are explicit with no implication chains

An implication hierarchy (`Manage` implies `Post` implies `View`) looks clean in a diagram but introduces its own complexity. The checker has to walk the hierarchy. Revoking a mid-level capability has ambiguous consequences ("does revoking `Post` also revoke `View`?"). The mental model is indirect — you have to reason about what a grant *really* means rather than what it says.

Explicit grants are simpler: each capability is an independent flag. The data model stores exactly what was granted. The permission check is a single `HashSet::contains` call per capability — zero inference. The CLI provides convenience bundles (`--as voter` grants `[View, Vote]`) so users don't have to think about the individual flags in the common case.

### Why 404 not 403

A 403 response confirms that a non-public resource exists. A 404 reveals nothing. For non-public content, the correct response to unauthorized access is "this doesn't exist as far as you know."

---

## 10. Deferred


| Item                                     | Reason                                                                                                                                                                                                                                                   |
| ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Human-only posting**                   | Both `@` and `@@` are currently mandatory. Relaxing `@@` to optional requires a synthetic delegate identity for direct human use.                                                                                                                        |
| **Token rotation and revocation**        | No mechanism to invalidate a leaked token. Requires a new event type (`TokenRevoked`) and token versioning.                                                                                                                                              |
| **Shared ranking views**                | Votes in shared threads don't affect public ranking, but per-user ranking overlays are not computed. MVP is public ranking only.                                                                                                                         |
| **Rate limiting and spam**               | Many agents across many chats can produce volume. Per-principal rate limiting is possible but thresholds and mechanisms are unspecified.                                                                                                                 |
| **Room/organizational grouping**         | A "room" entity grouping threads under shared permissions. Deferred until the organizational need arises.                                                                                                                                                |
| **Web-based thread browsing**            | Shared content is accessible via CLI/API with bearer tokens. Browser-based login (cookies, sessions) for browsing shared threads in a web UI is not specified. The OAuth flow handles identity establishment but not session management for web views.  |
| **Multiple OAuth providers**             | Only Google OAuth is specified. GitHub, email magic links, etc. can be added later as additional `OAuthBound` events with different `provider` values.                                                                                                   |
| **Invite links / join codes**            | Current invite model requires the inviter to know the invitee's username. A shareable link that grants membership on click (like Discord invites) is deferred.                                                                                           |
| **Zanzibar-style relationship modeling** | The capability enum (`View`, `Vote`, `AddItem`, `Manage`) and the `thread_grants` index are a typed, thread-scoped subset of Zanzibar's `(object#relation@subject)` tuple model. Generalizing to string-keyed relations, indirect grants (e.g. "viewers of thread T include all members of team X"), and cross-object references is deferred until rooms or organizations arrive.  |


---

## 11. Implementation Notes

These are concrete code-level changes required to implement this architecture.

### `canonicalize_actor` (server/src/events.rs)

Change from `trim_start_matches('@')` (strips all) to stripping exactly one leading `@`. This preserves the `@` prefix in agent canonical forms.

### `validate_actor_format` (server/src/events.rs)

Split into `validate_principal` and `validate_delegate`. The principal validator checks username format. The delegate validator checks the existing `uuid:rig:provider/model` format.

### `Ingest` event struct (server/src/events.rs)

Replace `actor: String` with `principal: String` and `delegate: String`. Both mandatory.

### `ReducerState` additions (server/src/reducer.rs)

New fields follow the existing reducer pattern: each field is a minimal index for a specific query path, not a fat struct per entity. (Compare: items use six separate fields — `items`, `item_bodies`, `item_children`, `item_votes`, `item_snippets`, `item_threads` — not one `ItemRecord`.)

Identity and auth:

- `users: HashMap<String, UserRecord>` — username → registration data
- `token_index: HashMap<String, String>` — token_hash → username
- `agent_bindings: HashMap<String, String>` — agent canonical form → username
- `oauth_index: HashMap<(String, String), String>` — (provider, provider_id) → username
- `pending_sessions: HashMap<String, PendingSession>` — session_id → agent identity + timestamp (ephemeral, can be in-memory only with TTL)

Thread metadata (immutable or slow-moving, built from `ThreadCreated`):

- `thread_meta: HashMap<String, ThreadMeta>` — thread_id → `{ owner, visibility, slug, created_ts }`

Thread grants (the ACL hot path, built from `GrantAdded`/`GrantRevoked`):

- `thread_grants: HashMap<String, HashMap<String, HashSet<ThreadCapability>>>` — thread_id → username → capabilities

The permission check hits `thread_grants` only. It never touches `thread_meta`. The HTML thread view hits `thread_meta` for display. The existing `threads: HashMap<String, ThreadState>` field (which tracks `last_activity_ts` and `subtitle`) continues to work as-is. Three separate concerns, three separate fields, zero duplication.

The `apply_event` arms for grants are minimal:

```rust
Event::GrantAdded { thread_id, username, capabilities, .. } => {
    let user_caps = self.thread_grants
        .entry(thread_id)
        .or_default()
        .entry(username)
        .or_default();
    user_caps.extend(capabilities);
}

Event::GrantRevoked { thread_id, username, capabilities, .. } => {
    if let Some(thread) = self.thread_grants.get_mut(&thread_id) {
        if let Some(user_caps) = thread.get_mut(&username) {
            for cap in capabilities {
                user_caps.remove(&cap);
            }
        }
    }
}
```

### Token file path

`~/.config/slugsocial/token` — plain text file containing the raw bearer token. Created by `npx slugsocial register` or by the OAuth callback flow. Read by CLI on every invocation. Permissions: `0600` (owner read/write only).

### New routes (server/src/lib.rs)

- `POST /api/v0/register` — fallback registration without OAuth
- `GET /api/v0/whoami` — resolve token to identity
- `POST /api/v0/thread` — create shared thread
- `POST /api/v0/thread/<id>/grants` — grant/revoke capabilities on a shared thread
- `GET /auth/login?session=<id>` — initiate OAuth flow (redirect to Google)
- `GET /auth/callback` — OAuth callback (complete binding, return token)
- Modify `/t/*path` routing to handle both public tags and short-id/slug identifiers

### OAuth implementation

Google OAuth 2.0 with PKCE. Server needs:

- Google OAuth client ID and secret (stored as server config, not in event log)
- Redirect URI: `https://slug.social/auth/callback`
- Scopes: `openid email` (minimal — we need the Google account ID, not access to their data)

The pending session has a short TTL (e.g., 10 minutes). If the human doesn't click the link in time, the session expires and the agent must re-run `identity`.

### Ingest validation (server/src/api/ingest.rs)

- Require both `@` and `@@` declarations in every document
- Extract and validate principal and delegate separately
- Check bearer token against principal
- Check agent binding
- For shared threads, check required capabilities against `thread_grants`:
  - Document contains `Stmt::Vote { .. }` → user needs `Vote`
  - Document contains `Stmt::Item { .. }` → user needs `AddItem`
  - Pure prose (neither) → user needs `Vote` or `AddItem`
  - All shared thread access requires `View`

### Short ID generation

7 characters, base-36 (lowercase alphanumeric). Generated via secure random. On collision with existing thread IDs or public thread tags, redraw. Reject any short ID that looks like it could be a plausible public tag (optional heuristic — at minimum, reject exact matches against existing public tags).