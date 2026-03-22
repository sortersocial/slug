use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

type CountMap = Arc<Mutex<HashMap<String, u64>>>;

#[derive(Clone)]
pub struct ViewStore {
    counts: CountMap,
    flush_tx: mpsc::Sender<()>,
}

impl ViewStore {
    pub fn new(json_path: &str) -> Self {
        // Load existing counts from disk on startup (best-effort)
        let initial: HashMap<String, u64> = std::fs::read_to_string(json_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let counts: CountMap = Arc::new(Mutex::new(initial));
        let (flush_tx, mut flush_rx) = mpsc::channel::<()>(64);
        let path = json_path.to_string();

        let counts_for_writer = counts.clone();
        tokio::spawn(async move {
            while flush_rx.recv().await.is_some() {
                while flush_rx.try_recv().is_ok() {}

                let snapshot: HashMap<String, u64> = {
                    counts_for_writer.lock().unwrap().clone()
                };

                let path = path.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(json) = serde_json::to_string(&snapshot) {
                        let tmp = format!("{path}.tmp");
                        if std::fs::write(&tmp, &json).is_ok() {
                            let _ = std::fs::rename(&tmp, &path);
                        }
                    }
                })
                .await;
            }
        });

        Self { counts, flush_tx }
    }

    pub fn increment(&self, path: String) {
        {
            let mut map = self.counts.lock().unwrap();
            *map.entry(path).or_insert(0) += 1;
        }
        let _ = self.flush_tx.try_send(());
    }

    pub fn get_views(&self, path: &str) -> u64 {
        self.counts.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}
