//! Telegram bot for vote ingestion.
//!
//! Polls the Telegram Bot API for incoming messages. Plain text is parsed as
//! .sorter DSL. Slash commands query the garden. Invalid DSL gets a friendly
//! error reply. Actor identity is deterministic UUID v5 from Telegram user ID.

use serde::Deserialize;
use std::time::Duration;

use crate::{
    api::{now_ms, sha256_hex, validate_ingest_document},
    events::{canonicalize_actor, Event, Ingest},
    state::{AppState, TelegramBotConfig},
};

// ── Actor identity ──────────────────────────────────────────────────────────

/// Slug-specific UUID v5 namespace for Telegram user IDs.
const TG_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x4b, 0x1c, 0xe8, 0x93, 0xa2, 0x7d, 0x48, 0x5f,
    0xb3, 0x91, 0x6e, 0xd0, 0x44, 0x8a, 0xcc, 0x12,
]);

/// Build a deterministic actor string for a Telegram user.
/// Format: `<uuid_v5(user_id)>:telegram:<username>`
fn tg_actor(user_id: &str, username: &str) -> String {
    let uuid = uuid::Uuid::new_v5(&TG_NAMESPACE, user_id.as_bytes());
    format!("{}:telegram:{}", uuid, username.to_lowercase())
}

// ── Telegram API types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TgResponse<T> {
    ok: bool,
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    message: Option<TgMessage>,
}

#[derive(Debug, Deserialize)]
struct TgMessage {
    message_id: i64,
    from: Option<TgUser>,
    chat: TgChat,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TgUser {
    id: i64,
    username: Option<String>,
    first_name: String,
}

#[derive(Debug, Deserialize)]
struct TgChat {
    id: i64,
}

// ── Telegram API helpers ────────────────────────────────────────────────────

fn api_url(cfg: &TelegramBotConfig, method: &str) -> String {
    format!("{}/bot{}/{}", cfg.api_base_url, cfg.bot_token, method)
}

async fn send_message(
    client: &reqwest::Client,
    cfg: &TelegramBotConfig,
    chat_id: i64,
    text: &str,
) {
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "Markdown",
    });
    match client
        .post(&api_url(cfg, "sendMessage"))
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            tracing::debug!(chat_id, "sent telegram reply");
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(chat_id, "telegram reply failed {status}: {body}");
        }
        Err(e) => {
            tracing::warn!(chat_id, "telegram reply http error: {e}");
        }
    }
}

// ── Slash commands ──────────────────────────────────────────────────────────

async fn handle_command(
    state: &AppState,
    cfg: &TelegramBotConfig,
    client: &reqwest::Client,
    chat_id: i64,
    command: &str,
    args: &str,
    username: &str,
) {
    let reply = match command {
        "/start" | "/help" => {
            "Welcome to slug.social!\n\n\
             *Send .sorter text* to vote:\n\
             ```\n\
             #parables\n\
             ~/parables/prodigal-son 3:1 ~/parables/good-samaritan { loss and return }\n\
             ```\n\n\
             *Commands:*\n\
             /rank <scope> — top-ranked items\n\
             /pair <scope> — random pair to vote on\n\
             /me — your actor identity\n\
             /help — this message"
                .to_string()
        }
        "/me" => {
            format!("You are `@{}` on slug.social", username)
        }
        "/rank" => {
            if args.is_empty() {
                "Usage: /rank <scope>\nExample: /rank parables".to_string()
            } else {
                let scope = args.split_whitespace().next().unwrap_or(args);
                let reduced = state.reduced.read().await;
                let canonical_scope = crate::events::canonicalize_item(&format!("~/{scope}"));
                let children = reduced.item_children.get(&canonical_scope);
                match children {
                    Some(kids) if !kids.is_empty() => {
                        let items: Vec<_> = kids.iter().cloned().collect();
                        let lines: Vec<String> = items
                            .iter()
                            .take(10)
                            .enumerate()
                            .map(|(i, item)| {
                                let name = item.rsplit('/').next().unwrap_or(item);
                                format!("{}. {}", i + 1, name)
                            })
                            .collect();
                        format!("*Items in {}:*\n{}", scope, lines.join("\n"))
                    }
                    _ => format!("No items found under ~/{scope}"),
                }
            }
        }
        "/pair" => {
            if args.is_empty() {
                "Usage: /pair <scope>\nExample: /pair parables".to_string()
            } else {
                let scope = args.split_whitespace().next().unwrap_or(args);
                let reduced = state.reduced.read().await;
                let canonical_scope = crate::events::canonicalize_item(&format!("~/{scope}"));
                let children = reduced.item_children.get(&canonical_scope);
                match children {
                    Some(kids) if kids.len() >= 2 => {
                        let items: Vec<_> = kids.iter().collect();
                        use rand::seq::SliceRandom;
                        let mut rng = rand::thread_rng();
                        let pair: Vec<_> = items.choose_multiple(&mut rng, 2).collect();
                        let a_name = pair[0].rsplit('/').next().unwrap_or(pair[0]);
                        let b_name = pair[1].rsplit('/').next().unwrap_or(pair[1]);
                        format!(
                            "*Which is better?*\n\nA: {a_name}\nB: {b_name}\n\n\
                             Vote by sending:\n\
                             `#tag\n~/{scope}/{a_name} 3:1 ~/{scope}/{b_name} {{ your reason }}`"
                        )
                    }
                    _ => format!("Need at least 2 items under ~/{scope}"),
                }
            }
        }
        _ => format!("Unknown command: {command}\nTry /help"),
    };

    send_message(client, cfg, chat_id, &reply).await;
}

