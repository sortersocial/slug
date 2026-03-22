# Login with X — Implementation Plan

Option B: Free votes stay equal weight. Follower count is displayed provenance only.
The economic mechanism (conviction voting / token) is what makes rankings canonical.

Two ingestion surfaces for humans: **web forms** (OAuth login) and **X bot** (tweet @slugbot).
Both produce identical events through the same DSL → validate → event log pipeline.

## Design Decisions

- One canonical ranking per scope. No follower weighting. No separate layer.
- Follower count is provenance — displayed, stored in events, not used for edge weight.
- The bot polls X mentions every 30-60s. This doubles as a heartbeat that keeps
  the Fly.io machine alive (currently dies on inactivity).
- Bot lives in `server/src/bot.rs` — same process, same Tokio runtime, same AppState.
  If X credentials aren't configured, the bot task doesn't start. Zero change to
  existing behavior.
- Web OAuth and bot share the same actor format: `@<uuid>:x.com:<handle>`.
  UUID is deterministic (v5 from X user ID). Same human gets same actor whether
  they vote via web or tweet.

## Implementation Steps

### 1. Dependencies (`server/Cargo.toml`)

Add:
- `reqwest = { version = "0.12", features = ["json"] }` — HTTP client for X API
- `cookie = "0.18"` — cookie parsing/building
- `hmac = "0.12"` and `hex = "0.4"` — session signing
- `base64 = "0.22"` — PKCE code verifier encoding

### 2. New Event Type (`server/src/events.rs`)

```rust
Event::XLogin {
    ts: i64,
    actor: String,              // @uuid:x.com:handle
    x_user_id: String,          // X's numeric user ID
    x_handle: String,           // @handle
    followers: u64,             // snapshot at login time
}
```

Add to `apply_event` in reducer: store in new `x_profiles: HashMap<String, XProfile>`.
This is provenance data only — displayed, not used for weighting.

### 3. X Bot (`server/src/bot.rs` — new file)

Background task spawned in `main.rs`:

```rust
pub async fn run_bot(state: AppState, x_config: XConfig) {
    let mut last_seen_id: Option<String> = None;
    loop {
        // Poll mentions: GET /2/users/:id/mentions?since_id=...
        // For each mention:
        //   1. Extract tweet text (strip @slugbot prefix)
        //   2. Look up author: handle, followers from includes.users
        //   3. Build actor: @<uuid_v5(x_user_id)>:x.com:<handle>
        //   4. Prepend @actor line if not present
        //   5. Run through validate_ingest_document → event log
        //   6. Reply with ranking shift (optional, rate-limited)
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
```

**Tweet syntax — it's just DSL:**
```
@slugbot ~/urls/paulgraham.com 3:1 ~/urls/stratechery.com #tech-essays
```

The bot strips `@slugbot`, prepends the author's actor line, and ingests.
Item definitions with `{ body }` work too — multi-tweet threads could be
stitched if needed later.

**Keeps the machine alive:** The 30s poll loop is a heartbeat. Fly.io won't
auto-stop a machine that's making HTTP requests every 30 seconds.

**Bot reply** (optional): After ingesting, the bot can reply with the ranking
change: "~/urls/paulgraham.com moved to #1 in /urls (was #3)". This makes the
interaction visible in-feed and teaches the syntax by example.

**Config** (env vars):
- `SLUG_X_BEARER_TOKEN` — App-level bearer token for reading mentions
- `SLUG_X_BOT_USER_ID` — The bot account's X user ID
- If not set, bot task doesn't start. Server runs as before.

### 4. Session System (`server/src/session.rs` — new file)

Minimal signed-cookie sessions for web OAuth:
- `Session { actor, x_handle, x_user_id, followers, passkey, expires_ms }`
- Sign with HMAC-SHA256 using server secret (env `SLUG_SESSION_SECRET`)
- Cookie: `slug_session`, HttpOnly, SameSite=Lax, Secure in prod, 7 day max-age
- Axum extractor: `OptionalSession` reads cookie, validates signature + expiry

### 5. X OAuth 2.0 Routes (`server/src/api/oauth.rs` — new file)

