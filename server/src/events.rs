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

    // Handle external URLs: store schemeless as host/path
    if let Some(rest) = s.strip_prefix("https://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if tail.is_empty() {
            return host;
        } else {
            return format!("{}/{}", host, tail);
        }
    }
    if let Some(rest) = s.strip_prefix("http://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if tail.is_empty() {
            return host;
        } else {
            return format!("{}/{}", host, tail);
        }
    }

    // Handle local items: ~/path becomes just path
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

    // Return schemeless canonical form
    tail
}

/// Check if a canonical item is an external/away item (contains `.` in first segment).
pub fn item_routes_as_away(canonical: &str) -> bool {
    if canonical.is_empty() {
        return false;
    }
    canonical.split('/').next().map(|seg| seg.contains('.')).unwrap_or(false)
}

/// Reconstruct the HTTPS URL from a canonical item for embeds and external links.
pub fn item_reconstruct_https_url(canonical: &str) -> String {
    if canonical.is_empty() {
        return "https://slug.social".to_string();
    }
    if item_routes_as_away(canonical) {
        // External URL: schemeless canonical -> https://canonical
        format!("https://{}", canonical)
    } else {
        // Local item: path -> https://slug.social/~/path
        format!("https://slug.social/~/{}", canonical)
    }
}

/// Get the API path prefix for an item: `/canonical` for local or `-/canonical` for away.
pub fn item_api_path(canonical: &str) -> String {
    if item_routes_as_away(canonical) {
        format!("-/{}", canonical)
    } else {
        format!("/{}", canonical)
    }
}

pub fn item_path_segments(input: &str) -> Vec<String> {
    let canonical = canonicalize_item(input);
    if canonical.is_empty() {
        return vec![];
    }

    // All canonicals are schemeless now, split by /
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

/// If the canonical path's first segment is a full UUID v4, return it. Otherwise None.
/// Canonical paths for private items look like: "uuid/path/to/item"
pub fn path_owner_uuid(canonical: &str) -> Option<&str> {
    if canonical.is_empty() {
        return None;
    }
    // For external/away items, first segment will contain a dot, so it can't be a UUID
    if item_routes_as_away(canonical) {
        return None;
    }
    let seg = canonical.split('/').next()?;
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


