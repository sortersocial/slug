//! Signal message bot.
//!
//! Two modes:
//!   1. Native mode (production): uses libsignal-service-rs to speak the Signal
//!      protocol directly. No JVM, no Docker, no sidecar. Links as a secondary
//!      device via QR code provisioning. Set SLUG_SIGNAL_PHONE only.
//!   2. HTTP mode (testing / fallback): polls a signal-cli-api REST endpoint.
//!      Set SLUG_SIGNAL_API_URL + SLUG_SIGNAL_PHONE.
//!
//! Both modes feed into the same ingestion pipeline: parse DSL, create
//! SignalMessage provenance event, validate, create Ingest event, broadcast.

use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    api::{now_ms, sha256_hex, validate_ingest_document},
    events::{canonicalize_actor, Event, Ingest},
    state::{AppState, SignalBotConfig},
};

// ── Actor identity ──────────────────────────────────────────────────────────

/// Slug-specific UUID v5 namespace for Signal phone numbers → deterministic actor UUIDs.
/// Generated once, never change this or all existing Signal actor UUIDs will shift.
const SIGNAL_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x8a, 0x3f, 0xd2, 0x11, 0xb7, 0x4e, 0x4c, 0x19,
    0x92, 0x5a, 0x0e, 0xc8, 0x73, 0x41, 0xaa, 0x6d,
]);

/// Build a deterministic actor string for a Signal user.
/// Format: `<uuid_v5(phone)>:signal:<phone_hash_prefix>`
fn signal_actor(phone: &str) -> String {
    let uuid = uuid::Uuid::new_v5(&SIGNAL_NAMESPACE, phone.as_bytes());
    let hash = phone_hash(phone);
    format!("{}:signal:{}", uuid, &hash[..8])
}

/// SHA-256 hex of a phone number. Provenance without PII.
fn phone_hash(phone: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(phone.as_bytes());
    hex::encode(hasher.finalize())
}

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

// ── Shared ingestion ────────────────────────────────────────────────────────

/// A message received from Signal, ready for ingestion.
struct IncomingSignalMessage {
    /// Sender's phone number (e.g. "+15551234567").
    source: String,
    /// Message text body.
    text: String,
    /// Signal message timestamp (string, used for dedup).
    signal_ts: String,
}

/// Result of attempting to ingest a message.
enum IngestResult {
    /// Successfully ingested.
    Ok,
    /// Skipped (dedup).
    Dedup,
    /// Validation error — caller should reply with the error text.
    ValidationError { reply_text: String },
    /// Internal error.
    Error,
}

/// Process a single incoming Signal message through the ingestion pipeline.
async fn ingest_message(
    state: &AppState,
    cfg: &SignalBotConfig,
    msg: &IncomingSignalMessage,
) -> IngestResult {
    // Dedup.
    {
        let reduced = state.reduced.read().await;
        if reduced.seen_signal_ts.contains(&msg.signal_ts) {
            return IngestResult::Dedup;
        }
    }

    let actor = signal_actor(&msg.source);
    let ph = phone_hash(&msg.source);
    let dsl_text = format!("@{actor}\n{}", msg.text);
    let ts = now_ms();

    // 1. Record SignalMessage provenance event.
    let signal_event = Event::SignalMessage {
        ts,
        actor: actor.clone(),
        phone_hash: ph,
        signal_ts: msg.signal_ts.clone(),
    };
    if let Err(e) = state.event_log.append(&signal_event).await {
        tracing::error!("failed to append SignalMessage event: {e}");
        return IngestResult::Error;
    }
    {
        let mut reduced = state.reduced.write().await;
        reduced.apply_event(signal_event);
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
                signal_ts = %msg.signal_ts,
                "skipping invalid signal message: {err_msg} (hint: {})",
                hint.as_deref().unwrap_or("none")
            );
            let reply_text = format!(
                "{}{}\n\ntry the interactive editor: {}/try",
                err_msg,
                hint.as_ref().map(|h| format!(" — {h}")).unwrap_or_default(),
                cfg.public_url,
            );
            return IngestResult::ValidationError { reply_text };
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
                return IngestResult::Error;
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
        return IngestResult::Error;
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

    IngestResult::Ok
}

// ── HTTP polling mode (testing / fallback) ──────────────────────────────────

