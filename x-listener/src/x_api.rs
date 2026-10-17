//! Minimal X API v2 filtered-stream client (raw reqwest — no abandoned Twitter SDK).

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::map::Tweet;

const API: &str = "https://api.x.com/2";

#[derive(Debug, Deserialize)]
struct RulesGet {
    #[serde(default)]
    data: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
struct Rule {
    id: String,
    value: String,
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamPayload {
    data: Tweet,
    #[serde(default)]
    includes: Option<Includes>,
}

#[derive(Debug, Deserialize, Default)]
struct Includes {
    #[serde(default)]
    users: Vec<User>,
}

#[derive(Debug, Deserialize)]
struct User {
    id: String,
    username: String,
}

#[derive(Debug, Clone)]
pub struct StreamTweet {
    pub tweet: Tweet,
    pub username: String,
}

pub struct XClient {
    bearer: String,
}

impl XClient {
    pub fn new(bearer: impl Into<String>) -> Self {
        Self {
            bearer: bearer.into(),
        }
    }

    fn http_short(&self) -> Result<reqwest::Client> {
        reqwest::Client::builder()
            .user_agent("slugsocial-x-listener/0.0.1")
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .context("reqwest client")
    }

    /// Ensure a filtered-stream rule exists for `value` (e.g. `#slugsocial`).
    pub async fn ensure_rule(&self, value: &str, tag: &str) -> Result<()> {
        let http = self.http_short()?;
        let url = format!("{API}/tweets/search/stream/rules");
        let resp = http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.bearer))
            .send()
            .await
            .context("list stream rules")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("list rules HTTP {status}: {}", body.trim()));
        }
        let parsed: RulesGet = serde_json::from_str(&body).unwrap_or(RulesGet { data: vec![] });
        if parsed.data.iter().any(|r| r.value == value) {
            eprintln!("x-listener: stream rule already present: {value}");
            return Ok(());
        }
        let stale: Vec<String> = parsed
            .data
            .iter()
            .filter(|r| r.tag.as_deref() == Some(tag))
            .map(|r| r.id.clone())
            .collect();
        if !stale.is_empty() {
            let del = json!({ "delete": { "ids": stale } });
            let resp = http
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.bearer))
                .json(&del)
                .send()
                .await
                .context("delete stale rules")?;
            if !resp.status().is_success() {
                let t = resp.text().await.unwrap_or_default();
                return Err(anyhow!("delete rules failed: {}", t.trim()));
            }
        }
        let add = json!({
            "add": [{ "value": value, "tag": tag }]
        });
        let resp = http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.bearer))
            .json(&add)
            .send()
            .await
            .context("add stream rule")?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("add rule HTTP {status}: {}", body.trim()));
        }
        eprintln!("x-listener: added stream rule {value:?} tag={tag}");
        Ok(())
    }

    /// Spawn a reconnecting filtered-stream reader; returns a channel of tweets.
    pub fn spawn_filtered_stream(&self) -> mpsc::Receiver<StreamTweet> {
        let (tx, rx) = mpsc::channel(32);
        let bearer = self.bearer.clone();
        tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                match stream_once(&bearer, &tx).await {
                    Ok(()) => {
                        eprintln!("x-listener: stream ended cleanly; reconnecting");
                        backoff = Duration::from_secs(1);
                    }
                    Err(e) => {
                        eprintln!("x-listener: stream error: {e:#}; retry in {backoff:?}");
                    }
                }
                if tx.is_closed() {
                    break;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        });
        rx
    }
}

async fn stream_once(bearer: &str, tx: &mpsc::Sender<StreamTweet>) -> Result<()> {
    let http = reqwest::Client::builder()
        .user_agent("slugsocial-x-listener/0.0.1")
        .connect_timeout(Duration::from_secs(20))
        .build()
        .context("stream client")?;
    let url = format!(
        "{API}/tweets/search/stream?\
         tweet.fields=author_id,created_at,entities,text&\
         expansions=author_id&\
         user.fields=username"
    );
    let resp = http
        .get(&url)
        .header("Authorization", format!("Bearer {bearer}"))
        .send()
        .await
        .context("connect stream")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("stream HTTP {status}: {}", body.trim()));
    }
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("stream chunk")?;
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match parse_stream_line(line) {
                Ok((tweet, username)) => {
                    if tx
                        .send(StreamTweet { tweet, username })
                        .await
                        .is_err()
                    {
                        return Ok(());
                    }
                }
                Err(e) => {
                    eprintln!("x-listener: skip undecodable line ({e}): {line}");
                }
            }
        }
    }
    Ok(())
}

fn resolve_username(payload: &StreamPayload) -> String {
    let Some(author_id) = payload.data.author_id.as_deref() else {
        return String::new();
    };
    let mut by_id: HashMap<&str, &str> = HashMap::new();
    if let Some(inc) = &payload.includes {
        for u in &inc.users {
            by_id.insert(u.id.as_str(), u.username.as_str());
        }
    }
    by_id.get(author_id).copied().unwrap_or("").to_string()
}

/// Parse a single stream JSON line (for fixtures / dry-run).
pub fn parse_stream_line(line: &str) -> Result<(Tweet, String)> {
    let payload: StreamPayload = serde_json::from_str(line).context("parse stream line")?;
    let username = resolve_username(&payload);
    Ok((payload.data, username))
}
