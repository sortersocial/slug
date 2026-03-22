//! X (Twitter) mention bot.
//!
//! Polls the authenticated user's mentions on an interval. Each mention is
//! parsed as DSL, attributed to the tweeter's deterministic actor identity,
//! and ingested through the standard event log pipeline.
//!
//! The poll loop also acts as a heartbeat that keeps the Fly.io machine alive.

use std::time::Duration;

use serde::Deserialize;

use crate::{
    api::{now_ms, sha256_hex, validate_ingest_document},
    events::{canonicalize_actor, Event, Ingest},
    state::{AppState, XBotConfig},
};

/// UUID v5 namespace for X user IDs → deterministic actor UUIDs.
const X_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x14, 0x9d, 0xad, 0x11, 0xd1,
    0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8,
]);

/// Build a deterministic actor string for an X user.
/// Format: `<uuid_v5(x_user_id)>:x.com:<handle>` (no leading @, already canonical).
fn x_actor(x_user_id: &str, handle: &str) -> String {
    let uuid = uuid::Uuid::new_v5(&X_NAMESPACE, x_user_id.as_bytes());
    format!("{}:x.com:{}", uuid, handle.to_lowercase())
}

/// Path in data_dir where we persist the last-seen mention ID across restarts.
fn cursor_path(data_dir: &str) -> String {
    format!("{}/x_bot_cursor.txt", data_dir)
}

