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
    VoteCast(VoteCast),
    ItemUpsert(ItemUpsert),
    TagAdd(TagAdd),
    DslIngested(DslIngested),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoteCast {
    /// Unix timestamp in milliseconds.
    pub ts: i64,
    /// Namespace without leading '#'. (Historically a tag; now often a user/space.)
    pub tag: String,
    /// Aspect without leading ':'.
    pub aspect: String,
    /// Item A without leading '/'.
    pub a: String,
    /// Item B without leading '/'.
    pub b: String,
    /// Ratio numerator for `a` (left item). Example: `/a 3:1 /b` => `ratio_left=3`.
    pub ratio_left: i32,
    /// Ratio numerator for `b` (right item). Example: `/a 3:1 /b` => `ratio_right=1`.
    pub ratio_right: i32,
    /// Vote explanation/body (from DSL `{ ... }` or CLI/API `body`).
    ///
    /// Required: votes without explanations are rejected (opaque/unhelpful).
    pub body: String,
    /// API key identifier (not secret), for attribution/rate limiting.
    pub voter_key_id: String,
    /// Optional self-declared actor (from DSL `@name` or CLI `--as @name`).
    #[serde(default)]
    pub actor: Option<String>,
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


