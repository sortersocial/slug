use serde::{Deserialize, Serialize};

/// Canonical identifiers stored without sigils.
/// - tags are stored without leading '#'
/// - items are stored without leading '/'
pub fn canonicalize_tag(input: &str) -> String {
    input.trim().trim_start_matches('#').to_lowercase()
}

pub fn canonicalize_item(input: &str) -> String {
    let mut s = input.trim();
    if let Some(rest) = s.strip_prefix("~/") {
        s = rest;
    } else if let Some(rest) = s.strip_prefix('/') {
        s = rest;
    }

    s.split('/')
        .filter_map(|seg| {
            let t = seg.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_lowercase())
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub fn item_path_segments(input: &str) -> Vec<String> {
    canonicalize_item(input)
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

/// If the path's first segment is a full UUID v4, return it. Otherwise None.
pub fn path_owner_uuid(path: &str) -> Option<&str> {
    let seg = path.split('/').next()?;
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

    // Future: non-DSL events like payments, subscriptions, etc.
    // Payment { ... },
    // Subscription { ... },
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


