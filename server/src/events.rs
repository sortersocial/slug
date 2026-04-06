use serde::{Deserialize, Serialize};

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
    RoomCreated(RoomCreated),
    GrantAdded(GrantAdded),
    GrantRevoked(GrantRevoked),
    InviteMinted(InviteMinted),
    InviteRedeemed(InviteRedeemed),
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
pub struct RoomCreated {
    pub ts: i64,
    pub room_id: String,
    pub slug: String,
    pub owner: String,
    pub visibility: ThreadVisibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantAdded {
    pub ts: i64,
    pub room_id: String,
    pub username: String,
    pub capabilities: Vec<ThreadCapability>,
    pub granted_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrantRevoked {
    pub ts: i64,
    pub room_id: String,
    pub username: String,
    pub capabilities: Vec<ThreadCapability>,
    pub revoked_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InviteMinted {
    pub ts: i64,
    pub token: String,
    pub room_id: String,
    pub capabilities: Vec<ThreadCapability>,
    pub inviter: String,
    pub max_uses: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InviteRedeemed {
    pub ts: i64,
    pub token: String,
    pub username: String,
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
    /// Human principal username (wire and storage: no `@`).
    pub principal: String,
    /// AI delegate id `uuid:rig:model` (wire and storage: no `@`). Omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate: Option<String>,
    /// Permission boundary: `"public"` or private room id (`shortid/slug`).
    pub room_id: String,
    /// Forum channel within the room (e.g. `languages`).
    pub thread_tag: String,
}

fn generate_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
