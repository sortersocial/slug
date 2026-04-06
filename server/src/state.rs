use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, RwLock};

use crate::{event_log::EventLog, events::ThreadCapability, reducer::ReducerState};

/// Ephemeral invite link (24h TTL, in-memory only; not written to the event log).
#[derive(Debug, Clone)]
pub struct InviteState {
    pub room_id: String,
    pub capabilities: Vec<ThreadCapability>,
    pub expires_at_ms: i64,
    pub max_uses: usize,
    pub current_uses: usize,
    pub inviter: String,
}

#[derive(Debug, Clone)]
pub struct PendingSession {
    pub agent: String,
    pub created_ts: i64,
    pub provider: Option<String>,
    pub provider_id: Option<String>,
    /// When set, successful OAuth completion redeems this invite token and appends [`crate::events::GrantAdded`].
    pub redeem_invite: Option<String>,
    pub complete: Option<(String /*username*/, String /*bearer*/ )>,
}

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
pub struct AppConfig {
    pub data_dir: String,
    pub event_log_path: String,
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<AppConfig>,
    pub event_log: Arc<EventLog>,
    pub reduced: Arc<RwLock<ReducerState>>,
    pub pending_sessions: Arc<RwLock<HashMap<String, PendingSession>>>,
    /// Ephemeral invite tokens (`inv_…`) until expiry or exhaustion.
    pub invites: Arc<RwLock<HashMap<String, InviteState>>>,
    /// Broadcast channel for SSE live-streaming. Capacity = 64 events.
    pub stream_tx: broadcast::Sender<StreamEvent>,
    /// Broadcast channel for web SSE HTML fragments (poem pattern). Capacity = 64.
    pub html_tx: broadcast::Sender<HtmlFragment>,
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
            pending_sessions: Arc::new(RwLock::new(HashMap::new())),
            invites: Arc::new(RwLock::new(HashMap::new())),
            stream_tx,
            html_tx,
        }
    }
}

