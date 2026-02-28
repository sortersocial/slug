use std::{collections::HashMap, sync::Arc};

use tokio::sync::{broadcast, RwLock};

use crate::{event_log::EventLog, reducer::ReducerState};

const PRESENCE_TTL_MS: i64 = 60_000;

/// An SSE event broadcast to all live stream subscribers when an ingest occurs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StreamEvent {
    pub ts: i64,
    pub actor: String,
    pub tags: Vec<String>,
    pub snippet: String,
}

/// An HTML fragment broadcast to web SSE subscribers (poem pattern).
/// `selector` is a CSS selector identifying the morph target.
/// `html` is the new HTML to morph into that element.
#[derive(Debug, Clone)]
pub struct HtmlFragment {
    pub selector: String,
    pub html: String,
}

#[derive(Debug, Clone)]
pub struct KeyRecord {
    pub id: String,
    pub secret: String,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub data_dir: String,
    pub event_log_path: String,
    pub keys: Vec<KeyRecord>,
    pub rate_limit_per_minute: u32,
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    pub event_log: Arc<EventLog>,
    pub reduced: Arc<RwLock<ReducerState>>,
    pub rate: Arc<RwLock<HashMap<String, RateWindow>>>,
    /// Active thread-page viewers keyed by stable browser session id.
    pub presence: Arc<RwLock<HashMap<String, PresenceSession>>>,
    /// Broadcast channel for SSE live-streaming. Capacity = 64 events.
    pub stream_tx: broadcast::Sender<StreamEvent>,
    /// Broadcast channel for web SSE HTML fragments (poem pattern). Capacity = 64.
    pub html_tx: broadcast::Sender<HtmlFragment>,
}

#[derive(Debug, Clone)]
pub struct RateWindow {
    pub window_start_ms: i64,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct PresenceSession {
    pub thread_tag: String,
    pub last_seen_ms: i64,
    pub cursor_anchor: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct PresenceCounts {
    pub global_viewers: usize,
    pub local_viewers: usize,
}

fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

impl AppState {
    pub fn new(cfg: AppConfig) -> Self {
        let event_log = EventLog::new(cfg.event_log_path.clone());
        let (stream_tx, _) = broadcast::channel(64);
        let (html_tx, _) = broadcast::channel(64);
        Self {
            cfg: Arc::new(cfg),
            event_log: Arc::new(event_log),
            reduced: Arc::new(RwLock::new(ReducerState::default())),
            rate: Arc::new(RwLock::new(HashMap::new())),
            presence: Arc::new(RwLock::new(HashMap::new())),
            stream_tx,
            html_tx,
        }
    }

    /// Update one viewer heartbeat and return (global, local) active counts.
    pub async fn upsert_presence(
        &self,
        session_id: String,
        thread_tag: String,
        cursor_anchor: Option<String>,
    ) -> PresenceCounts {
        let now = now_ms();
        let mut presence = self.presence.write().await;
        presence.retain(|_, entry| now.saturating_sub(entry.last_seen_ms) <= PRESENCE_TTL_MS);
        presence.insert(
            session_id,
            PresenceSession {
                thread_tag: thread_tag.clone(),
                last_seen_ms: now,
                cursor_anchor,
            },
        );
        let global_viewers = presence.len();
        let local_viewers = presence
            .values()
            .filter(|entry| entry.thread_tag == thread_tag)
            .count();
        PresenceCounts {
            global_viewers,
            local_viewers,
        }
    }

    /// Read presence counts for initial server-rendered thread view.
    pub async fn presence_counts_for_thread(&self, thread_tag: &str) -> PresenceCounts {
        let now = now_ms();
        let mut presence = self.presence.write().await;
        presence.retain(|_, entry| now.saturating_sub(entry.last_seen_ms) <= PRESENCE_TTL_MS);
        let global_viewers = presence.len();
        let local_viewers = presence
            .values()
            .filter(|entry| entry.thread_tag == thread_tag)
            .count();
        PresenceCounts {
            global_viewers,
            local_viewers,
        }
    }
}