fn load_cursor(data_dir: &str) -> Option<String> {
    std::fs::read_to_string(cursor_path(data_dir))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn save_cursor(data_dir: &str, id: &str) {
    let _ = std::fs::write(cursor_path(data_dir), id);
}

// --- X API response types ---

#[derive(Debug, Deserialize)]
struct MentionsResponse {
    data: Option<Vec<Tweet>>,
    includes: Option<Includes>,
    meta: Option<Meta>,
}

#[derive(Debug, Deserialize)]
struct Tweet {
    id: String,
    text: String,
    author_id: String,
}

#[derive(Debug, Deserialize)]
struct Includes {
    users: Option<Vec<XUser>>,
}

#[derive(Debug, Deserialize)]
struct XUser {
    id: String,
    username: String,
    public_metrics: Option<PublicMetrics>,
}

#[derive(Debug, Deserialize)]
struct PublicMetrics {
    followers_count: u64,
}

#[derive(Debug, Deserialize)]
struct Meta {
    newest_id: Option<String>,
}

/// Main bot loop. Runs until the process shuts down.
pub async fn run_bot(state: AppState, x_cfg: XBotConfig) {
    let client = reqwest::Client::new();
    let mut cursor = load_cursor(&state.cfg.data_dir);

    // On first boot with no cursor, check reducer for already-seen tweets.
    if cursor.is_none() {
        let reduced = state.reduced.read().await;
        if !reduced.seen_tweet_ids.is_empty() {
            // We don't know the newest ID, but we can avoid re-processing.
            // The API will return recent mentions; we'll skip any with known tweet IDs.
            tracing::info!("bot: no cursor file, {} tweet IDs already in reducer", reduced.seen_tweet_ids.len());
        }
    }

    let interval = Duration::from_secs(x_cfg.poll_interval_secs);
    tracing::info!(
        bot_user_id = %x_cfg.bot_user_id,
        poll_secs = x_cfg.poll_interval_secs,
        "x bot started"
    );

    loop {
        match poll_and_ingest(&state, &x_cfg, &client, &mut cursor).await {
            Ok(n) if n > 0 => tracing::info!(ingested = n, "x bot poll"),
            Ok(_) => tracing::debug!("x bot poll: no new mentions"),
            Err(e) => tracing::warn!("x bot poll error: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}

async fn poll_and_ingest(
    state: &AppState,
    x_cfg: &XBotConfig,
    client: &reqwest::Client,
    cursor: &mut Option<String>,
) -> Result<usize, String> {
    let mut url = format!(
        "https://api.x.com/2/users/{}/mentions\
         ?tweet.fields=author_id,created_at\
         &expansions=author_id\
         &user.fields=public_metrics,username\
         &max_results=100",
        x_cfg.bot_user_id
    );
    if let Some(ref since) = cursor {
        url.push_str(&format!("&since_id={}", since));
    }

    let resp = client
        .get(&url)
        .bearer_auth(&x_cfg.bearer_token)
        .send()
        .await
        .map_err(|e| format!("http error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("X API {status}: {body}"));
    }

    let mentions: MentionsResponse = resp
        .json()
        .await
        .map_err(|e| format!("json parse error: {e}"))?;

    let tweets = match mentions.data {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(0),
    };

    // Update cursor to newest tweet ID.
    if let Some(ref meta) = mentions.meta {
        if let Some(ref newest) = meta.newest_id {
            *cursor = Some(newest.clone());
            save_cursor(&state.cfg.data_dir, newest);
        }
    }

    // Build user lookup from includes.
    let users: std::collections::HashMap<String, &XUser> = mentions
        .includes
        .as_ref()
        .and_then(|inc| inc.users.as_ref())
        .map(|users| users.iter().map(|u| (u.id.clone(), u)).collect())
        .unwrap_or_default();

    let mut ingested = 0;

    // Process oldest-first so cursor advances monotonically.
    for tweet in tweets.iter().rev() {
        // Dedup: skip tweets already in the reducer.
        {
            let reduced = state.reduced.read().await;
            if reduced.seen_tweet_ids.contains(&tweet.id) {
                continue;
            }
        }

        let user = match users.get(&tweet.author_id) {
            Some(u) => u,
            None => {
                tracing::warn!(tweet_id = %tweet.id, "no user data for author_id {}", tweet.author_id);
                continue;
            }
        };

        let handle = user.username.to_lowercase();
        let followers = user
            .public_metrics
            .as_ref()
            .map(|m| m.followers_count)
            .unwrap_or(0);
        let actor = x_actor(&tweet.author_id, &handle);

        // Strip the @bot mention from the tweet text.
        let text = strip_bot_mention(&tweet.text, &x_cfg.bot_handle);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }

        // Prepend the actor declaration so the DSL parser sees it.
        let dsl_text = format!("@{actor}\n{text}");

        let ts = now_ms();

        // 1. Record XMention event (provenance).
        let mention_event = Event::XMention {
            ts,
            actor: actor.clone(),
            x_user_id: tweet.author_id.clone(),
            x_handle: handle.clone(),
            followers,
            tweet_id: tweet.id.clone(),
        };
        if let Err(e) = state.event_log.append(&mention_event).await {
            tracing::error!("failed to append XMention event: {e}");
            continue;
        }
        {
            let mut reduced = state.reduced.write().await;
            reduced.apply_event(mention_event);
        }

        // 2. Validate and ingest the DSL document.
        let reduced = state.reduced.read().await;
        let validated = match validate_ingest_document(
            &reduced,
            &dsl_text,
            "bot ingest requires @actor (auto-prepended)",
        ) {
            Ok(v) => v,
            Err((_status, msg, hint)) => {
                tracing::warn!(
                    tweet_id = %tweet.id,
                    handle = %handle,
                    "skipping invalid mention: {msg} (hint: {})",
                    hint.as_deref().unwrap_or("none")
                );
                continue;
            }
        };
        drop(reduced);

        // Generate passkey for new actors (same as first-ingest flow).
        {
            let reduced = state.reduced.read().await;
            if !reduced.actor_keys.contains_key(&canonicalize_actor(&actor)) {
                drop(reduced);
                let pk = format!("slug_sk_{}", uuid::Uuid::new_v4().simple());
                let key_hash = sha256_hex(&pk);
                let reg_event = Event::ActorKeyRegistration {
                    ts,
                    actor: actor.clone(),
                    key_hash,
                };
                if let Err(e) = state.event_log.append(&reg_event).await {
                    tracing::error!("failed to append ActorKeyRegistration: {e}");
                    continue;
                }
                let mut reduced = state.reduced.write().await;
                reduced.apply_event(reg_event);
                // Note: passkey is not communicated back to the X user.
                // If they later want CLI access they'd need a key rotation mechanism.
            }
        }

        let ingest_event = Event::Ingest(Ingest {
            ts,
            id: uuid::Uuid::new_v4().to_string(),
            raw: dsl_text.clone(),
            voter_key_id: actor.clone(),
            actor: actor.clone(),
        });

        if let Err(e) = state.event_log.append(&ingest_event).await {
            tracing::error!("failed to append Ingest event: {e}");
            continue;
        }

        let actor_for_stream = actor.clone();
        {
            let mut reduced = state.reduced.write().await;
            reduced.apply_event(ingest_event);
        }

        // Broadcast SSE.
        let _ = state.stream_tx.send(crate::state::StreamEvent {
            ts,
            actor: actor_for_stream,
            tags: validated.threads.iter().map(|t| format!("#{t}")).collect(),
            snippet: dsl_text.chars().take(200).collect(),
        });
        let html = crate::html::thread_feed_html(&state).await;
        let _ = state.html_tx.send(crate::state::HtmlFragment {
            selector: "#thread-feed".to_string(),
            html,
        });

        ingested += 1;
    }

    Ok(ingested)
}

/// Remove `@bothandle` (case-insensitive) from the tweet text.
fn strip_bot_mention(text: &str, bot_handle: &str) -> String {
    let pattern = format!("@{}", bot_handle);
    // Case-insensitive removal of all occurrences of @bothandle.
    let mut result = String::with_capacity(text.len());
    let lower = text.to_lowercase();
    let pat_lower = pattern.to_lowercase();
    let mut i = 0;
    while i < text.len() {
        if lower[i..].starts_with(&pat_lower) {
            i += pat_lower.len();
        } else {
            // Advance one character.
            let c = text[i..].chars().next().unwrap();
            result.push(c);
            i += c.len_utf8();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_x_actor_deterministic() {
        let a1 = x_actor("123456789", "alice");
        let a2 = x_actor("123456789", "alice");
        assert_eq!(a1, a2);
        assert!(a1.contains(":x.com:alice"));
        // UUID part should be valid.
        let uuid_str = a1.split(':').next().unwrap();
        assert!(uuid::Uuid::parse_str(uuid_str).is_ok());
    }

    #[test]
    fn test_x_actor_different_users() {
        let a1 = x_actor("111", "alice");
        let a2 = x_actor("222", "bob");
        assert_ne!(a1, a2);
    }

    #[test]
    fn test_strip_bot_mention() {
        assert_eq!(
            strip_bot_mention("@SlugBot ~/a 3:1 ~/b #test", "slugbot"),
            " ~/a 3:1 ~/b #test"
        );
        assert_eq!(
            strip_bot_mention("hey @SLUGBOT check this ~/a {body}", "slugbot"),
            "hey  check this ~/a {body}"
        );
        assert_eq!(
            strip_bot_mention("no mention here", "slugbot"),
            "no mention here"
        );
    }
}
