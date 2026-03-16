use std::{collections::HashMap, sync::Arc};

use tokio::sync::{broadcast, RwLock};

use crate::{event_log::EventLog, reducer::ReducerState};

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
    /// Active SSE viewers per thread (thread_tag → connection count).
    pub sse_presence: Arc<RwLock<HashMap<String, usize>>>,
    /// Broadcast channel for presence count updates (JSON string).
    pub presence_tx: broadcast::Sender<String>,
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
        let (presence_tx, _) = broadcast::channel(64);
        Self {
            cfg: Arc::new(cfg),
            event_log: Arc::new(event_log),
            reduced: Arc::new(RwLock::new(ReducerState::default())),
            rate: Arc::new(RwLock::new(HashMap::new())),
            sse_presence: Arc::new(RwLock::new(HashMap::new())),
            presence_tx,
            stream_tx,
            html_tx,
        }
    }

    /// Returns (global_viewers, local_viewers) for server-side initial render.
    pub async fn presence_counts(&self, thread_tag: &str) -> (usize, usize) {
        let p = self.sse_presence.read().await;
        let global: usize = p.values().sum();
        let local = p.get(thread_tag).copied().unwrap_or(0);
        (global, local)
    }
}

