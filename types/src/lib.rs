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
    pub tag: String,
    pub aspect: String,
    pub ranking: Vec<RankRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairResponse {
    pub tag: String,
    pub aspect: String,
    pub left: String,
    pub right: String,
    pub left_body: Option<String>,
    pub right_body: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NextMoves {
    pub pair: String,
    pub rank: String,
    pub web: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TagsResponse {
    pub tags: Vec<TagSummary>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TagSummary {
    pub tag: String,
    pub items: usize,
    pub aspects: usize,
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
    pub items: usize,
    pub aspects: usize,
    pub web: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TagDetailResponse {
    pub tag: String,
    pub items: Vec<String>,
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
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecentVotesResponse {
    pub votes: Vec<VoteRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteRow {
    pub ts: i64,
    pub tag: String,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct Notification {
    pub ts: i64,
    pub ingest_id: String,
    pub actor: String,
    #[serde(flatten)]
    pub notification_type: NotificationType,
}

#[derive(Debug, Serialize, Deserialize)]
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
    pub tags: Vec<String>,
    pub events_appended: usize,
    pub next: NextMoves,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckGroup {
    pub tag: String,
    pub aspect: String,
    pub ranking: Vec<RankRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub tags: Vec<String>,
    pub groups: Vec<CheckGroup>,
    pub next: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteRequest {
    /// Hashtag namespace (required).
    pub tag: String,
    pub aspect: String,
    pub a: String,
    pub b: String,
    /// Ratio string like "3:1" (preferred).
    #[serde(default)]
    pub ratio: Option<String>,
    /// Required human explanation for the vote (non-empty).
    pub body: String,
    /// Optional self-declared actor (e.g. "@tommy").
    #[serde(default)]
    pub actor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VoteResponse {
    pub ok: bool,
    pub tag: String,
    pub aspect: String,
    pub ranking: Vec<RankRow>,
    pub next: NextMoves,
}