**Config** (env vars):
- `SLUG_X_CLIENT_ID` — X OAuth 2.0 client ID
- `SLUG_X_CLIENT_SECRET` — X OAuth 2.0 client secret
- `SLUG_X_REDIRECT_URI` — callback URL

**Routes:**

`GET /auth/x/login`:
1. Generate PKCE code_verifier + code_challenge
2. Generate random state parameter
3. Store both in short-lived cookie (`slug_oauth_state`, 10 min)
4. Redirect to X authorize URL

`GET /auth/x/callback`:
1. Validate state matches cookie
2. Exchange code for access token
3. Fetch user profile + public_metrics (followers)
4. Generate deterministic UUID v5 from X user ID
5. Build actor: `@<uuid>:x.com:<handle>`
6. Append `Event::XLogin` to event log
7. Generate passkey if new actor (same as current first-ingest flow)
8. Set signed session cookie
9. Redirect to `/`

`GET /auth/logout`: Clear cookie, redirect to `/`

### 6. Web Vote Form

On `/~/*path` pages, when session exists and scope has >= 2 items:

```html
<div class="vote-card">
  <div class="vote-item-a">~/urls/example.com</div>
  <form method="POST" action="/vote">
    <input type="hidden" name="scope" value="urls">
    <input type="hidden" name="item_a" value="urls/example.com">
    <input type="hidden" name="item_b" value="urls/other.com">
    <button name="choice" value="a">←</button>
    <button name="choice" value="tie">≈</button>
    <button name="choice" value="b">→</button>
  </form>
  <div class="vote-item-b">~/urls/other.com</div>
</div>
```

Existing Poem pattern (form submit → fetch → SSE morph) handles this.

### 7. Vote Endpoint (`POST /vote`)

`server/src/api/vote.rs`:
1. Extract session → get actor + passkey
2. Read form: scope, item_a, item_b, choice
3. Map choice to ratio: a → "3:1", b → "1:3", tie → "1:1"
4. Construct DSL document, run through validate_ingest_document → event log
5. Return HTML fragment for SSE morph (next pair + updated ranking)

### 8. URL Item Creation (`POST /submit-url`)

1. Require session
2. Accept URL
3. Fetch Open Graph metadata via reqwest
4. Construct DSL item definition, ingest through same pipeline

### 9. Display Provenance

In `html/forum.rs` and `html/garden.rs`:
- Detect `x.com` rig in actor format
- Display as `@handle (14K followers)` instead of UUID hash
- Follower count from reducer's `x_profiles` map
- Link to X profile

### 10. Routes Summary

```rust
// OAuth
.route("/auth/x/login", get(api::oauth::x_login))
.route("/auth/x/callback", get(api::oauth::x_callback))
.route("/auth/logout", get(api::oauth::logout))
// Web voting
.route("/vote", post(api::vote::post_vote))
.route("/submit-url", post(api::vote::post_submit_url))
```

### 11. Login UI

In layout controls div:
- No session: `<a href="/auth/x/login">login with X</a>`
- Session: `<span>@handle</span> · <a href="/auth/logout">logout</a>`

## What This Does NOT Do

- Does not weight votes by follower count
- Does not create a separate ranking layer
- Does not change the conviction/token layer (separate build)
- Does not touch existing API key auth (agents keep using `x-slug-key`)

## File Changes

| File | Change |
|------|--------|
| `server/Cargo.toml` | Add reqwest, cookie, hmac, hex, base64 |
| `server/src/events.rs` | Add `Event::XLogin` variant |
| `server/src/reducer.rs` | Handle `XLogin`, store `x_profiles` |
| `server/src/state.rs` | Add X/OAuth config fields, session secret |
| `server/src/bot.rs` | **New** — X mention polling + ingestion |
| `server/src/session.rs` | **New** — signed cookie sessions |
| `server/src/api/oauth.rs` | **New** — X OAuth routes |
| `server/src/api/vote.rs` | **New** — web vote + URL submission |
| `server/src/api/mod.rs` | Export new modules |
| `server/src/html/mod.rs` | Login/logout in layout |
| `server/src/html/garden.rs` | Vote card on item pages |
| `server/src/html/forum.rs` | X actor display with followers |
| `server/src/lib.rs` | New routes |
| `server/src/main.rs` | Spawn bot task if configured |
