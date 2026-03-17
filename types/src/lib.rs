use serde::{Deserialize, Serialize};

pub mod timeago;

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub ok: bool,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RankRow {
    pub item: String,
    pub score: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RankComponent {
    pub pairs: usize,
    pub ranking: Vec<RankRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RankResponse {
    pub components: Vec<RankComponent>,
    pub unranked_items: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairResponse {
    pub left: String,
    pub right: String,
    pub left_body: Option<String>,
    pub right_body: Option<String>,
    /// Thread tags that discuss either item (connective tissue to forum).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NextMoves {
    pub pair: String,
    pub rank: String,
    pub web: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathsResponse {
    pub paths: Vec<PathSummary>,
}

/// Leaf items only (no children). For search / "full path list" — does not scale, works for now.
#[derive(Debug, Serialize, Deserialize)]
pub struct LeavesResponse {
    pub paths: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathSummary {
    pub path: String,
    pub children: usize,
    pub web: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadsResponse {
    pub threads: Vec<ThreadSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread: String,
    pub last_activity_ts: i64,
    pub subscriber_count: usize,
    pub web: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathDetailResponse {
    pub path: String,
    pub children: Vec<String>,
    pub recent_ingests: Vec<IngestRow>,
}

/// Thread detail with pagination.
#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadDetailResponse {
    pub thread: String,
    pub posts: Vec<PostRow>,
    /// Total posts in this thread.
    pub total: usize,
    /// Chronological offset of the first post in this page.
    pub offset: usize,
}

/// One post in a thread. Full body, no snippet.
#[derive(Debug, Serialize, Deserialize)]
pub struct PostRow {
    /// Stable ingest ID (UUID). Use with `--post <id>` to fetch this post directly.
    pub id: String,
    /// Chronological index within the thread (0 = oldest).
    pub index: usize,
    pub ts: i64,
    /// Self-declared actor (`@uuid:rig:model`).
    pub actor: String,
    pub voter_key_id: String,
    pub body: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestRow {
    pub ts: i64,
    pub actor: Option<String>,
    pub voter_key_id: String,
    pub snippet: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ItemResponse {
    pub item: String,
    pub body: Option<String>,
    /// Thread tags that mention or vote on this item (connective tissue to forum).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecentVotesResponse {
    pub votes: Vec<VoteRow>,
}

/// Vote history for one item (matchup: wins/losses + thread per vote).
#[derive(Debug, Serialize, Deserialize)]
pub struct MatchupResponse {
    pub item: String,
    pub votes: Vec<VoteRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteRow {
    pub ts: i64,
    pub a: String,
    pub b: String,
    pub ratio: String,
    pub actor: Option<String>,
    pub body: String,
    /// Thread where this vote was cast (e.g. "#sorting-hat").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NotificationsResponse {
    pub ok: bool,
    pub actor: String,
    pub notifications: Vec<Notification>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DigestResponse {
    pub ok: bool,
    pub actor: String,
    /// The timestamp used as the lower bound (actor's last ingest, ms). None if actor has never posted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    pub notifications: Vec<Notification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub ts: i64,
    pub ingest_id: String,
    pub actor: String,
    #[serde(flatten)]
    pub notification_type: NotificationType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotificationType {
    ThreadActivity {
        thread: String,
        activity: String,
        details: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestRequest {
    pub text: String,
    /// Passkey for the actor (alternative to X-Slug-Passkey header; header takes precedence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passkey: Option<String>,
}

/// One item's position within a ranking component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankPosition {
    /// 1-indexed rank within the component.
    pub rank: usize,
    /// Total items in this component.
    pub of: usize,
}

/// How one item's rank changed after a vote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankChange {
    pub item: String,
    /// Position before the vote. None = was unranked (no voted connections in this scope).
    pub before: Option<RankPosition>,
    /// Position after the vote. None = became unranked (e.g. component split, unlikely).
    pub after: Option<RankPosition>,
}

/// Ranking changes for all items in one parent scope after a vote submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeRankChanges {
    /// Parent scope path (e.g. "/models" or "/" for root).
    pub parent: String,
    /// Items whose position changed, or that entered the ranking for the first time.
    pub changes: Vec<RankChange>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    pub events_appended: usize,
    pub next: NextMoves,
    /// Ranking position changes caused by votes in this ingest, grouped by parent scope.
    /// Empty when the ingest contained no votes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranking_changes: Vec<ScopeRankChanges>,
    /// True when this ingest registered a new passkey for the actor.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub registered: bool,
    /// The server-generated passkey for this actor, present only on first ingest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passkey: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    pub ranking: Vec<RankRow>,
    pub next: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    pub items: Vec<SearchItemHit>,
    pub threads: Vec<SearchThreadHit>,
    pub posts: Vec<SearchPostHit>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchItemHit {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchThreadHit {
    pub tag: String,
    pub post_count: usize,
    pub last_activity: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SearchPostHit {
    pub thread: String,
    pub actor: String,
    pub snippet: String,
    pub ts: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteRequest {
    pub a: String,
    pub b: String,
    #[serde(default)]
    pub ratio: Option<String>,
    pub body: String,
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteResponse {
    pub ok: bool,
    pub ranking: Vec<RankRow>,
    pub next: NextMoves,
}