// ── Ingestion ───────────────────────────────────────────────────────────────

async fn try_ingest(
    state: &AppState,
    cfg: &TelegramBotConfig,
    client: &reqwest::Client,
    chat_id: i64,
    user_id: &str,
    username: &str,
    message_id: &str,
    text: &str,
) -> bool {
    let actor = tg_actor(user_id, username);

    // Prepend actor declaration.
    let dsl_text = format!("@{actor}\n{text}");
    let ts = now_ms();

    // 1. Record TelegramMessage provenance.
    let tg_event = Event::TelegramMessage {
        ts,
        actor: actor.clone(),
        tg_user_id: user_id.to_string(),
        tg_username: username.to_string(),
        message_id: message_id.to_string(),
    };
    if let Err(e) = state.event_log.append(&tg_event).await {
        tracing::error!("failed to append TelegramMessage event: {e}");
        return false;
    }
    {
        let mut reduced = state.reduced.write().await;
        reduced.apply_event(tg_event);
    }

    // 2. Validate DSL.
    let reduced = state.reduced.read().await;
    let validated = match validate_ingest_document(
        &reduced,
        &dsl_text,
        "bot ingest requires @actor (auto-prepended)",
    ) {
        Ok(v) => v,
        Err((_status, err_msg, hint)) => {
            tracing::warn!(
                message_id,
                "skipping invalid telegram message: {err_msg} (hint: {})",
                hint.as_deref().unwrap_or("none")
            );
            let reply = format!(
                "{}{}\n\ntry the interactive editor: {}/try",
                err_msg,
                hint.as_ref().map(|h| format!(" — {h}")).unwrap_or_default(),
                cfg.public_url,
            );
            send_message(client, cfg, chat_id, &reply).await;
            return false;
        }
    };
    drop(reduced);

    // 3. Register actor key if new.
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
                return false;
            }
            let mut reduced = state.reduced.write().await;
            reduced.apply_event(reg_event);
        }
    }

    // 4. Create Ingest event.
    let ingest_event = Event::Ingest(Ingest {
        ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: dsl_text.clone(),
        voter_key_id: actor.clone(),
        actor: actor.clone(),
    });

    if let Err(e) = state.event_log.append(&ingest_event).await {
        tracing::error!("failed to append Ingest event: {e}");
        return false;
    }

    let actor_for_stream = actor;
    {
        let mut reduced = state.reduced.write().await;
        reduced.apply_event(ingest_event);
    }

    // 5. Broadcast SSE.
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

    // 6. Confirm to user.
    send_message(client, cfg, chat_id, "Ingested.").await;

    true
}

// ── Cursor persistence ──────────────────────────────────────────────────────

