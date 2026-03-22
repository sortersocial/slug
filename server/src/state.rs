use std::{collections::HashMap, sync::Arc};

use tokio::sync::{broadcast, RwLock};

use crate::{event_log::EventLog, reducer::ReducerState, views::ViewStore};

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
    /// Override for views DB path. When None, uses `{data_dir}/views.redb`.
    /// Useful when multiple servers share the same data_dir (e.g. integration tests).
    pub views_path: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    pub event_log: Arc<EventLog>,
    pub reduced: Arc<RwLock<ReducerState>>,
    pub rate: Arc<RwLock<HashMap<String, RateWindow>>>,
    /// Broadcast channel for SSE live-streaming. Capacity = 64 events.
    pub stream_tx: broadcast::Sender<StreamEvent>,
    /// Broadcast channel for web SSE HTML fragments (poem pattern). Capacity = 64.
    pub html_tx: broadcast::Sender<HtmlFragment>,
    pub views: ViewStore,
}

#[derive(Debug, Clone)]
pub struct RateWindow {
    pub window_start_ms: i64,
    pub count: u32,
}


impl AppState {
    pub fn new(cfg: AppConfig) -> Self {
        let event_log = EventLog::new(cfg.event_log_path.clone());
        let (stream_tx, _) = broadcast::channel(64);
        let (html_tx, _) = broadcast::channel(64);
        let views_path = cfg
            .views_path
            .clone()
            .unwrap_or_else(|| format!("{}/views.json", cfg.data_dir));
        let views = ViewStore::new(&views_path);
        Self {
            cfg: Arc::new(cfg),
            event_log: Arc::new(event_log),
            reduced: Arc::new(RwLock::new(ReducerState::default())),
            rate: Arc::new(RwLock::new(HashMap::new())),
            stream_tx,
            html_tx,
            views,
        }
    }
}

