use serde::{Deserialize, Serialize};

/// Canonical identifiers stored without sigils.
/// - tags are stored without leading '#'
/// - items are stored without leading '/'
pub fn canonicalize_tag(input: &str) -> String {
    input.trim().trim_start_matches('#').to_lowercase()
}

pub fn canonicalize_item(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }

    if let Some(rest) = s.strip_prefix("https://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if tail.is_empty() {
            return format!("https://{}", host);
        } else {
            return format!("https://{}/{}", host, tail);
        }
    }
    if let Some(rest) = s.strip_prefix("http://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if tail.is_empty() {
            return format!("http://{}", host);
        } else {
            return format!("http://{}/{}", host, tail);
        }
    }

    let is_tilde = s.starts_with("~/");
    let rest = s.strip_prefix("~/").or_else(|| s.strip_prefix("/")).unwrap_or(s);

    let tail = rest
        .split('/')
        .filter_map(|seg| {
            let t = seg.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_lowercase())
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    if is_tilde {
        format!("https://slug.social/~/{}", tail)
    } else if tail.is_empty() {
        "https://slug.social".to_string()
    } else {
        format!("https://slug.social/{}", tail)
    }
}

pub fn item_path_segments(input: &str) -> Vec<String> {
    let canonical = canonicalize_item(input);
    if canonical.is_empty() {
        return vec![];
    }

    if let Some(rest) = canonical.strip_prefix("https://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let mut out = vec![format!("https://{}", host)];
        out.extend(tail.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()));
        return out;
    }
    if let Some(rest) = canonical.strip_prefix("http://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let mut out = vec![format!("http://{}", host)];
        out.extend(tail.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()));
        return out;
    }

    // Should be unreachable since all canonical items are now URLs
    canonical
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

pub fn item_parent_path(input: &str) -> Option<String> {
    let segs = item_path_segments(input);
    if segs.len() <= 1 {
        return None;
    }
    Some(segs[..segs.len() - 1].join("/"))
}

pub fn canonicalize_actor(input: &str) -> String {
    input.trim().trim_start_matches('@').to_lowercase()
}

/// Returns true if the path's first segment is a full UUID v4 (private namespace).
pub fn is_private_path(path: &str) -> bool {
    path_owner_uuid(path).is_some()
}

/// If the path's first segment after the ~ is a full UUID v4, return it. Otherwise None.
pub fn path_owner_uuid(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("https://slug.social/~/")?;
    let seg = rest.split('/').next()?;
    if uuid::Uuid::parse_str(seg).is_ok() { Some(seg) } else { None }
}

/// Extract the UUID part from a canonicalized actor string (uuid:rig:model).
pub fn actor_uuid(actor: &str) -> &str {
    actor.split(':').next().unwrap_or(actor)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Ingest of a .sorter document. All DSL-expressible actions (votes, items, tags)
    /// are inferred from parsing the `raw` field.
    Ingest(Ingest),

    /// First-come-first-serve passkey registration for an actor.
    /// The passkey itself is never stored — only the hex-encoded SHA-256 hash.
    ActorKeyRegistration {
        ts: i64,
        actor: String,
        key_hash: String,
    },

    /// X (Twitter) mention ingested by the bot. Records provenance (handle,
    /// follower count at mention time, tweet ID) alongside the actor identity.
    /// Follower count is displayed provenance only — it does not affect vote weight.
    XMention {
        ts: i64,
        actor: String,
        x_user_id: String,
        x_handle: String,
        followers: u64,
        tweet_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ingest {
    /// Unix timestamp in milliseconds.
    pub ts: i64,
    /// Unique identifier for this ingest (stable reference for threading).
    #[serde(default = "generate_id")]
    pub id: String,
    /// Raw DSL/prose that was ingested.
    pub raw: String,
    /// API key identifier (not secret), for attribution/rate limiting.
    pub voter_key_id: String,
    /// Self-declared actor (from DSL `@name` or CLI `--as @name`).
    /// Required for notification and attribution.
    pub actor: String,
}

fn generate_id() -> String {
    uuid::Uuid::new_v4().to_string()
}