fn cursor_path(data_dir: &str) -> String {
    format!("{}/tg_bot_cursor.txt", data_dir)
}

fn load_cursor(data_dir: &str) -> Option<i64> {
    std::fs::read_to_string(cursor_path(data_dir))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn save_cursor(data_dir: &str, offset: i64) {
    let _ = std::fs::write(cursor_path(data_dir), offset.to_string());
}

// ── Main loop ───────────────────────────────────────────────────────────────

/// Main bot loop. Runs until the process shuts down.
pub async fn run_bot(state: AppState, cfg: TelegramBotConfig) {
    let client = reqwest::Client::new();
    let mut offset = load_cursor(&state.cfg.data_dir).unwrap_or(0);
    let interval = Duration::from_secs(cfg.poll_interval_secs);

    tracing::info!(
        poll_secs = cfg.poll_interval_secs,
        "telegram bot started"
    );

    loop {
        match poll_updates(&state, &cfg, &client, &mut offset).await {
            Ok(n) if n > 0 => tracing::info!(ingested = n, "telegram bot poll"),
            Ok(_) => tracing::debug!("telegram bot poll: no new messages"),
            Err(e) => tracing::warn!("telegram bot poll error: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}

async fn poll_updates(
    state: &AppState,
    cfg: &TelegramBotConfig,
    client: &reqwest::Client,
    offset: &mut i64,
) -> Result<usize, String> {
    let url = format!(
        "{}?offset={}&timeout=0&allowed_updates=[\"message\"]",
        api_url(cfg, "getUpdates"),
        offset
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("http error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("telegram api {status}: {body}"));
    }

    let tg_resp: TgResponse<Vec<TgUpdate>> = resp
        .json()
        .await
        .map_err(|e| format!("json parse error: {e}"))?;

    if !tg_resp.ok {
        return Err("telegram api returned ok=false".to_string());
    }

    let updates = match tg_resp.result {
        Some(u) if !u.is_empty() => u,
        _ => return Ok(0),
    };

    // Advance offset past the newest update.
    if let Some(last) = updates.last() {
        *offset = last.update_id + 1;
        save_cursor(&state.cfg.data_dir, *offset);
    }

    let mut ingested = 0;

    for update in &updates {
        let msg = match &update.message {
            Some(m) => m,
            None => continue,
        };

        let text = match &msg.text {
            Some(t) if !t.trim().is_empty() => t.trim(),
            _ => continue,
        };

        let user = match &msg.from {
            Some(u) => u,
            None => continue,
        };

        let user_id = user.id.to_string();
        let username = user
            .username
            .as_deref()
            .unwrap_or(&user.first_name)
            .to_lowercase();
        let message_id = msg.message_id.to_string();

        // Dedup.
        {
            let reduced = state.reduced.read().await;
            if reduced.seen_tg_message_ids.contains(&message_id) {
                continue;
            }
        }

        // Dispatch: slash command or ingest attempt.
        if text.starts_with('/') {
            let (cmd, args) = text
                .split_once(|c: char| c.is_whitespace())
                .unwrap_or((text, ""));
            // Strip @botname suffix from commands (e.g. /rank@slugbot -> /rank)
            let cmd = cmd.split('@').next().unwrap_or(cmd);
            handle_command(state, cfg, client, msg.chat.id, cmd, args.trim(), &username).await;
        } else {
            if try_ingest(
                state, cfg, client, msg.chat.id, &user_id, &username, &message_id, text,
            )
            .await
            {
                ingested += 1;
            }
        }
    }

    Ok(ingested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tg_actor_deterministic() {
        let a1 = tg_actor("123456789", "alice");
        let a2 = tg_actor("123456789", "alice");
        assert_eq!(a1, a2);
        assert!(a1.contains(":telegram:alice"));
        let uuid_str = a1.split(':').next().unwrap();
        assert!(uuid::Uuid::parse_str(uuid_str).is_ok());
    }

    #[test]
    fn test_tg_actor_different_users() {
        let a1 = tg_actor("111", "alice");
        let a2 = tg_actor("222", "bob");
        assert_ne!(a1, a2);
    }
}