#[derive(Debug, Deserialize)]
struct HttpEnvelope {
    envelope: Option<HttpEnvelopeInner>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HttpEnvelopeInner {
    source: Option<String>,
    timestamp: Option<i64>,
    data_message: Option<HttpDataMessage>,
}

#[derive(Debug, Deserialize)]
struct HttpDataMessage {
    message: Option<String>,
    timestamp: Option<i64>,
}

async fn reply_http(
    client: &reqwest::Client,
    cfg: &SignalBotConfig,
    recipient: &str,
    text: &str,
) {
    let api_url = match &cfg.api_base_url {
        Some(u) => u,
        None => return,
    };
    let url = format!("{}/v2/send", api_url);
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

/// HTTP polling loop — used when SLUG_SIGNAL_API_URL is set.
pub async fn run_http_bot(state: AppState, cfg: SignalBotConfig) {
    let client = reqwest::Client::new();
    let interval = Duration::from_secs(cfg.poll_interval_secs);
    let api_url = cfg.api_base_url.clone().unwrap_or_default();

    tracing::info!(
        phone = %cfg.phone_number,
        api_url = %api_url,
        poll_secs = cfg.poll_interval_secs,
        "signal bot started (http mode)"
    );

    loop {
        match poll_http(&state, &cfg, &client).await {
            Ok(n) if n > 0 => tracing::info!(ingested = n, "signal bot poll"),
            Ok(_) => tracing::debug!("signal bot poll: no new messages"),
            Err(e) => tracing::warn!("signal bot poll error: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}

async fn poll_http(
    state: &AppState,
    cfg: &SignalBotConfig,
    client: &reqwest::Client,
) -> Result<usize, String> {
    let api_url = cfg.api_base_url.as_deref().unwrap_or("");
    let url = format!(
        "{}/v1/receive/{}",
        api_url,
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
        return Err(format!("signal api {status}: {body}"));
    }

    let envelopes: Vec<HttpEnvelope> = resp
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
            None => continue,
        };
        let message = match &data.message {
            Some(m) if !m.trim().is_empty() => m.trim().to_string(),
            _ => continue,
        };
        let source = match &inner.source {
            Some(s) => s.clone(),
            None => continue,
        };
        let signal_ts = data
            .timestamp
            .or(inner.timestamp)
            .map(|t| t.to_string())
            .unwrap_or_default();
        if signal_ts.is_empty() {
            continue;
        }

        let msg = IncomingSignalMessage {
            source: source.clone(),
            text: message,
            signal_ts,
        };

        match ingest_message(state, cfg, &msg).await {
            IngestResult::Ok => ingested += 1,
            IngestResult::ValidationError { reply_text } => {
                reply_http(client, cfg, &msg.source, &reply_text).await;
            }
            _ => {}
        }
    }

    Ok(ingested)
}

// ── Native Signal protocol mode (production) ────────────────────────────────

/// Native Signal protocol loop — used when SLUG_SIGNAL_API_URL is NOT set.
/// Connects directly to Signal servers via libsignal-service-rs.
pub async fn run_native_bot(state: AppState, cfg: SignalBotConfig) {
    use futures_util::StreamExt;
    use libsignal_service::{
        configuration::SignalServers,
        messagepipe::Incoming,
        push_service::PushService,
        receiver::MessageReceiver,
    };

    let db_path = format!("{}/signal.db", state.cfg.data_dir);
    tracing::info!(
        phone = %cfg.phone_number,
        db = %db_path,
        "signal bot started (native mode)"
    );

    // Load or create credentials from stored state.
    let (_stored, creds) = match load_credentials(&state.cfg.data_dir).await {
        Some(c) => c,
        None => {
            tracing::error!(
                "signal bot: no credentials found. run the server with \
                 SLUG_SIGNAL_LINK=1 to link as a secondary device first."
            );
            return;
        }
    };

    let push_service = PushService::new(
        SignalServers::Production,
        Some(creds.clone()),
        "slug-signal-bot/0.1",
    );

    let mut receiver = MessageReceiver::new(push_service);

    loop {
        match receiver
            .create_message_pipe(creds.clone(), false)
            .await
        {
            Ok(pipe) => {
                tracing::info!("signal websocket connected");
                let stream = pipe.stream();
                futures_util::pin_mut!(stream);

                while let Some(item) = stream.next().await {
                    match item {
                        Ok(Incoming::Envelope(envelope)) => {
                            // Extract sender and message from the envelope.
                            // The envelope is a protobuf; we need to decrypt it
                            // with ServiceCipher, but for now log raw receipt.
                            if let Some(source) = &envelope.source_service_id {
                                tracing::info!(source = %source, "received signal envelope");
                            }
                            // TODO: decrypt envelope with ServiceCipher and feed
                            // the plaintext into ingest_message(). This requires
                            // the full store + cipher setup from link_device().
                        }
                        Ok(Incoming::QueueEmpty) => {
                            tracing::debug!("signal message queue empty");
                        }
                        Err(e) => {
                            tracing::warn!("signal stream error: {e}");
                            break; // reconnect
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("signal websocket connect failed: {e}");
            }
        }

        tracing::info!("signal bot reconnecting in 5s…");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// Stored credentials for the linked Signal device.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StoredCredentials {
    pub phone_number: String,
    pub uuid: String,
    pub password: String,
    pub device_id: u32,
}

impl StoredCredentials {
    fn to_service_credentials(&self) -> libsignal_service::configuration::ServiceCredentials {
        use libsignal_service::configuration::ServiceCredentials;
        use libsignal_service::protocol::DeviceId;
        ServiceCredentials {
            aci: self.uuid.parse().ok(),
            pni: None,
            phonenumber: self
                .phone_number
                .parse()
                .expect("stored phone number should be valid E164"),
            password: Some(self.password.clone()),
            device_id: DeviceId::new(self.device_id as u8).ok(),
        }
    }
}

async fn load_credentials(data_dir: &str) -> Option<(StoredCredentials, libsignal_service::configuration::ServiceCredentials)> {
    let path = format!("{}/signal_credentials.json", data_dir);
    let data = tokio::fs::read_to_string(&path).await.ok()?;
    let stored: StoredCredentials = serde_json::from_str(&data).ok()?;
    let creds = stored.to_service_credentials();
    Some((stored, creds))
}

/// Link this bot as a secondary Signal device. Prints a QR code URL to stderr.
/// Called once during initial setup (SLUG_SIGNAL_LINK=1).
pub async fn link_device(data_dir: &str, phone_number: &str) {
    use futures::StreamExt;
    use libsignal_service::{
        configuration::SignalServers,
        provisioning::{link_device as signal_link, SecondaryDeviceProvisioning},
        push_service::PushService,
    };

    let db_path = format!("{}/signal.db", data_dir);
    let mut rng = rand_09::thread_rng();
    let identity_key_pair =
        libsignal_service::protocol::IdentityKeyPair::generate(&mut rng);
    let registration_id = 1;

    let mut aci_store =
        crate::signal_store::SignalStore::open(&db_path, identity_key_pair, registration_id)
            .await
            .expect("failed to open signal store");
    let mut pni_store = aci_store.clone();

    let password: String = {
        use rand_09::Rng;
        let mut rng = rand_09::thread_rng();
        (0..24)
            .map(|_| {
                let idx: u8 = rng.gen_range(0..36);
                if idx < 10 {
                    (b'0' + idx) as char
                } else {
                    (b'a' + idx - 10) as char
                }
            })
            .collect()
    };

    let push_service = PushService::new(
        SignalServers::Production,
        None,
        "slug-signal-bot/0.1",
    );

    let (tx, mut rx) = futures::channel::mpsc::channel(2);

    let device_name = "slug-signal-bot".to_string();
    let password_clone = password.clone();
    let link_handle = tokio::task::spawn_local(async move {
        signal_link(
            &mut aci_store,
            &mut pni_store,
            &mut rand_09::thread_rng(),
            push_service,
            &password_clone,
            &device_name,
            tx,
        )
        .await
    });

    while let Some(event) = rx.next().await {
        match event {
            SecondaryDeviceProvisioning::Url(url) => {
                eprintln!("\n╔══════════════════════════════════════════╗");
                eprintln!("║  Scan this URL as QR code with Signal:   ║");
                eprintln!("╠══════════════════════════════════════════╣");
                eprintln!("║  {url}");
                eprintln!("╚══════════════════════════════════════════╝\n");
            }
            SecondaryDeviceProvisioning::NewDeviceRegistration(reg) => {
                let creds = StoredCredentials {
                    phone_number: phone_number.to_string(),
                    uuid: reg.service_ids.aci.to_string(),
                    password: password.clone(),
                    device_id: u32::from(reg.device_id),
                };
                let path = format!("{}/signal_credentials.json", data_dir);
                let data = serde_json::to_string_pretty(&creds).unwrap();
                tokio::fs::write(&path, &data).await.unwrap();
                eprintln!("[signal] linked! credentials saved to {path}");
            }
        }
    }

    match link_handle.await {
        Ok(Ok(())) => eprintln!("[signal] device linking complete"),
        Ok(Err(e)) => eprintln!("[signal] linking failed: {e}"),
        Err(e) => eprintln!("[signal] linking task panicked: {e}"),
    }
}

// ── Entry point ─────────────────────────────────────────────────────────────

/// Main entry point. Chooses HTTP or native mode based on config.
pub async fn run_bot(state: AppState, cfg: SignalBotConfig) {
    if cfg.api_base_url.is_some() {
        run_http_bot(state, cfg).await;
    } else {
        run_native_bot(state, cfg).await;
    }
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
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn test_phone_hash_different() {
        let h1 = phone_hash("+15551234567");
        let h2 = phone_hash("+15559876543");
        assert_ne!(h1, h2);
    }
}
