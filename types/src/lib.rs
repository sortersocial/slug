use serde::{Deserialize, Serialize};

pub mod paths;
pub mod timeago;

pub use paths::{
    canonicalize_item, canonicalize_tag, item_parent_path, item_path_segments, normalize_slug_ontology_storage_url,
    CanonicalItemUrl, ForumThreadUrl, GardenItemUrl, RelativePath, SLUG_TILDE_ONTOLOGY_ROOT,
    TildeHttpPathTail, TildeOntologyPath, TildePath, tilde_http_path_to_canonical,
};

/// Max characters returned for a garden item body unless `full=true` / `--full` (API + CLI).
pub const MAX_ITEM_BODY_PREVIEW_CHARS: usize = 100_000;

/// Max characters shown for a forum post in HTML and thread RPC until expanded (`expand_post_full`).
pub const MAX_FORUM_POST_PREVIEW_CHARS: usize = 20_000;

#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub ok: bool,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankRow {
    pub item: GardenItemUrl,
    pub score: f64,
    /// Normalized score as a percentage of the top item (0–100). Present when ?percent=true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
}

/// Flat, paginated global ranking across all items regardless of scope.
#[derive(Debug, Serialize, Deserialize)]
pub struct GlobalRankResponse {
    /// Total ranked items (have at least one vote connecting them to another item).
    pub ranked_total: usize,
    /// Total unranked items (exist but have no votes).
    pub unranked_total: usize,
    /// Pagination offset applied.
    pub offset: usize,
    /// Pagination limit applied.
    pub limit: usize,
    /// The page of items: ranked items first (descending score), then unranked (alphabetical).
    pub items: Vec<RankRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RankComponent {
    pub pairs: usize,
    pub ranking: Vec<RankRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RankResponse {
    pub components: Vec<RankComponent>,
    pub unranked_items: Vec<GardenItemUrl>,
}

/// Graph connectivity stats for a scope, returned with pair suggestions.
#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectivityStats {
    /// Total items in scope.
    pub items: usize,
    /// Number of connected components (each isolate counts as one).
    pub components: usize,
    /// Minimum comparisons needed to make the graph fully connected (components - 1).
    pub comparisons_until_connected: usize,
    /// Number of distinct pairs that have been voted on in this scope.
    pub pairs_voted: usize,
    /// Total possible pairs: items * (items - 1) / 2.
    pub pairs_possible: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairResponse {
    pub left: GardenItemUrl,
    pub right: GardenItemUrl,
    pub left_body: Option<String>,
    pub right_body: Option<String>,
    /// Thread tags that discuss either item (connective tissue to forum).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<String>,
    /// Graph connectivity stats for the scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connectivity: Option<ConnectivityStats>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NextMoves {
    pub pair: String,
    pub rank: String,
    pub web: ForumThreadUrl,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathsResponse {
    pub paths: Vec<PathSummary>,
}

/// Leaf items only (no children). For search / "full path list" — does not scale, works for now.
#[derive(Debug, Serialize, Deserialize)]
pub struct LeavesResponse {
    pub paths: Vec<GardenItemUrl>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathSummary {
    pub path: TildeOntologyPath,
    pub children: usize,
    pub web: GardenItemUrl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadsResponse {
    pub threads: Vec<ThreadSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread: String,
    pub last_activity_ts: i64,
    pub web: ForumThreadUrl,
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
    /// Chronological page: prose posts and room system lines, oldest first within the window.
    pub items: Vec<ThreadItem>,
    /// Total rows (posts + system lines) in this thread after filters.
    pub total: usize,
    /// Offset into the merged chronological list.
    pub offset: usize,
}

/// One row in a thread timeline: a normal post or a room system line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadItem {
    Post {
        id: String,
        index: usize,
        ts: i64,
        actor: String,
        body: String,
        truncated: bool,
        /// Author redacted this post; body is empty and garden contributions were removed.
        #[serde(default)]
        redacted: bool,
        /// When the redaction was recorded (ms), if redacted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redacted_at_ts: Option<i64>,
    },
    System {
        ts: i64,
        text: String,
    },
}

/// One post in a thread. Full body, no snippet.
#[derive(Debug, Serialize, Deserialize)]
pub struct PostRow {
    /// Stable ingest ID (UUID). Use with `--post <id>` to fetch this post directly.
    pub id: String,
    /// Chronological index within the thread (0 = oldest).
    pub index: usize,
    pub ts: i64,
    /// Principal username (stored form, no `@`).
    pub actor: String,
    pub body: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestRow {
    pub ts: i64,
    pub actor: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ItemResponse {
    pub item: GardenItemUrl,
    pub body: Option<String>,
    /// True when the body was truncated due to size. Fetch with `?full=true` for the complete body.
    /// The limit is `MAX_ITEM_BODY_PREVIEW_CHARS` in the `slug_types` crate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Total character length of the full body (present when truncated=true).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub body_len: usize,
    /// Thread tags that mention or vote on this item (connective tissue to forum).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<String>,
}

fn is_zero(n: &usize) -> bool { *n == 0 }

#[derive(Debug, Serialize, Deserialize)]
pub struct RecentVotesResponse {
    pub votes: Vec<VoteRow>,
}

/// Vote history for one item (matchup: wins/losses + thread per vote).
#[derive(Debug, Serialize, Deserialize)]
pub struct MatchupResponse {
    pub item: GardenItemUrl,
    pub votes: Vec<VoteRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteRow {
    pub ts: i64,
    pub a: GardenItemUrl,
    pub b: GardenItemUrl,
    pub ratio: String,
    /// Principal username when present (stored form, no `@`).
    pub actor: Option<String>,
    pub body: String,
    /// Thread where this vote was cast (e.g. "#sorting-hat").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

/// Response for the feed endpoint — all ingests since a cutoff, newest first.
#[derive(Debug, Serialize, Deserialize)]
pub struct FeedResponse {
    /// When set, feed cutoff is scoped to this agent delegate (`uuid:rig:provider/model`). Omitted when using principal-wide catch-up (`GetFeed` without `delegate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate: Option<String>,
    /// Lower-bound timestamp (ms): last matching ingest for the chosen scope. None if no prior matching ingest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    pub posts: Vec<FeedPost>,
    pub total: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeedPost {
    pub ts: i64,
    pub id: String,
    /// Primary thread tag (without #), if the ingest declared one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// 1-based display ordinal for this post within the thread (feed only; URLs use 0-based paths).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_post_index: Option<usize>,
    /// Full raw body of the ingest document.
    pub body: String,
}

// ---------------------------------------------------------------------------
// RPC batch API (`POST /api/v0/rpc`)
// ---------------------------------------------------------------------------

fn default_invite_max_uses() -> usize {
    1
}

/// One principal's capabilities in a private room (from [`RpcCommand::RoomAudit`]).
#[derive(Debug, Serialize, Deserialize)]
pub struct RoomAuditEntry {
    pub username: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RoomAuditResponse {
    pub room: String,
    pub grants: Vec<RoomAuditEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RoomListResponse {
    pub rooms: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RpcBatch(pub Vec<RpcCommand>);

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RpcCommand {
    Post {
        room: String,
        thread_tag: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delegate: Option<String>,
        text: String,
        #[serde(default)]
        return_rank_diff: bool,
    },
    Check {
        room: String,
        text: String,
    },
    GetGardenRank {
        room: String,
        parent_path: String,
        #[serde(default)]
        depth: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        percent: Option<bool>,
    },
    GetGardenItem {
        room: String,
        item_path: String,
        #[serde(default)]
        full: Option<bool>,
    },
    GetForumThread {
        room: String,
        thread_tag: String,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_id: Option<String>,
    },
    ListForumThreads {
        room: String,
    },
    RoomCreate {
        slug: String,
    },
    RoomGrant {
        room: String,
        username: String,
        /// Capability names: `view`, `post`, `vote`, `add_item`, `manage`.
        capabilities: Vec<String>,
    },
    RoomRevoke {
        room: String,
        username: String,
        capability: String,
    },
    /// Mint a shareable invite link (24h TTL, stored in memory only until redeemed or expiry).
    RoomMintInvite {
        room: String,
        capabilities: Vec<String>,
        #[serde(default = "default_invite_max_uses")]
        max_uses: usize,
    },
    /// List principals granted access in a room (requires View or Manage).
    RoomAudit {
        room: String,
    },
    /// List rooms the authenticated principal has access to.
    RoomList,
    /// Permanently delete a private room and all of its forum and garden data (requires Manage).
    RoomDelete {
        room: String,
    },
    GetGlobalRank {
        room: String,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
        #[serde(default)]
        percent: Option<bool>,
    },
    GetPair {
        room: String,
        parent_path: String,
    },
    GetMatchup {
        room: String,
        item_path: String,
        #[serde(default)]
        limit: Option<usize>,
    },
    GetRankHistory {
        room: String,
        item_path: String,
    },
    GetLeaves {
        room: String,
    },
    GetPaths {
        room: String,
    },
    GetRecentVotes {
        room: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
    Search {
        query: String,
    },
    GetFeed {
        /// When omitted or empty, feed uses the bearer principal's last ingest as the cutoff (any delegate or none).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delegate: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<i64>,
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Remove the author's post from the garden and replace the body with a tombstone in the thread.
    PostRedact {
        post_id: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum RpcResult {
    PostOk {
        events_appended: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ranking_changes: Option<Vec<ScopeRankChanges>>,
        threads: Vec<String>,
        next: NextMoves,
    },
    CheckOk {
        rankings: Vec<CheckScopeRanking>,
        threads: Vec<String>,
        next: Vec<String>,
    },
    GardenRank(RankResponse),
    GardenItem(ItemResponse),
    ForumThread(ThreadDetailResponse),
    ForumThreads(ThreadsResponse),
    RoomCreated {
        room_id: String,
    },
    RoomInviteMinted {
        invite_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at_ms: Option<i64>,
        max_uses: usize,
    },
    RoomAudit(RoomAuditResponse),
    RoomList(RoomListResponse),
    RoomDeletedOk {},
    GrantOk {},
    GlobalRank(GlobalRankResponse),
    Pair(PairResponse),
    Matchup(MatchupResponse),
    RankHistory(RankHistoryResponse),
    Leaves(LeavesResponse),
    Paths(PathsResponse),
    RecentVotes(RecentVotesResponse),
    Search(SearchResponse),
    Feed(FeedResponse),
    RedactPostOk {},
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcLine {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<RpcResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcBatchResponse {
    pub results: Vec<RpcLine>,
}

/// Start a browser-based OAuth login flow for a CLI agent.
#[derive(Debug, Serialize, Deserialize)]
pub struct PendingSessionStartRequest {
    /// Delegate id: `uuid:rig:provider/model` (no `@`).
    pub agent: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingSessionStartResponse {
    pub session: String,
    pub login_url: String,
    pub poll_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingSessionPollResponse {
    pub ok: bool,
    pub complete: bool,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WhoamiResponse {
    /// Username (stored form, no `@`).
    pub user: String,
    pub agents_bound: usize,
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
    pub item: GardenItemUrl,
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
}

/// One scoped ranking preview for `check` (dry-run), grouped into components.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckScopeRanking {
    /// Parent scope path (e.g. "/models" or "/" for root).
    pub parent: String,
    pub components: Vec<RankComponent>,
    pub unranked_items: Vec<GardenItemUrl>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckResponse {
    pub ok: bool,
    pub threads: Vec<String>,
    /// Ranking previews for each parent scope touched by votes in this doc.
    /// Empty when the doc contains no votes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rankings: Vec<CheckScopeRanking>,
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
    pub path: GardenItemUrl,
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

/// One snapshot in an item's rank history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankHistoryRow {
    pub ts: i64,
    /// 1-indexed rank within the item's parent scope after this ingest. 0 = unranked.
    pub scope_rank: usize,
    /// scope_rank delta vs prior entry (after - before; 0 on first appearance).
    pub scope_rank_delta: i32,
    /// Total items in scope at time of this ingest.
    pub scope_total: usize,
    /// 1-indexed rank globally across all items after this ingest. 0 = not in ranking group.
    pub global_rank: usize,
    /// global_rank delta vs prior entry (after - before; 0 on first appearance).
    pub global_rank_delta: i32,
    /// Total items in the global ranking group at time of this ingest.
    pub global_total: usize,
    pub score: f64,
    /// Thread tag of the ingest that triggered this rank change.
    pub thread: String,
    /// 0-indexed chronological position of this post within the thread (same as `/t/tag/N` routes).
    pub thread_post_index: usize,
    /// Votes from this ingest that directly touched this item. Empty when change was transitive.
    pub caused_by: Vec<VoteRow>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RankHistoryResponse {
    pub item: GardenItemUrl,
    pub history: Vec<RankHistoryRow>,
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
