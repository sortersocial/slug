//! Map an X post into a `.sorter` ingest body.
//!
//! Everything sticks: tweet URL becomes a garden item; other URLs in the text
//! become items too. Prose carries attribution + the raw tweet.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Tweet {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub author_id: Option<String>,
    #[serde(default)]
    pub entities: Option<TweetEntities>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TweetEntities {
    #[serde(default)]
    pub urls: Vec<UrlEntity>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UrlEntity {
    pub url: String,
    #[serde(default)]
    pub expanded_url: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub display_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MappedIngest {
    pub tweet_id: String,
    pub author: String,
    pub status_url: String,
    pub text: String,
}

/// Build a status URL. Prefer real username; fall back to `i` when unknown.
pub fn status_url(username: &str, tweet_id: &str) -> String {
    let user = if username.is_empty() { "i" } else { username };
    format!("https://x.com/{user}/status/{tweet_id}")
}

/// Expand t.co links in tweet text using entity metadata when present.
pub fn expand_urls(text: &str, entities: Option<&TweetEntities>) -> String {
    let Some(ents) = entities else {
        return text.to_string();
    };
    let mut out = text.to_string();
    for u in &ents.urls {
        let expanded = u
            .expanded_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(u.url.as_str());
        if expanded != u.url {
            out = out.replace(&u.url, expanded);
        }
    }
    out
}

/// Collect expanded http(s) URLs from entities, excluding the tweet's own status URL.
fn external_urls(entities: Option<&TweetEntities>, status: &str) -> Vec<String> {
    let Some(ents) = entities else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for u in &ents.urls {
        let expanded = u
            .expanded_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(u.url.as_str());
        if !expanded.starts_with("http://") && !expanded.starts_with("https://") {
            continue;
        }
        // Skip x.com/twitter.com status self-links and media wrappers that aren't useful items.
        if expanded == status {
            continue;
        }
        if out.iter().any(|x: &String| x == expanded) {
            continue;
        }
        out.push(expanded.to_string());
    }
    out
}

fn escape_item_body(s: &str) -> String {
    // Item bodies are `{ … }`; keep braces literal — DSL parser tolerates nested text in practice
    // when not starting a new path. Prefer stripping unmatched control that breaks fences later.
    s.replace('\r', "")
}

/// Map a tweet (+ resolved username) into sorter text for `RpcCommand::Post`.
pub fn tweet_to_sorter(tweet: &Tweet, username: &str) -> MappedIngest {
    let author = if username.is_empty() {
        tweet
            .author_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        username.trim_start_matches('@').to_string()
    };
    let status = status_url(&author, &tweet.id);
    let body = expand_urls(&tweet.text, tweet.entities.as_ref());
    let body = escape_item_body(&body);
    let extras = external_urls(tweet.entities.as_ref(), &status);

    let mut text = String::new();
    text.push_str(&format!("@{author} on x:\n\n"));
    text.push_str(&body);
    text.push('\n');
    text.push_str(&format!("\n{status} {{\n{body}\n}}\n"));
    for url in extras {
        text.push_str(&format!("\n{url} {{\nfrom @{author} via {status}\n}}\n"));
    }

    MappedIngest {
        tweet_id: tweet.id.clone(),
        author,
        status_url: status,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_plain_tweet() {
        let t = Tweet {
            id: "123".into(),
            text: "hello #slugsocial".into(),
            author_id: Some("99".into()),
            entities: None,
        };
        let m = tweet_to_sorter(&t, "alice");
        assert_eq!(m.status_url, "https://x.com/alice/status/123");
        assert!(m.text.contains("@alice on x:"));
        assert!(m.text.contains("hello #slugsocial"));
        assert!(m.text.contains("https://x.com/alice/status/123 {\nhello #slugsocial\n}"));
    }

    #[test]
    fn expands_tco_and_adds_external_item() {
        let t = Tweet {
            id: "456".into(),
            text: "see https://t.co/abc #slugsocial".into(),
            author_id: None,
            entities: Some(TweetEntities {
                urls: vec![UrlEntity {
                    url: "https://t.co/abc".into(),
                    expanded_url: Some("https://example.com/page".into()),
                    display_url: Some("example.com/page".into()),
                }],
            }),
        };
        let m = tweet_to_sorter(&t, "bob");
        assert!(m.text.contains("see https://example.com/page #slugsocial"));
        assert!(m.text.contains("https://example.com/page {\nfrom @bob via https://x.com/bob/status/456\n}"));
    }

    #[test]
    fn status_url_falls_back_without_username() {
        assert_eq!(status_url("", "1"), "https://x.com/i/status/1");
    }
}
