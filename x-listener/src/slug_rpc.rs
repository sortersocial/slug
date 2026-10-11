//! Post mapped tweets into slug via `POST /api/v0/rpc`.

use anyhow::{anyhow, Result};
use slug_types::{RpcBatch, RpcBatchResponse, RpcCommand, RpcLine, RpcResult};

pub struct SlugClient {
    http: reqwest::Client,
    base: String,
    bearer: String,
    delegate: String,
    room: String,
    thread_tag: String,
}

impl SlugClient {
    pub fn new(
        base: impl Into<String>,
        bearer: impl Into<String>,
        delegate: impl Into<String>,
        room: impl Into<String>,
        thread_tag: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into(),
            bearer: bearer.into(),
            delegate: delegate.into(),
            room: room.into(),
            thread_tag: thread_tag.into(),
        }
    }

    pub async fn post_text(&self, text: &str) -> Result<String> {
        let url = format!("{}/api/v0/rpc", self.base.trim_end_matches('/'));
        let batch = RpcBatch(vec![RpcCommand::Post {
            room: self.room.clone(),
            thread_tag: self.thread_tag.clone(),
            delegate: Some(self.delegate.clone()),
            text: text.to_string(),
            return_rank_diff: false,
        }]);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.bearer))
            .json(&batch)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("slug rpc HTTP {status}: {}", body.trim()));
        }
        let parsed: RpcBatchResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow!("slug rpc decode: {e}; body={}", body.trim()))?;
        let line = parsed
            .results
            .first()
            .ok_or_else(|| anyhow!("slug rpc empty results"))?;
        let result = rpc_line_ok(line)?;
        match result {
            RpcResult::PostOk { post_id, post_index, .. } => Ok(format!(
                "posted thread={} post_id={} index={}",
                self.thread_tag,
                post_id.as_deref().unwrap_or("?"),
                post_index.map(|i| i.to_string()).unwrap_or_else(|| "?".into())
            )),
            other => Err(anyhow!("unexpected rpc result: {other:?}")),
        }
    }
}

fn rpc_line_ok(line: &RpcLine) -> Result<&RpcResult> {
    if !line.ok {
        let mut m = line.error.clone().unwrap_or_else(|| "rpc error".into());
        if let Some(h) = &line.hint {
            m.push_str(&format!("\nhint: {h}"));
        }
        return Err(anyhow!(m));
    }
    line.result
        .as_ref()
        .ok_or_else(|| anyhow!("rpc missing result"))
}
