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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    VoteCast(VoteCast),
    ItemUpsert(ItemUpsert),
    TagAdd(TagAdd),
    DslIngested(DslIngested),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteCast {
    /// Unix timestamp in milliseconds.
    pub ts: i64,
    /// Tag without leading '#'.
    pub tag: String,
    /// Aspect without leading ':'.
    pub aspect: String,
    /// Item A without leading '/'.
    pub a: String,
    /// Item B without leading '/'.
    pub b: String,
    /// Preference score in [-50, 50]. Positive means prefer `a`, negative means prefer `b`.
    pub score: i32,
    /// Optional vote explanation/body (from DSL `{ ... }`).
    #[serde(default)]
    pub body: Option<String>,
    /// API key identifier (not secret), for attribution/rate limiting.
    pub voter_key_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemUpsert {
    pub ts: i64,
    /// Item without leading '/'.
    pub item: String,
    /// Optional human-readable description/notes.
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagAdd {
    pub ts: i64,
    /// Tag without leading '#'.
    pub tag: String,
    /// Item without leading '/'.
    pub item: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DslIngested {
    /// Unix timestamp in milliseconds.
    pub ts: i64,
    /// Raw DSL/prose that was ingested.
    pub raw: String,
    /// Tags referenced/active during ingestion (without leading '#').
    pub tags: Vec<String>,
    /// API key identifier (not secret), for attribution/rate limiting.
    pub voter_key_id: String,
}


