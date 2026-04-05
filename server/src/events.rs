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

/// Canonical username stored without leading '@'.
pub fn canonicalize_username(input: &str) -> String {
    input.trim().trim_start_matches('@').to_lowercase()
}

/// Validate username: lowercase alphanumeric + '-' '_' only; length 1-32.
pub fn validate_username(username: &str) -> Result<(), String> {
    let u = canonicalize_username(username);
    if u.is_empty() || u.len() > 32 {
        return Err("username must be 1-32 characters".to_string());
    }
    if !u
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("username must be lowercase alphanumeric with '-' or '_' only".to_string());
    }
    Ok(())
}

/// Canonical agent identity stored with single leading '@'.
///
/// Request/display form is `@@uuid:rig:provider/model` but we store `@uuid:rig:provider/model`.
pub fn canonicalize_agent(input: &str) -> String {
    let s = input.trim();
    let s = s.strip_prefix("@@").or_else(|| s.strip_prefix('@')).unwrap_or(s);
    format!("@{}", s.to_lowercase())
}

/// Validate agent format: @@<uuid-v4>:<rig>:<provider/model>
pub fn validate_agent_format(agent: &str) -> Result<(), String> {
    let a = agent.trim();
    if !a.starts_with("@@") && !a.starts_with('@') {
        return Err("agent must start with @@".to_string());
    }
    let a = a.strip_prefix("@@").or_else(|| a.strip_prefix('@')).unwrap_or(a);
    let parts: Vec<&str> = a.split(':').collect();
    if parts.len() != 3 {
        return Err("agent must be @@<uuid>:<rig>:<provider/model>".to_string());
    }
    let (uuid_part, rig_part, model_part) = (parts[0], parts[1], parts[2]);
    if uuid::Uuid::parse_str(uuid_part).is_err() {
        return Err("agent uuid must be a valid UUID v4".to_string());
    }
    if rig_part.trim().is_empty() {
        return Err("agent rig must be non-empty".to_string());
    }
    if !model_part.contains('/') {
        return Err("agent model must be <provider/model>".to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadVisibility {
    Public,
    Private,
}

#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadCapability {
    View,
    Post,
    Vote,
    AddItem,
    Manage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    UserRegistered(UserRegistered),
    TokenIssued(TokenIssued),
    AgentBound(AgentBound),
    ThreadCreated(ThreadCreated),
    GrantAdded(GrantAdded),
    GrantRevoked(GrantRevoked),
    /// Ingest of a DSL+prose body. Identity and routing live in event metadata.
    Ingest(Ingest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRegistered {
    pub ts: i64,
    pub username: String,
    pub provider: String,
    pub provider_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenIssued {
    pub ts: i64,
    pub username: String,
    pub token_id: String,
    pub token_hash: String,
    pub salt: String,
    pub issued_via: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentBound {
    pub ts: i64,
    pub agent: String,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadCreated {
    pub ts: i64,
    pub thread_id: String,
    pub slug: String,
    pub owner: String,
    pub visibility: ThreadVisibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantAdded {
    pub ts: i64,
    pub thread_id: String,
    pub username: String,
    pub capabilities: Vec<ThreadCapability>,
    pub granted_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantRevoked {
    pub ts: i64,
    pub thread_id: String,
    pub username: String,
    pub capabilities: Vec<ThreadCapability>,
    pub revoked_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ingest {
    /// Unix timestamp in milliseconds.
    pub ts: i64,
    /// Unique identifier for this ingest (stable reference for threading).
    #[serde(default = "generate_id")]
    pub id: String,
    /// Raw DSL+prose body only (no identity/routing metadata).
    pub raw: String,
    /// Human principal username (no leading '@').
    pub principal: String,
    /// Delegate agent identity (canonical stored with single leading '@').
    pub delegate: String,
    /// Thread identifier: public tag (e.g. "languages") or private id/slug (e.g. "a7f2k9x/project-review").
    pub thread_id: String,
}

fn generate_id() -> String {
    uuid::Uuid::new_v4().to_string()
}


