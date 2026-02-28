use serde::{Deserialize, Serialize};

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
pub struct RankResponse {
    pub aspect: String,
    pub ranking: Vec<RankRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GraphStats {
    /// Minimum number of additional pairwise comparisons needed to make the
    /// comparison graph fully connected (every item reachable from every other).
    pub comparisons_until_connected: usize,
    /// Number of distinct pairs that have been compared so far.
    pub pairs_compared: usize,
    /// Total possible pairs: n*(n-1)/2.
    pub total_pairs: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairResponse {
    pub aspect: String,
    pub left: String,
    pub right: String,
    pub left_body: Option<String>,
    pub right_body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_stats: Option<GraphStats>,
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
    pub aspects: Vec<String>,
    pub recent_ingests: Vec<IngestRow>,
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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecentVotesResponse {
    pub votes: Vec<VoteRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteRow {
    pub ts: i64,
    pub aspect: String,
    pub a: String,
    pub b: String,
    pub ratio: String,
    pub actor: Option<String>,
    pub body: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NotificationsResponse {
    pub ok: bool,
    pub actor: String,
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
        actor: String,
        details: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestRequest {
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    pub events_appended: usize,
    pub next: NextMoves,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckGroup {
    pub aspect: String,
    pub ranking: Vec<RankRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    pub groups: Vec<CheckGroup>,
    pub next: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteRequest {
    pub aspect: String,
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
    pub aspect: String,
    pub ranking: Vec<RankRow>,
    pub next: NextMoves,
}
