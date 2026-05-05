use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, RwLock};

use crate::{
    event_log::EventLog, events::ThreadCapability, reducer::ReducerState, write_cmd::WriteCmd,
};

/// Coarse channel for server-push morphs (SSE topic subscription).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LiveTopic {
    ForumHome,
    PublicThread {
        tag: String,
    },
    RoomForum {
        room_id: String,
    },
    RoomThread {
        room_id: String,
        tag: String,
    },
    PublicGardenRoot,
    PublicGardenPath {
        tail: String,
    },
    PublicGardenExternal {
        tail: String,
    },
    RoomGardenRoot {
        room_id: String,
    },
    RoomGardenPath {
        room_id: String,
        tail: String,
    },
    RoomGardenExternal {
        room_id: String,
        tail: String,
    },
    GardenItem {
        room_wire: Option<String>,
        item_storage: String,
    },
}

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
    pub complete: Option<(String /*username*/, String /*bearer*/)>,
}

/// An SSE event broadcast to all live stream subscribers when an ingest occurs.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StreamEvent {
    pub ts: i64,
    pub actor: String,
    pub tags: Vec<String>,
    pub snippet: String,
}

/// Who may receive a [`JsSnippet`] over `/sse` (matches private-room HTML visibility).
#[derive(Debug, Clone)]
pub enum JsSnippetAudience {
    /// Public forum + pages not tied to a specific room (home, `/t/…`, etc.).
    Public,
    /// Members with [`crate::events::ThreadCapability::View`] on this room (`short/slug` wire id).
    RoomViewers(String),
}

/// JavaScript snippet broadcast to web SSE subscribers and `eval`'d client-side.
#[derive(Debug, Clone)]
pub struct JsSnippet {
    pub code: String,
    /// Deliver only to subscribers whose topic set intersects one of these topics.
    pub topics: Vec<LiveTopic>,
    pub audience: JsSnippetAudience,
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
    /// Ephemeral invite tokens (`inv_…`): in this process only, not written to the event log.
    /// Minted via `RoomMintInvite`; restarting the server drops any unused links. (Log event
    /// types `InviteMinted` / `InviteRedeemed` exist for replay and a possible future persisted path.)
    pub invites: Arc<RwLock<HashMap<String, InviteState>>>,
    /// Broadcast channel for SSE live-streaming. Capacity = 64 events.
    pub stream_tx: broadcast::Sender<StreamEvent>,
    /// Broadcast channel for web SSE JavaScript snippets. Capacity = 64.
    pub js_tx: broadcast::Sender<JsSnippet>,
    /// All durable writes and reducer mutations are serialized through this channel.
    pub write_tx: mpsc::Sender<WriteCmd>,
    pub views: crate::views::ViewStore,
}

impl AppState {
    /// Build state with a fresh writer channel; call [`crate::spawn_writer_actor_for_test`] after any in-process seeding.
    pub fn create_for_test(cfg: AppConfig) -> (Self, mpsc::Receiver<WriteCmd>) {
        let (write_tx, write_rx) = mpsc::channel(256);
        (Self::new_for_test(cfg, write_tx), write_rx)
    }

    /// Prefer [`crate::create_app_state`] so the writer task is started and `write_tx` is wired.
    pub fn new_for_test(cfg: AppConfig, write_tx: mpsc::Sender<WriteCmd>) -> Self {
        let event_log = EventLog::new(cfg.event_log_path.clone());
        let (stream_tx, _) = broadcast::channel(64);
        let (js_tx, _) = broadcast::channel(64);
        let views_path = format!("{}/views.json", cfg.data_dir);
        let views = crate::views::ViewStore::new(&views_path);
        Self {
            cfg: Arc::new(cfg),
            event_log: Arc::new(event_log),
            reduced: Arc::new(RwLock::new(ReducerState::default())),
            pending_sessions: Arc::new(RwLock::new(HashMap::new())),
            invites: Arc::new(RwLock::new(HashMap::new())),
            stream_tx,
            js_tx,
            write_tx,
            views,
        }
    }
}
