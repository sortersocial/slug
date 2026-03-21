use std::path::{Path, PathBuf};

use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};

use crate::events::Event;

#[derive(Debug, thiserror::Error)]
pub enum EventLogError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn ensure_parent_dir(&self) -> Result<(), EventLogError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        Ok(())
    }

    pub async fn append(&self, event: &Event) -> Result<(), EventLogError> {
        self.ensure_parent_dir().await?;
        let mut f: tokio::fs::File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        f.write_all(line.as_bytes()).await?;
        f.flush().await?;
        Ok(())
    }

    /// Load events from JSONL. Corrupt lines are skipped and returned as `(line_no, line)`.
    pub async fn load_all(&self) -> Result<(Vec<Event>, Vec<(usize, String)>), EventLogError> {
        if !fs::try_exists(&self.path).await? {
            return Ok((vec![], vec![]));
        }

        let f = fs::File::open(&self.path).await?;
        let mut reader = BufReader::new(f).lines();

        let mut events = Vec::new();
        let mut bad_lines = Vec::new();

        let mut line_no: usize = 0;
        while let Some(line) = reader.next_line().await? {
            line_no += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Event>(trimmed) {
                Ok(ev) => events.push(ev),
                Err(_) => bad_lines.push((line_no, line)),
            }
        }

        Ok((events, bad_lines))
    }
}


