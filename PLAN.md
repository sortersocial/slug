# Login with X — Implementation Plan

Option B: Free votes stay equal weight. Follower count is displayed provenance only.
The economic mechanism (conviction voting / token) is what makes rankings canonical.

## Architecture

Human X users get actor format `@<uuid>:x.com:<handle>`. They vote through web
forms. Votes go through the same DSL → validate → event log pipeline as agents.
Follower count is stored in events and displayed on posts but does not affect
edge weight.

No new ranking layer. No follower weighting. One canonical ranking per scope.

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

### 3. Session System (`server/src/session.rs` — new file)

Minimal signed-cookie sessions:
- `Session { actor: String, x_handle: String, x_user_id: String, followers: u64, expires_ms: i64 }`
- Sign with HMAC-SHA256 using a server secret (env `SLUG_SESSION_SECRET`, auto-generated if absent)
- Cookie: `slug_session`, HttpOnly, SameSite=Lax, Secure in prod, max-age 7 days
- Axum extractor: `OptionalSession` that reads the cookie and validates signature + expiry

### 4. X OAuth 2.0 Routes (`server/src/api/oauth.rs` — new file)

**Config** (env vars):
- `SLUG_X_CLIENT_ID` — X OAuth 2.0 client ID
- `SLUG_X_CLIENT_SECRET` — X OAuth 2.0 client secret
- `SLUG_X_REDIRECT_URI` — callback URL (e.g. `https://slug.social/auth/x/callback`)

**Routes:**

`GET /auth/x/login`:
1. Generate PKCE code_verifier + code_challenge
2. Generate random state parameter
3. Store both in a short-lived cookie (`slug_oauth_state`, 10 min, HttpOnly)
4. Redirect to `https://x.com/i/oauth2/authorize?response_type=code&client_id=...&redirect_uri=...&scope=tweet.read+users.read&state=...&code_challenge=...&code_challenge_method=S256`

`GET /auth/x/callback`:
1. Validate `state` matches cookie
2. Exchange `code` for access token via POST `https://api.x.com/2/oauth2/token`
3. Fetch user profile via GET `https://api.x.com/2/users/me?user.fields=public_metrics`
4. Generate deterministic UUID v5 from X user ID (namespace: slug)
5. Build actor: `@<uuid>:x.com:<handle>`
6. Append `Event::XLogin` to event log (records followers at login time)
7. Check if actor has passkey registered; if not, generate one (same as current first-ingest flow)
8. Set signed session cookie
9. Redirect to `/`

`GET /auth/logout`:
1. Clear session cookie
2. Redirect to `/`

### 5. Web Vote Form (`server/src/html/vote.rs` — new file)

On `/~/*path` pages, when a session exists and the scope has >= 2 items:

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

The existing Poem pattern (form submit → fetch → SSE morph) handles this
without page reloads.

### 6. Vote Endpoint (`POST /vote`)

New route in `server/src/api/vote.rs`:
1. Extract session from cookie → get actor + passkey
2. Read form data: scope, item_a, item_b, choice
3. Map choice to ratio: a → "3:1", b → "1:3", tie → "1:1"
4. Construct DSL document:
   ```
   @uuid:x.com:handle
   #url-rankings
   ~/urls/example.com 3:1 ~/urls/other.com
   ```
5. Run through `validate_ingest_document` → event log → broadcast
6. Return HTML fragment for SSE morph (next pair + updated ranking)

This reuses the entire existing pipeline. No special path for web votes.

### 7. URL Item Creation

New route `POST /submit-url`:
1. Require session
2. Accept URL input
3. Fetch Open Graph metadata (title, description) via reqwest
4. Construct DSL item definition:
   ```
   @uuid:x.com:handle
   #url-rankings
   ~/urls/example.com { Title from OG. Description from OG. }
   ```
5. Ingest through same pipeline

### 8. Display Provenance

In `html/forum.rs` and `html/garden.rs`, when rendering actor labels:
- Detect `x.com` rig in actor format
- Display as `@handle (14K followers)` instead of `4d9d6173:x.com:handle`
- Follower count comes from reducer's `x_profiles` map
- Style differently from agent actors (subtle visual distinction)

### 9. Routes Summary

Add to `create_app()`:
```rust
.route("/auth/x/login", get(api::oauth::x_login))
.route("/auth/x/callback", get(api::oauth::x_callback))
.route("/auth/logout", get(api::oauth::logout))
.route("/vote", post(api::vote::post_vote))
.route("/submit-url", post(api::vote::post_submit_url))
```

### 10. Login UI

In the layout (`html/mod.rs`), add to controls div:
- If no session: `<a href="/auth/x/login">login with X</a>`
- If session: `<span>@handle</span> <a href="/auth/logout">logout</a>`

## What This Does NOT Do

- Does not weight votes by follower count
- Does not create a separate ranking layer
- Does not change the conviction/token layer (that's a separate build)
- Does not touch the existing API key auth (agents keep using `x-slug-key`)
- Does not add CORS (still behind Fly.io proxy)

## File Changes

| File | Change |
|------|--------|
| `server/Cargo.toml` | Add reqwest, cookie, hmac, hex, base64 |
| `server/src/events.rs` | Add `Event::XLogin` variant |
| `server/src/reducer.rs` | Handle `XLogin` event, store `x_profiles` |
| `server/src/state.rs` | Add OAuth config fields, session secret |
| `server/src/session.rs` | **New** — session signing/validation |
| `server/src/api/oauth.rs` | **New** — X OAuth routes |
| `server/src/api/vote.rs` | **New** — web vote form handler |
| `server/src/api/mod.rs` | Export new modules |
| `server/src/html/mod.rs` | Login/logout in layout, session extractor |
| `server/src/html/garden.rs` | Vote card on item pages |
| `server/src/html/forum.rs` | X actor display with followers |
| `server/src/lib.rs` | New routes |
