# X Bot — Implementation Plan

Option B: Free votes stay equal weight. Follower count is displayed provenance only.
The economic mechanism (conviction voting / token) is what makes rankings canonical.

One human ingestion surface: **X bot**. Tweet `@slugbot`, bot ingests through
the same DSL pipeline as agents. Web stays read-only. No OAuth, no sessions,
no cookies, no web forms.

## Design Decisions

- One canonical ranking per scope. No follower weighting. No separate layer.
- Follower count is provenance — displayed, stored in events, not edge weight.
- Bot polls X mentions every 30s. This doubles as a heartbeat that keeps
  the Fly.io machine alive (currently dies on inactivity).
- Bot lives in `server/src/bot.rs` — same process, same Tokio runtime, same
  AppState. If X credentials aren't configured, bot doesn't start. Zero change
  to existing behavior.
- Actor format: `@<uuid_v5(x_user_id)>:x.com:<handle>`. Deterministic UUID
  so the same human always gets the same actor.

## Implementation

### 1. Dependency (`server/Cargo.toml`)

Move reqwest from dev-dependencies to dependencies:
- `reqwest = { version = "0.12", features = ["json"] }`

### 2. Event Type (`server/src/events.rs`)

```rust
Event::XMention {
    ts: i64,
    actor: String,          // @uuid:x.com:handle
    x_user_id: String,      // X numeric user ID
    x_handle: String,       // handle without @
    followers: u64,         // snapshot at mention time
    tweet_id: String,       // for dedup + reply
}
```

### 3. Reducer (`server/src/reducer.rs`)

New field on ReducerState:
```rust
pub x_profiles: HashMap<String, XProfile>,  // actor → profile
```

```rust
pub struct XProfile {
    pub x_handle: String,
    pub followers: u64,
}
```

`apply_event` for XMention: upsert x_profiles (latest followers wins).

### 4. Config (`server/src/state.rs`)

New optional fields on AppConfig:
```rust
pub x_bearer_token: Option<String>,   // SLUG_X_BEARER_TOKEN
pub x_bot_user_id: Option<String>,    // SLUG_X_BOT_USER_ID
```

### 5. Bot (`server/src/bot.rs` — new file)

```rust
pub async fn run_bot(state: AppState) {
    // Extract config, return early if not configured
    let mut last_seen_id: Option<String> = None;
    loop {
        // GET /2/users/:bot_id/mentions
        //   ?since_id=...&tweet.fields=author_id,created_at
        //   &expansions=author_id&user.fields=public_metrics,username
        //
        // For each mention:
        //   1. Strip @bothandle from tweet text
        //   2. Look up author: handle, followers from includes.users
        //   3. Build actor: @<uuid_v5(x_user_id)>:x.com:<handle>
        //   4. Prepend @actor line to tweet text
        //   5. Append Event::XMention to event log, apply to reducer
        //   6. Run tweet text through validate_ingest_document → event log
        //   7. Broadcast SSE
        //   8. Optionally reply with ranking shift
        //
        // Persist last_seen_id to data dir for restart continuity
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
```

**Tweet syntax — it's just DSL:**
```
@slugbot ~/urls/paulgraham.com 3:1 ~/urls/stratechery.com #tech-essays
```

### 6. Actor Display (`server/src/html/forum.rs`)

Detect `x.com` in actor rig field. Display as `@handle (14K)` with link
to X profile instead of truncated UUID.

### 7. Startup (`server/src/main.rs`)

After server starts, if X config is present:
```rust
tokio::spawn(bot::run_bot(state.clone()));
```

## File Changes

| File | Change |
|------|--------|
| `server/Cargo.toml` | Add reqwest to dependencies |
| `server/src/events.rs` | Add `Event::XMention` variant |
| `server/src/reducer.rs` | Handle `XMention`, add `x_profiles` |
| `server/src/state.rs` | Add X bot config fields |
| `server/src/bot.rs` | **New** — mention poll loop + ingestion |
| `server/src/html/forum.rs` | X actor display with followers |
| `server/src/html/mod.rs` | Helper for X actor label |
| `server/src/main.rs` | Spawn bot task if configured |
| `server/src/lib.rs` | Add `pub mod bot;` |
