use serde::{Deserialize, Serialize};

/// Canonical identifiers stored without sigils.
/// - tags are stored without leading '#'
/// - aspects are stored without leading ':'
/// - items are stored without leading '/'
pub fn canonicalize_tag(input: &str) -> String {
    input.trim().trim_start_matches('#').to_lowercase()
}

pub fn canonicalize_aspect(input: &str) -> String {
    input.trim().trim_start_matches(':').to_lowercase()
}

pub fn canonicalize_item(input: &str) -> String {
    input.trim().trim_start_matches('/').to_lowercase()
}

pub fn canonicalize_actor(input: &str) -> String {
    input.trim().trim_start_matches('@').to_lowercase()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Ingest of a .sorter document. All DSL-expressible actions (votes, items, tags)
    /// are inferred from parsing the `raw` field.
    Ingest(Ingest),

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


