//! Signal message bot.
//!
//! Polls a signal-cli-api instance for incoming messages. Each message is
//! parsed as DSL, attributed to the sender's deterministic actor identity,
//! and ingested through the standard event log pipeline.
//!
//! Architecture: signal-cli-api (Rust crate, no Docker) wraps signal-cli and
//! exposes a REST API. We poll GET /v1/receive/{number} which returns and
//! consumes pending messages. Any compatible Signal REST API works.

use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    api::{now_ms, sha256_hex, validate_ingest_document},
    events::{canonicalize_actor, Event, Ingest},
    state::{AppState, SignalBotConfig},
};

/// Slug-specific UUID v5 namespace for Signal phone numbers → deterministic actor UUIDs.
/// Generated once, never change this or all existing Signal actor UUIDs will shift.
const SIGNAL_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x8a, 0x3f, 0xd2, 0x11, 0xb7, 0x4e, 0x4c, 0x19,
    0x92, 0x5a, 0x0e, 0xc8, 0x73, 0x41, 0xaa, 0x6d,
]);

/// Build a deterministic actor string for a Signal user.
/// Format: `<uuid_v5(phone)>:signal:<phone_hash_prefix>`
/// We use a hash prefix instead of the raw phone number to avoid storing PII.
fn signal_actor(phone: &str) -> String {
    let uuid = uuid::Uuid::new_v5(&SIGNAL_NAMESPACE, phone.as_bytes());
    let hash = phone_hash(phone);
    format!("{}:signal:{}", uuid, &hash[..8])
}

/// SHA-256 hex of a phone number. Used for provenance without storing PII.
fn phone_hash(phone: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(phone.as_bytes());
    hex::encode(hasher.finalize())
}

// We need hex encoding — use the sha2 output directly.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

// --- signal-cli-api response types ---

#[derive(Debug, Deserialize)]
struct SignalEnvelope {
    envelope: Option<EnvelopeInner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvelopeInner {
    source: Option<String>,
    timestamp: Option<i64>,
    data_message: Option<DataMessage>,
}

#[derive(Debug, Deserialize)]
struct DataMessage {
    message: Option<String>,
    timestamp: Option<i64>,
}

/// Reply to a Signal user via signal-cli-api.
async fn reply_to_signal(
    client: &reqwest::Client,
    cfg: &SignalBotConfig,
    recipient: &str,
    text: &str,
) {
    let url = format!("{}/v2/send", cfg.api_base_url);
    let body = serde_json::json!({
        "message": text,
        "number": cfg.phone_number,
        "recipients": [recipient],
    });

    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            tracing::info!(recipient, "replied via signal");
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!(recipient, "signal reply failed {status}: {body}");
        }
        Err(e) => {
            tracing::warn!(recipient, "signal reply http error: {e}");
        }
    }
}

/// Main bot loop. Runs until the process shuts down.
pub async fn run_bot(state: AppState, cfg: SignalBotConfig) {
    let client = reqwest::Client::new();

    let interval = Duration::from_secs(cfg.poll_interval_secs);
    tracing::info!(
        phone = %cfg.phone_number,
        poll_secs = cfg.poll_interval_secs,
        "signal bot started"
    );

    loop {
        match poll_and_ingest(&state, &cfg, &client).await {
            Ok(n) if n > 0 => tracing::info!(ingested = n, "signal bot poll"),
            Ok(_) => tracing::debug!("signal bot poll: no new messages"),
            Err(e) => tracing::warn!("signal bot poll error: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}

async fn poll_and_ingest(
    state: &AppState,
    cfg: &SignalBotConfig,
    client: &reqwest::Client,
) -> Result<usize, String> {
    // GET /v1/receive/{number} returns and consumes pending messages.
    let url = format!(
        "{}/v1/receive/{}",
        cfg.api_base_url,
        urlencoding::encode(&cfg.phone_number)
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("http error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("signal-cli-api {status}: {body}"));
    }

    let envelopes: Vec<SignalEnvelope> = resp
        .json()
        .await
        .map_err(|e| format!("json parse error: {e}"))?;

    let mut ingested = 0;

    for env in &envelopes {
        let inner = match &env.envelope {
            Some(e) => e,
            None => continue,
        };

        let data = match &inner.data_message {
            Some(d) => d,
            None => continue, // receipt, typing indicator, etc.
        };

        let message = match &data.message {
            Some(m) if !m.trim().is_empty() => m.trim(),
            _ => continue,
        };

        let source = match &inner.source {
            Some(s) => s.as_str(),
            None => continue,
        };

        // Use Signal's message timestamp as dedup key.
        let signal_ts = data
            .timestamp
            .or(inner.timestamp)
            .map(|t| t.to_string())
            .unwrap_or_default();

        if signal_ts.is_empty() {
            continue;
        }

        // Dedup: skip messages already processed.
        {
            let reduced = state.reduced.read().await;
            if reduced.seen_signal_ts.contains(&signal_ts) {
                continue;
            }
        }

        let actor = signal_actor(source);
        let ph = phone_hash(source);

        // Prepend the actor declaration so the DSL parser sees it.
        let dsl_text = format!("@{actor}\n{message}");

        let ts = now_ms();

        // 1. Record SignalMessage event (provenance).
        let signal_event = Event::SignalMessage {
            ts,
            actor: actor.clone(),
            phone_hash: ph,
            signal_ts: signal_ts.clone(),
        };
        if let Err(e) = state.event_log.append(&signal_event).await {
            tracing::error!("failed to append SignalMessage event: {e}");
            continue;
        }
        {
            let mut reduced = state.reduced.write().await;
            reduced.apply_event(signal_event);
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
                    signal_ts,
                    "skipping invalid signal message: {msg} (hint: {})",
                    hint.as_deref().unwrap_or("none")
                );
                let reply_text = format!(
                    "{}{}\n\ntry the interactive editor: {}/try",
                    msg,
                    hint.as_ref().map(|h| format!(" — {h}")).unwrap_or_default(),
                    cfg.public_url,
                );
                reply_to_signal(client, cfg, source, &reply_text).await;
                continue;
            }
        };
        drop(reduced);

        // Generate passkey for new actors.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_actor_deterministic() {
        let a1 = signal_actor("+15551234567");
        let a2 = signal_actor("+15551234567");
        assert_eq!(a1, a2);
        assert!(a1.contains(":signal:"));
        // UUID part should be valid.
        let uuid_str = a1.split(':').next().unwrap();
        assert!(uuid::Uuid::parse_str(uuid_str).is_ok());
    }

    #[test]
    fn test_signal_actor_different_numbers() {
        let a1 = signal_actor("+15551234567");
        let a2 = signal_actor("+15559876543");
        assert_ne!(a1, a2);
    }

    #[test]
    fn test_phone_hash_consistent() {
        let h1 = phone_hash("+15551234567");
        let h2 = phone_hash("+15551234567");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_phone_hash_different() {
        let h1 = phone_hash("+15551234567");
        let h2 = phone_hash("+15559876543");
        assert_ne!(h1, h2);
    }
}
