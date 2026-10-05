//! Score-history replay: the sorter-proof "Ranking Replay 2" experience —
//! one-HTML-file React app (CDN React 18 + babel-standalone + Chart.js +
//! React Query, inline JSX) served as a query-param view of any scope page.
//!
//! - `GET /~/<scope>?v=history`      → the replay app (its own full page)
//! - `GET /~/<scope>?v=history.json` → the sorter-proof-shaped data payload
//!   `{criteria, items, history[], final_matrix}` built from `ContentState`:
//!   per-event snapshots **re-solve the scope's member↔member vote graph**
//!   through the reducer's own kernel (`GroupState::apply_vote` →
//!   `rank_partition`), one honest solve per vote event — never scores mixed
//!   from different solves the way per-item rank-history carry-forward would
//!   mix them. Comparisons and matrix cells quote vote reasonings from
//!   `item_votes`.
//!
//! The flat scope page links here via [`score_history_link_panel`], which is
//! always visible on scopes with members (empty state until history suffices).

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::http::Uri;
use axum::response::{Html, IntoResponse};
use maud::{html, Markup};
use serde::Serialize;

use crate::html::ThreadNav;
use crate::path_types::ItemId;
use crate::reducer::{ContentState, VoteData};
use crate::scope_rank::ChildrenRankings;

use super::item::{item_display_path, item_href};

/// Most vote events replayed (recent window when history is longer).
const MAX_EVENTS: usize = 240;
/// Most members in the replay (chart datasets and matrix rows alike).
const MAX_MEMBERS: usize = 64;

/// Which replay response a `?v=` query selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReplayMode {
    /// `?v=history` — the app page.
    Page,
    /// `?v=history.json` — the data payload the app polls.
    Data,
}

pub(super) fn replay_mode_from_uri(uri: &Uri) -> Option<ReplayMode> {
    uri.query()
        .into_iter()
        .flat_map(|q| q.split('&'))
        .find_map(|pair| pair.strip_prefix("v="))
        .and_then(|v| match v {
            "history" => Some(ReplayMode::Page),
            "history.json" => Some(ReplayMode::Data),
            _ => None,
        })
}

