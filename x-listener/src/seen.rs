//! Persistent set of ingested tweet ids (one id per line).

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub struct SeenStore {
    path: PathBuf,
    ids: HashSet<String>,
}

impl SeenStore {
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mut ids = HashSet::new();
        if path.exists() {
            let raw = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("read seen file {}", path.display()))?;
            for line in raw.lines() {
                let id = line.trim();
                if !id.is_empty() {
                    ids.insert(id.to_string());
                }
            }
        } else if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create {}", parent.display()))?;
        }
        Ok(Self { path, ids })
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    pub async fn insert(&mut self, id: &str) -> Result<()> {
        if !self.ids.insert(id.to_string()) {
            return Ok(());
        }
        let mut f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .with_context(|| format!("append {}", self.path.display()))?;
        f.write_all(id.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persists_across_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seen.txt");
        {
            let mut s = SeenStore::open(&path).await.unwrap();
            s.insert("111").await.unwrap();
            s.insert("222").await.unwrap();
            assert!(s.contains("111"));
        }
        let s = SeenStore::open(&path).await.unwrap();
        assert!(s.contains("111"));
        assert!(s.contains("222"));
        assert_eq!(s.len(), 2);
    }
}
