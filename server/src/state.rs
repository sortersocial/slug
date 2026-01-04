use std::{collections::HashMap, sync::Arc};

use tokio::sync::RwLock;

use crate::{
    event_log::EventLog,
    reducer::{GroupKey, ReducerState},
};

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
}

#[derive(Debug, Clone)]
pub struct RateWindow {
    pub window_start_ms: i64,
    pub count: u32,
}

impl AppState {
    pub fn new(cfg: AppConfig) -> Self {
        let event_log = EventLog::new(cfg.event_log_path.clone());
        Self {
            cfg: Arc::new(cfg),
            event_log: Arc::new(event_log),
            reduced: Arc::new(RwLock::new(ReducerState::default())),
            rate: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn group_keys(&self) -> Vec<GroupKey> {
        let s = self.reduced.read().await;
        s.groups.keys().cloned().collect()
    }
}