/// The `?by=` voter filter: fork the garden to one voter's canonical votes.
/// Matches the vote's human principal or its full delegate string.
pub(super) fn voter_from_uri(uri: &Uri) -> Option<String> {
    uri.query()
        .into_iter()
        .flat_map(|q| q.split('&'))
        .find_map(|pair| pair.strip_prefix("by="))
        .and_then(|v| urlencoding::decode(v).ok().map(|s| s.into_owned()))
        .filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ReplayRanked {
    item: String,
    score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ReplayComparison {
    left: String,
    right: String,
    score: f64,
    explanation: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ReplayStep {
    timestamp: i64,
    comparison: ReplayComparison,
    current_rankings: Vec<ReplayRanked>,
    unsorted_items: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct MatrixDetails {
    score: f64,
    explanation: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(super) struct MatrixCell {
    #[serde(skip_serializing_if = "is_zero_weight")]
    weight: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<MatrixDetails>,
}

fn is_zero_weight(w: &f64) -> bool {
    *w == 0.0
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ReplayData {
    criteria: String,
    items: Vec<String>,
    history: Vec<ReplayStep>,
    final_matrix: Vec<Vec<MatrixCell>>,
    /// Members omitted from the replay (over [`MAX_MEMBERS`]).
    truncated_members: usize,
    /// The `?by=` voter this replay is filtered to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    voter: Option<String>,
    /// False when there is not enough history to chart (panel shows the
    /// empty state instead of the replay link).
    #[serde(skip_serializing)]
    sufficient: bool,
}

/// `Forward: a / ---- / b / REASONING: body` — the sorter-proof explanation card.
fn format_vote_explanation(content_label: &impl Fn(&ItemId) -> String, v: &VoteData) -> String {
    format!(
        "Forward: {}\n---------\n{}\n\nREASONING:\n{}",
        content_label(&v.a),
        content_label(&v.b),
        if v.body.trim().is_empty() {
            "(no reasoning recorded)"
        } else {
            v.body.trim()
        }
    )
}

/// Signed log-ratio scaled like the prototype's comparison scores
/// (3:1 → +11.0, 3:2 → +4.1, 1:2 → −6.9, 1:1 → 0.0).
fn vote_score(ratio_left: i32, ratio_right: i32) -> f64 {
    let l = (ratio_left.max(1)) as f64;
    let r = (ratio_right.max(1)) as f64;
    let v = (l / r).ln() * 10.0;
    (v * 10.0).round() / 10.0
}

/// Build the replay payload for one scope, or `None` when the scope has no
/// members at all (childless leaf pages get neither panel nor replay).
/// `voter` (the `?by=` filter) forks the replay to one voter's votes.
pub(super) fn build_replay(
    content: &ContentState,
    rankings: &ChildrenRankings,
    scope_item: &str,
    voter: Option<&str>,
) -> Option<ReplayData> {
    // Members in display order: ranked across components, then unranked.
    let mut members: Vec<ItemId> = rankings
        .component_rankings
        .iter()
        .flat_map(|c| c.ranked.iter().map(|r| r.item.ontology_leaf()))
        .collect();
    members.extend(rankings.unranked_items.iter().map(|i| i.ontology_leaf()));
    let mut seen = HashSet::new();
    members.retain(|m| seen.insert(m.clone()));
    if members.is_empty() {
        return None;
    }

    let total_members = members.len();
    let truncated_members = total_members.saturating_sub(MAX_MEMBERS);
    members.truncate(MAX_MEMBERS);
    let member_set: HashSet<&ItemId> = members.iter().collect();
    let label_of = |item: &ItemId| item_display_path(item.as_str());
    let leaf = ItemId::parse(scope_item)
        .map(|id| id.ontology_leaf())
        .unwrap_or_else(|| ItemId::opaque(scope_item.to_string()));
    let criteria = content
        .item_bodies
        .get(&leaf)
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| format!("members of {}", item_display_path(scope_item)));

    // Member↔member canonical votes, each counted once: a vote is recorded in
    // both endpoints' `item_votes` lists, so we take it only from the smaller
    // endpoint's list (multiple votes on the same pair stay distinct events).
    let mut votes_by_ts: BTreeMap<i64, Vec<&VoteData>> = BTreeMap::new();
    for m in &members {
        if let Some(vs) = content.item_votes.get(m) {
            for v in vs {
                let lo = if v.a <= v.b { &v.a } else { &v.b };
                if lo == m && member_set.contains(&v.a) && member_set.contains(&v.b) {
                    if let Some(voter) = voter {
                        if v.principal != voter && v.delegate.as_deref() != Some(voter) {
                            continue;
                        }
                    }
                    votes_by_ts.entry(v.ts).or_default().push(v);
                }
            }
        }
    }
    let all_steps: Vec<i64> = votes_by_ts.keys().copied().collect();
    let steps: &[i64] = if all_steps.len() > MAX_EVENTS {
        &all_steps[all_steps.len() - MAX_EVENTS..]
    } else {
        &all_steps
    };
    let sufficient = members.len() >= 2 && steps.len() >= 2;

    // Replay the scope's own vote graph through the same kernel the reducer
    // uses (`GroupState::apply_vote` → `rank_partition`), solving after every
    // event so each snapshot is one honest solve — never scores mixed from
    // different solves the way per-item rank-history carry-forward would be.
    let mut group = crate::reducer::GroupState::new();
    let mut history: Vec<ReplayStep> = Vec::with_capacity(steps.len());
    for &ts in steps {
        if let Some(step_votes) = votes_by_ts.get(&ts) {
            for v in step_votes {
                group.apply_vote((*v).clone());
            }
        }
        let (mut comps, _) = crate::ranking::connected_components_from_voted_pairs(
            group.idx_to_item.len(),
            group.voted_pairs.iter().copied(),
        );
        comps.sort_by_key(|b| std::cmp::Reverse(b.len()));
        let mut scores: HashMap<ItemId, f64> = HashMap::new();
        for ranked in crate::ranking::rank_partition(&group, &comps, 10000, 1e-8) {
            for r in ranked {
                scores.insert(r.item, r.score);
            }
        }
        let mut current_rankings = Vec::new();
        let mut unsorted_items = Vec::new();
        for m in &members {
            match scores.get(m) {
                Some(&score) => current_rankings.push(ReplayRanked {
                    item: label_of(m),
                    score,
                }),
                None => unsorted_items.push(label_of(m)),
            }
        }
        let step_votes = votes_by_ts.get(&ts).cloned().unwrap_or_default();
        let comparison = match step_votes.first() {
            Some(v) => ReplayComparison {
                left: label_of(&v.a),
                right: label_of(&v.b),
                score: vote_score(v.ratio_left, v.ratio_right),
                explanation: step_votes
                    .iter()
                    .map(|v| format_vote_explanation(&label_of, v))
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n"),
            },
            None => ReplayComparison {
                left: String::new(),
                right: String::new(),
                score: 0.0,
                explanation: "no votes on this scope's members in this event".to_string(),
            },
        };
        history.push(ReplayStep {
            timestamp: ts,
            comparison,
            current_rankings,
            unsorted_items,
        });
    }

    // Pairwise matrix over members: win share + quoted reasoning per edge.
    let n = members.len();
    let mut final_matrix: Vec<Vec<MatrixCell>> = vec![vec![MatrixCell::default(); n]; n];
    for (i, mi) in members.iter().enumerate() {
        let Some(votes) = content.item_votes.get(mi) else {
            continue;
        };
        for (j, mj) in members.iter().enumerate() {
            if i == j {
                continue;
            }
            let edge: Vec<&VoteData> = votes
                .iter()
                .filter(|v| {
                    (&v.a == mi && &v.b == mj) || (&v.a == mj && &v.b == mi)
                })
                .collect();
            if edge.is_empty() {
                continue;
            }
            let shares: Vec<f64> = edge
                .iter()
                .map(|v| {
                    let l = v.ratio_left.max(1) as f64;
                    let r = v.ratio_right.max(1) as f64;
                    if &v.a == mi {
                        l / (l + r)
                    } else {
                        r / (l + r)
                    }
                })
                .collect();
            let mean = shares.iter().sum::<f64>() / shares.len() as f64;
            let clamped = mean.clamp(0.01, 0.99);
            final_matrix[i][j] = MatrixCell {
                weight: (mean * 1000.0).round() / 10.0,
                details: Some(MatrixDetails {
                    score: (((clamped / (1.0 - clamped)).ln() * 10.0) * 10.0).round() / 10.0,
                    explanation: edge
                        .iter()
                        .map(|v| format_vote_explanation(&label_of, v))
                        .collect::<Vec<_>>()
                        .join("\n\n---\n\n"),
                }),
            };
        }
    }

    Some(ReplayData {
        criteria,
        items: members.iter().map(label_of).collect(),
        history,
        final_matrix,
        truncated_members,
        voter: voter.map(str::to_string),
        sufficient,
    })
}

/// The `?v=history.json` payload (`?by=` forks to one voter's votes).
pub(super) fn replay_history_json_response(
    content: &ContentState,
    rankings: &ChildrenRankings,
    scope_item: &str,
    voter: Option<&str>,
) -> axum::response::Response {
    match build_replay(content, rankings, scope_item, voter) {
        Some(data) => axum::Json(serde_json::to_value(data).expect("replay serializes"))
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            "no members in this scope",
        )
            .into_response(),
    }
}

/// HTML-escape for the page `<title>`.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The `?v=history` page: one HTML file, the sorter-proof stack verbatim.
pub(super) fn replay_page_response(
    nav: &ThreadNav,
    scope_item: &str,
    voter: Option<&str>,
) -> axum::response::Response {
    let display = item_display_path(scope_item);
    let back_href = item_href(scope_item, nav);
    let by_query = voter
        .map(|v| format!("&by={}", urlencoding::encode(v)))
        .unwrap_or_default();
    let config = serde_json::json!({
        "dataUrl": format!("{back_href}?v=history.json{by_query}"),
        "backHref": back_href,
        "backLabel": display,
    });
    // serde_json does not escape '<' — keep the config safe inside <script>.
    let config = config.to_string().replace("</", "<\\/");
    let page = REPLAY_HTML
        .replace("__SLUG_TITLE__", &esc(&format!("{display} · score history")))
        .replace("__SLUG_CONFIG__", &config);
    Html(page).into_response()
}

/// The flat-page link bar: always visible on scopes with members. Links to
/// `?v=history` when history suffices; otherwise an empty state naming what
/// it waits for (plus a vote link when there are at least two children).
/// `voter` (the `?by=` filter) forks the counts and carries into the link.
pub(super) fn score_history_link_panel(
    content: &ContentState,
    rankings: &ChildrenRankings,
    nav: &ThreadNav,
    scope_item: &str,
    vote_href: Option<String>,
    voter: Option<&str>,
) -> Option<Markup> {
    let data = build_replay(content, rankings, scope_item, voter)?;
    let by_query = voter
        .map(|v| format!("&by={}", urlencoding::encode(v)))
        .unwrap_or_default();
    let replay_href = format!("{}?v=history{}", item_href(scope_item, nav), by_query);
    Some(if data.sufficient {
        html! {
            div class="ont-history-link" {
                span class="ont-history-link-meta muted" {
                    "score history · "
                    (data.items.len())
                    " members · "
                    (data.history.len())
                    " vote events"
                }
                " "
                a class="ont-history-link-open" href=(replay_href) { "open the replay →" }
            }
        }
    } else {
        html! {
            div class="ont-history-link ont-history-empty" {
                span class="muted" {
                    "score history draws itself once two or more members carry votes"
                }
                @if let Some(href) = &vote_href {
                    " — "
                    a class="ont-vote-children-btn" href=(href.as_str()) { "vote on children" }
                }
            }
        }
    })
}

/// The replay app. Stack, styles, chart config, controls, matrix, and dialog
/// are the sorter-proof "Ranking Replay 2" file adapted only to bind to a
/// fixed data URL (`window.SLUG_REPLAY`) instead of the file picker, plus a
/// back link and an empty-history state.
const REPLAY_HTML: &str = r##"<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>__SLUG_TITLE__</title>

    <!-- External Dependencies (same as sorter-proof) -->
    <script src="https://unpkg.com/react@18/umd/react.development.js"></script>
    <script src="https://unpkg.com/react-dom@18/umd/react-dom.development.js"></script>
    <script src="https://unpkg.com/babel-standalone@6/babel.min.js"></script>
    <script src="https://cdn.jsdelivr.net/npm/chart.js"></script>
    <script src="https://unpkg.com/@tanstack/react-query@4/build/umd/index.production.js"></script>

    <!-- Styles (sorter-proof, verbatim) -->
    <style>
        .container {
            display: flex;
            flex-direction: column;
            max-width: 1200px;
            margin: 0 auto;
        }
        .chart-container {
            width: 100%;
            height: 400px;
            margin: 20px 0;
            display: flex;
            gap: 20px;
        }
        .chart-wrapper {
            flex: 3;
            position: relative;
        }
        .rankings-legend {
            flex: 1;
            padding: 20px;
            border-left: 1px solid #ccc;
            overflow-y: auto;
            max-height: 400px;
        }
        .content {
            display: flex;
        }
        .rankings, .history {
            flex: 1;
            padding: 20px;
            margin: 10px;
            border: 1px solid #ccc;
        }
        .comparison {
            margin-bottom: 20px;
            padding: 10px;
            border-bottom: 1px solid #eee;
        }
        .controls {
            display: flex;
            align-items: center;
            gap: 10px;
            padding: 10px;
        }
        .controls input[type="range"] {
            flex: 1;
        }
        #playPauseBtn {
            width: 40px;
            height: 40px;
            font-size: 20px;
            cursor: pointer;
        }
        .loading { text-align: center; padding: 2rem; }
        .dialog-overlay {
            position: fixed;
            top: 0;
            left: 0;
            right: 0;
            bottom: 0;
            background: rgba(0, 0, 0, 0.5);
            display: flex;
            justify-content: center;
            align-items: center;
            z-index: 1000;
        }
        .dialog-content {
            background: white;
            padding: 20px;
            border-radius: 8px;
            max-width: 500px;
            width: 90%;
        }
    </style>
    <!-- Slug additions: top bar + subtitle only -->
    <style>
        body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 0; }
        .replay-topbar { padding: 10px 14px; border-bottom: 1px solid #ccc; max-width: 1200px; margin: 0 auto; }
        .replay-topbar a { color: #1a0dab; text-decoration: none; }
        .replay-topbar a:hover { text-decoration: underline; }
        .replay-subtitle { color: #666; font-size: 0.9em; padding: 0 14px; max-width: 1200px; margin: 4px auto 0; }
        .comparison-table th, .comparison-table td { max-width: 160px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    </style>
</head>

<body>
    <div id="root"></div>
    <script>window.SLUG_REPLAY = __SLUG_CONFIG__;</script>

    <script type="text/babel">
        const { useState, useEffect, useRef } = React;
        const { QueryClient, QueryClientProvider, useQuery } = ReactQuery;
        const R = window.SLUG_REPLAY;

        const queryClient = new QueryClient();

        function RankingApp() {
            const [currentStep, setCurrentStep] = useState(0);
            const [isPlaying, setIsPlaying] = useState(false);
            const [playbackSpeed, setPlaybackSpeed] = useState(1);
            const chartRef = useRef(null);
            const chartInstance = useRef(null);
            const [dialogData, setDialogData] = useState(null);

            const topbar = (
                <div className="replay-topbar">
                    <a href={R.backHref}>← {R.backLabel}</a>
                </div>
            );

            // Query for replay data
            const { data, isLoading, isError } = useQuery({
                queryKey: ['replayData'],
                queryFn: () =>
                    fetch(R.dataUrl).then(res => res.json()),
                onSuccess: (newData) => {
                    // Only reset to beginning if this is a new file
                    if (!data) {
                        setCurrentStep(0);  // Start at beginning instead of end
                        setIsPlaying(true);
                    } else if (currentStep >= data.history.length - 1) {
                        // If we're at the end, move to the new end
                        setCurrentStep(newData.history.length - 1);
                    }
                },
                refetchInterval: 5000,
                refetchIntervalInBackground: true
            });

            // Chart effect
            useEffect(() => {
                if (data && data.items && chartRef.current) {
                    if (chartInstance.current) {
                        chartInstance.current.destroy();
                    }

                    const ctx = chartRef.current.getContext('2d');
                    chartInstance.current = new Chart(ctx, {
                        type: 'line',
                        data: {
                            labels: [],
                            datasets: data.items.map((item, i) => ({
                                label: item,
                                data: [],
                                borderColor: `hsl(${(i * 360) / data.items.length}, 70%, 50%)`,
                                tension: 0.4
                            }))
                        },
                        options: {
                            responsive: true,
                            maintainAspectRatio: false,
                            animation: false,
                            scales: {
                                y: { beginAtZero: true }
                            },
                            plugins: {
                                legend: {
                                    display: false
                                }
                            },
                            hover: {
                                mode: 'dataset',
                                intersect: false
                            }
                        }
                    });

                    // Call updateChart after initialization
                    updateChart();
                }

                return () => {
                    if (chartInstance.current) {
                        chartInstance.current.destroy();
                    }
                };
            }, [data]);

            // Also add an effect to update chart when currentStep changes
            useEffect(() => {
                updateChart();
            }, [currentStep]);

            // Playback effect
            useEffect(() => {
                if (isPlaying && data && currentStep < data.history.length - 1) {
                    const timer = setTimeout(() => {
                        setCurrentStep(step => step + 1);
                        updateChart();
                    }, 1000 / playbackSpeed);
                    return () => clearTimeout(timer);
                }
            }, [isPlaying, currentStep, data, playbackSpeed]);

            const updateChart = () => {
                if (!data || !chartInstance.current) return;

                const chart = chartInstance.current;

                // Generate labels from 1 to currentStep + 1
                chart.data.labels = Array.from({length: currentStep + 1}, (_, i) => i + 1);

                // Update each dataset with all points up to currentStep
                data.items.forEach((item, i) => {
                    chart.data.datasets[i].data = data.history
                        .slice(0, currentStep + 1)
                        .map(step => {
                            const ranking = step.current_rankings.find(r => r.item === item);
                            return ranking ? ranking.score : null;
                        });
                });

                chart.update();
            };

            // Loading state
            if (isLoading) {
                return <div className="container">{topbar}<div className="loading">Loading...</div></div>;
            }

            // Error state
            if (isError) {
                return <div className="container">{topbar}<div className="loading">Error loading data</div></div>;
            }

            // Empty state
            if (data && (!data.history || data.history.length === 0)) {
                return (
                    <div className="container">
                        {topbar}
                        <div className="loading">no vote events yet in this scope — the replay draws itself as votes land.</div>
                    </div>
                );
            }

            const currentHistory = data && data.history ? data.history[currentStep] : null;

            return (
                <div className="container">
                    {topbar}
                    <div className="replay-subtitle">
                        {data.criteria}
                        {data.voter ? ` · filtered by ${data.voter}` : ''}
                        {data.truncated_members > 0 ? ` · top ${data.items.length} of ${data.items.length + data.truncated_members} members` : ''}
                    </div>

                    <div className="controls">
                        <button id="playPauseBtn" onClick={() => setIsPlaying(!isPlaying)}>
                            {isPlaying ? '⏸' : '⏵'}
                        </button>
                        {data && data.history && (
                            <div>
                                <input
                                    type="range"
                                    min="0"
                                    max={data.history.length - 1}
                                    value={currentStep}
                                    onChange={(e) => setCurrentStep(Number(e.target.value))}
                                />
                                <span>{currentStep} / {data.history.length - 1}</span>
                            </div>
                        )}
                        <div>
                        <input
                            type="range"
                                min="0.25"
                                max="4"
                                step="0.25"
                                value={playbackSpeed}
                                onChange={(e) => setPlaybackSpeed(Number(e.target.value))}
                            />
                            <span>{playbackSpeed}x</span>
                        </div>
                    </div>

                    <div className="chart-container">
                        <div className="chart-wrapper">
                            <canvas ref={chartRef}></canvas>
                        </div>
                        <div className="rankings-legend">
                            <h3>Current Rankings</h3>
                            {currentHistory && currentHistory.current_rankings
                                .slice()
                                .sort((a, b) => b.score - a.score)
                                .map((r, i) => (
                                    <div key={r.item}>
                                        {i+1}. {r.item} ({r.score.toFixed(4)})
                                    </div>
                                ))
                            }
                            <h3>Unsorted Items</h3>
                            {currentHistory && currentHistory.unsorted_items &&
                                currentHistory.unsorted_items.map(item => (
                                    <div key={item}>• {item}</div>
                                ))
                            }
                        </div>
                    </div>

                    <div className="content">
                        <div className="rankings">
                            <h3>Comparison Matrix</h3>
                            {data && data.final_matrix && (
                                <table className="comparison-table" style={{ borderCollapse: 'collapse', fontSize: '0.8em' }}>
                                    <thead>
                                        <tr>
                                            <th></th>
                                            {data.items.map(item => (
                                                <th key={item} style={{ padding: '4px' }}>{item}</th>
                                            ))}
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {data.items.map((row_item, i) => (
                                            <tr key={row_item}>
                                                <td style={{ padding: '4px' }}>{row_item}</td>
                                                {data.final_matrix[i].map((cell, j) => {
                                                    const mirrorCell = data.final_matrix[j][i];
                                                    const details = cell.details || (mirrorCell && mirrorCell.details);

                                                    // Calculate color based on weight
                                                    let backgroundColor = 'white';
                                                    if (cell.weight > 0) {
                                                        if (cell.weight < 50) {
                                                            const intensity = Math.floor((cell.weight / 50) * 255);
                                                            backgroundColor = `rgb(255, ${intensity}, ${intensity})`;
                                                        } else {
                                                            const intensity = Math.floor(((100 - cell.weight) / 50) * 255);
                                                            backgroundColor = `rgb(${intensity}, 255, ${intensity})`;
                                                        }
                                                    }

                                                    return (
                                                        <td key={j} style={{
                                                            padding: '4px',
                                                            border: '1px solid #ccc',
                                                            cursor: details ? 'pointer' : 'default',
                                                            backgroundColor,
                                                            color: 'inherit'
                                                        }}
                                                        onClick={() => {
                                                            if (details) {
                                                                setDialogData(details);
                                                            }
                                                        }}>
                                                            {i === j ? '×' : cell.weight > 0 ? cell.weight.toFixed(1) : ''}
                                                        </td>
                                                    );
                                                })}
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            )}
                        </div>
                    </div>

                    {dialogData && (
                        <div className="dialog-overlay" onClick={() => setDialogData(null)}>
                            <div className="dialog-content" onClick={e => e.stopPropagation()}>
                                <h3>Comparison Details</h3>
                                <p><strong>Score: </strong>{dialogData.score}</p>
                                <p style={{ whiteSpace: 'pre-wrap' }}>{dialogData.explanation}</p>
                                <button onClick={() => setDialogData(null)}>Close</button>
                            </div>
                        </div>
                    )}
                </div>
            );
        }

        function App() {
            return (
                <QueryClientProvider client={queryClient}>
                    <RankingApp />
                </QueryClientProvider>
            );
        }

        const root = ReactDOM.createRoot(document.getElementById('root'));
        root.render(<App />);
    </script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ranking::RankedItem;
    use crate::scope_rank::ScopedComponent;

    fn vote(ts: i64, a: &ItemId, b: &ItemId, l: i32, r: i32, body: &str) -> VoteData {
        VoteData {
            ts,
            a: a.clone(),
            b: b.clone(),
            ratio_left: l,
            ratio_right: r,
            body: body.to_string(),
            principal: "dev".to_string(),
            delegate: None,
            thread_tag: "t".to_string(),
        }
    }

    fn leaf(name: &str) -> ItemId {
        ItemId::parse(name).expect("leaf parses")
    }

    fn rankings_for(names: &[&str], unranked: &[&str]) -> ChildrenRankings {
        ChildrenRankings {
            component_rankings: vec![ScopedComponent {
                pairs: names.len().saturating_sub(1),
                ranked: names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| RankedItem {
                        item: leaf(n),
                        score: 1.0 / (i + 1) as f64,
                    })
                    .collect(),
            }],
            unranked_items: unranked.iter().map(|n| leaf(n)).collect(),
        }
    }

    /// Record each vote in both endpoints' lists, exactly like the reducer.
    fn content_with_votes(votes: Vec<VoteData>) -> ContentState {
        let mut content = ContentState::default();
        for v in votes {
            content.item_votes.entry(v.a.clone()).or_default().push_front(v.clone());
            content.item_votes.entry(v.b.clone()).or_default().push_front(v);
        }
        content
    }

    fn score_of(step: &ReplayStep, item: &str) -> Option<f64> {
        step.current_rankings
            .iter()
            .find(|r| r.item == item)
            .map(|r| r.score)
    }

    #[test]
    fn childless_scope_builds_nothing() {
        let content = ContentState::default();
        let rankings = rankings_for(&[], &[]);
        assert!(build_replay(&content, &rankings, "~scope", None).is_none());
    }

    #[test]
    fn single_event_is_not_yet_a_replay() {
        let content = content_with_votes(vec![vote(1, &leaf("~a"), &leaf("~b"), 2, 1, "a over b")]);
        let rankings = rankings_for(&["~a", "~b"], &[]);
        let data = build_replay(&content, &rankings, "~scope", None).expect("builds");
        assert!(!data.sufficient, "one vote event is not a replay");
        assert_eq!(data.history.len(), 1);
    }

    #[test]
    fn snapshots_are_fresh_solves_and_laggards_stay_unsorted() {
        let content = content_with_votes(vec![
            vote(1, &leaf("~a"), &leaf("~b"), 2, 1, "a over b"),
            vote(2, &leaf("~b"), &leaf("~c"), 2, 1, "b over c"),
            vote(3, &leaf("~a"), &leaf("~c"), 3, 1, "a over c"),
        ]);
        let rankings = rankings_for(&["~a", "~b", "~c"], &[]);
        let data = build_replay(&content, &rankings, "~scope", None).expect("builds");
        assert!(data.sufficient);
        assert_eq!(data.history.len(), 3);

        // Step 0: only the first pair is connected; the third item waits unsorted.
        let step0 = &data.history[0];
        assert_eq!(step0.unsorted_items, vec!["~/c".to_string()]);
        let (a0, b0) = (score_of(step0, "~/a").unwrap(), score_of(step0, "~/b").unwrap());
        assert!(a0 > b0, "2:1 winner leads the fresh solve");
        assert!((a0 + b0 - 1.0).abs() < 0.05, "component mass sums to one");
        assert!((data.history[0].comparison.score - 6.9).abs() < 0.05);

        // Final step: the whole chain solved, order follows the vote directions.
        let last = data.history.last().unwrap();
        assert!(last.unsorted_items.is_empty());
        let (a, b, c) = (
            score_of(last, "~/a").unwrap(),
            score_of(last, "~/b").unwrap(),
            score_of(last, "~/c").unwrap(),
        );
        assert!(a > b && b > c, "a > b > c after the full replay: {a} {b} {c}");
    }

    #[test]
    fn matrix_weight_and_reasoning_and_dedupe() {
        // Two votes on the same pair must both count (the kernel adds them),
        // while the mirror copy in the opponent's list must not double them.
        let content = content_with_votes(vec![
            vote(1, &leaf("~a"), &leaf("~b"), 3, 1, "a clearly outranks b"),
            vote(2, &leaf("~a"), &leaf("~b"), 2, 1, "a still outranks b"),
        ]);
        let rankings = rankings_for(&["~a", "~b"], &[]);
        let data = build_replay(&content, &rankings, "~scope", None).expect("builds");
        assert_eq!(data.history.len(), 2, "two events, no dedupe collapse");

        // mean(75%, 66.7%) win share for a over b.
        let ab = &data.final_matrix[0][1];
        assert!((ab.weight - 70.8).abs() < 0.1, "weight: {}", ab.weight);
        let details = ab.details.as_ref().expect("details");
        assert_eq!(
            details.explanation.matches("REASONING:").count(),
            2,
            "both votes quoted"
        );
        assert!(details.explanation.contains("a clearly outranks b"));
    }

    #[test]
    fn member_and_event_caps_bite() {
        let mut votes = Vec::new();
        for t in 0..300 {
            votes.push(vote(t, &leaf("~m00"), &leaf("~m01"), 2, 1, "again"));
        }
        let content = content_with_votes(votes);
        let names: Vec<String> = (0..70).map(|i| format!("~m{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let rankings = rankings_for(&refs, &[]);
        let data = build_replay(&content, &rankings, "~scope", None).expect("builds");
        assert_eq!(data.items.len(), MAX_MEMBERS);
        assert_eq!(data.truncated_members, 70 - MAX_MEMBERS);
        assert_eq!(data.history.len(), MAX_EVENTS);
        assert_eq!(data.final_matrix.len(), MAX_MEMBERS);
    }

    #[test]
    fn voter_filter_forks_the_replay() {
        let mut alice_vote = vote(1, &leaf("~a"), &leaf("~b"), 3, 1, "alice loves a");
        alice_vote.principal = "alice".to_string();
        let mut bob_vote = vote(2, &leaf("~b"), &leaf("~a"), 3, 1, "bob loves b");
        bob_vote.principal = "bob".to_string();
        let content = content_with_votes(vec![alice_vote, bob_vote]);
        let rankings = rankings_for(&["~a", "~b"], &[]);

        let canon = build_replay(&content, &rankings, "~scope", None).expect("builds");
        assert_eq!(canon.history.len(), 2, "both votes in the canonical replay");
        assert!(canon.voter.is_none());

        let alice = build_replay(&content, &rankings, "~scope", Some("alice")).expect("builds");
        assert_eq!(alice.voter.as_deref(), Some("alice"));
        assert_eq!(alice.history.len(), 1, "only alice's vote survives");
        let a = score_of(&alice.history[0], "~/a").unwrap();
        let b = score_of(&alice.history[0], "~/b").unwrap();
        assert!(a > b, "alice's fork crowns a");

        let bob = build_replay(&content, &rankings, "~scope", Some("bob")).expect("builds");
        let a = score_of(&bob.history[0], "~/a").unwrap();
        let b = score_of(&bob.history[0], "~/b").unwrap();
        assert!(b > a, "bob's fork crowns b");

        let nobody =
            build_replay(&content, &rankings, "~scope", Some("mallory")).expect("builds");
        assert!(nobody.history.is_empty(), "unknown voter gets an empty replay");
        assert!(!nobody.sufficient);
    }

    #[test]
    fn replay_mode_parsing() {
        let page: Uri = "/~/topic?v=history".parse().unwrap();
        assert_eq!(replay_mode_from_uri(&page), Some(ReplayMode::Page));
        let data: Uri = "/~/topic?v=history.json".parse().unwrap();
        assert_eq!(replay_mode_from_uri(&data), Some(ReplayMode::Data));
        let depth: Uri = "/~/topic?depth=2".parse().unwrap();
        assert_eq!(replay_mode_from_uri(&depth), None);
        let both: Uri = "/~/topic?depth=2&v=history".parse().unwrap();
        assert_eq!(replay_mode_from_uri(&both), Some(ReplayMode::Page));
    }
}
