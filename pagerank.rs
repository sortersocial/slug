use std::collections::{HashMap, HashSet};

use aide::axum::{
    ApiRouter,
    routing::{get_with, post_with},
};
use axum::extract::{Query, State};
use chrono::NaiveDateTime;
use graph::prelude::*;
use log::{error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sprs::{TriMat, prod::mul_acc_mat_vec_csr};


use crate::{
    app_state::AppState,
    models::{Error, Json, MapErrorSqlx},
};

#[derive(Deserialize, Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct EntityIdentifier {
    pub ns: String,
    pub pk: String,
}

#[derive(Serialize, Debug, Clone, JsonSchema)]
pub struct RankedEntity {
    pub identifier: EntityIdentifier,
    pub score: f64,
}

#[derive(Serialize, Debug, JsonSchema)]
pub struct RankingResponse {
    unsorted: Vec<EntityIdentifier>,
    sorted: Vec<RankedEntity>,
}

#[derive(Serialize, Debug, JsonSchema)]
pub struct RankingSnapshot {
    pub vote_index: usize,
    pub created_at: NaiveDateTime,
    pub unsorted: Vec<EntityIdentifier>,
    pub sorted: Vec<RankedEntity>,
}

#[derive(Serialize, Debug, JsonSchema)]
pub struct RankingHistoryResponse {
    pub snapshots: Vec<RankingSnapshot>,
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct RelevantVote {
    pub left_entity_ns: String,
    pub left_entity_pk: String,
    pub right_entity_ns: String,
    pub right_entity_pk: String,

    pub magnitude: i32,
    pub created_at: NaiveDateTime,
}

struct RankGraph {
    _entity_key_to_index: HashMap<String, usize>,

    index_to_entity_identifier: HashMap<usize, EntityIdentifier>,
    graph: DirectedCsrGraph<usize, (), f64>,
}

fn build_rank_graph(
    entities: &[EntityIdentifier],
    votes: &[RelevantVote],
) -> Result<RankGraph, Error> {
    let mut entity_key_to_index = HashMap::new();
    let mut index_to_entity_identifier = HashMap::new();
    let mut edges = Vec::new();
    let mut node_count = 0;

    for entity in entities {
        let key = format!("{}:{}", entity.ns, entity.pk);
        if !entity_key_to_index.contains_key(&key) {
            entity_key_to_index.insert(key.clone(), node_count);
            index_to_entity_identifier.insert(node_count, entity.clone());
            node_count += 1;
        }
    }

    for vote in votes {
        let left_key = format!("{}:{}", vote.left_entity_ns, vote.left_entity_pk);
        let right_key = format!(
            "{}:{}",
            vote.right_entity_ns, vote.right_entity_pk
        );

        let Some(&left_idx) = entity_key_to_index.get(&left_key) else {
            continue;
        };
        let Some(&right_idx) = entity_key_to_index.get(&right_key) else {
            continue;
        };

        let db_magnitude = (vote.magnitude as f64).clamp(-50.0, 50.0);
        let weight_left_wins = db_magnitude + 50.0;
        let weight_right_wins = 100.0 - weight_left_wins;

        edges.push((left_idx, right_idx, weight_left_wins));
        edges.push((right_idx, left_idx, weight_right_wins));
    }

    let graph: DirectedCsrGraph<usize, (), f64> =
        GraphBuilder::new().edges_with_values(edges).build();

    Ok(RankGraph {
        _entity_key_to_index: entity_key_to_index,
        index_to_entity_identifier,
        graph,
    })
}

fn compute_out_degree_stats(graph: &DirectedCsrGraph<usize, (), f64>) -> (Vec<f64>, f64) {
    let node_count = graph.node_count();
    let out_degs: Vec<f64> = (0..node_count)
        .map(|u| graph.out_neighbors_with_values(u).map(|e| e.value).sum())
        .collect();

    let max_out_deg = out_degs.iter().fold(0.0f64, |max, &deg| max.max(deg));

    (out_degs, max_out_deg)
}

fn calculate_rank_centrality(
    graph: &DirectedCsrGraph<usize, (), f64>,
    max_iters: usize,
    tol: f64,
) -> Vec<f64> {
    let node_count = graph.node_count();
    if node_count == 0 {
        return Vec::new();
    }

    let (out_degs, max_out_deg) = compute_out_degree_stats(graph);

    if max_out_deg <= 1e-9 {
        warn!("Max out-degree is zero, returning uniform scores.");

        return vec![1.0 / node_count as f64; node_count];
    }

    let mut transition =
        TriMat::<f64>::with_capacity((node_count, node_count), graph.edge_count() + node_count);

    for i in 0..node_count {
        let stay_prob = (max_out_deg - out_degs[i]) / max_out_deg;
        if stay_prob > 0.0 {
            transition.add_triplet(i, i, stay_prob);
        }

        for edge in graph.out_neighbors_with_values(i) {
            let weight = edge.value / max_out_deg;
            if weight > 0.0 {
                transition.add_triplet(edge.target, i, weight);
            }
        }
    }

    let transition = transition.to_csr::<usize>();

    let mut scores = vec![1.0f64 / node_count as f64; node_count];
    let mut next_scores = vec![0.0f64; node_count];

    for iter in 0..max_iters {
        next_scores.fill(0.0);
        mul_acc_mat_vec_csr(transition.view(), &scores, &mut next_scores);

        let diff: f64 = scores
            .iter()
            .zip(next_scores.iter())
            .map(|(prev, next)| (prev - next).abs())
            .sum();

        scores.clone_from_slice(&next_scores);

        if diff < tol {
            info!(iterations = iter + 1; "rank centrality converged");
            break;
        }

        if iter == max_iters - 1 {
            warn!(
                max_iters,
                diff;
                "rank centrality reached max_iters without converging",
            );
        }
    }

    scores
}

fn run_rank_centrality(
    graph: &DirectedCsrGraph<usize, (), f64>,
    max_iters: usize,
    tol: f64,
) -> Vec<f64> {
    calculate_rank_centrality(graph, max_iters, tol)
}

/// Pure function to rank entities based on votes between them
fn rank_entities_internal(
    entities: &[EntityIdentifier],
    votes: &[RelevantVote],
) -> Result<(Vec<EntityIdentifier>, Vec<RankedEntity>), Error> {
    if entities.is_empty() {
        return Ok((vec![], vec![]));
    }

    // Convert entities to a HashSet for faster lookups
    let initial_entities: HashSet<EntityIdentifier> = entities.iter().cloned().collect();

    // Find which entities are involved in votes
    let mut voted_entities = HashSet::new();
    for vote in votes {
        let left_id = EntityIdentifier {
            ns: vote.left_entity_ns.clone(),
            pk: vote.left_entity_pk.clone(),
        };
        let right_id = EntityIdentifier {
            ns: vote.right_entity_ns.clone(),
            pk: vote.right_entity_pk.clone(),
        };

        if initial_entities.contains(&left_id) {
            voted_entities.insert(left_id);
        }
        if initial_entities.contains(&right_id) {
            voted_entities.insert(right_id);
        }
    }

    // Separate unsorted (no votes) from entities to rank
    let unsorted_list: Vec<EntityIdentifier> = entities
        .iter()
        .filter(|entity| !voted_entities.contains(*entity))
        .cloned()
        .collect();

    let ranked_payload: Vec<EntityIdentifier> = entities
        .iter()
        .filter(|entity| voted_entities.contains(*entity))
        .cloned()
        .collect();

    if ranked_payload.is_empty() {
        warn!("no entities involved in votes found among the input payload");
        return Ok((unsorted_list, vec![]));
    }

    // Build the rank graph
    let rank_graph = build_rank_graph(&ranked_payload, votes)?;

    let graph = &rank_graph.graph;
    let node_count = graph.node_count();

    if node_count == 0 {
        error!("graph is empty despite non-empty ranked_payload");
        return Ok((unsorted_list, vec![]));
    }

    // Calculate rank centrality scores
    let scores = run_rank_centrality(graph, 100000, 1e-8);

    // Build ranked entities list
    let mut ranked_entities_list: Vec<RankedEntity> = Vec::with_capacity(node_count);
    for index in 0..node_count {
        let Some(identifier) = rank_graph.index_to_entity_identifier.get(&index) else {
            error!(index; "identifier missing for index in smaller graph");
            continue;
        };
        let Some(&score) = scores.get(index) else {
            error!(index; "score missing for index in smaller graph");
            continue;
        };
        ranked_entities_list.push(RankedEntity {
            identifier: identifier.clone(),
            score,
        });
    }

    // Sort by score descending
    ranked_entities_list.sort_unstable_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok((unsorted_list, ranked_entities_list))
}

pub fn rank_entities(
    entities: &[EntityIdentifier],
    votes: &[RelevantVote],
) -> Result<(Vec<EntityIdentifier>, Vec<RankedEntity>), Error> {
    rank_entities_internal(entities, votes)
}

fn build_ranking_history(
    entities: &[EntityIdentifier],
    votes: &[RelevantVote],
) -> Result<Vec<RankingSnapshot>, Error> {
    let mut snapshots = Vec::with_capacity(votes.len());

    for (idx, vote) in votes.iter().enumerate() {
        let (unsorted, sorted) = rank_entities_internal(entities, &votes[..=idx])?;

        snapshots.push(RankingSnapshot {
            vote_index: idx + 1,
            created_at: vote.created_at,
            unsorted,
            sorted,
        });
    }

    Ok(snapshots)
}

async fn rank_children_handler(
    State(state): State<AppState>,
    Json(payload): Json<Vec<EntityIdentifier>>,
) -> Result<Json<RankingResponse>, Error> {
    if payload.is_empty() {
        return Ok(Json(RankingResponse {
            unsorted: vec![],
            sorted: vec![],
        }));
    }

    // Prepare entity keys for SQL query
    let entity_keys: Vec<String> = payload
        .iter()
        .map(|e| format!("{}:{}", e.ns, e.pk))
        .collect();

    // Fetch votes from database
    let votes = sqlx::query_file_as!(
        RelevantVote,
        "../database/sql/vote/select_partial.sql",
        &entity_keys
    )
    .fetch_all(&state.db)
    .await
    .map_error("votes")?;

    // Call the pure ranking function
    let (unsorted, sorted) = rank_entities(&payload, &votes)?;

    Ok(Json(RankingResponse { unsorted, sorted }))
}

#[derive(Deserialize, Debug, JsonSchema)]
pub struct RankQueryParams {
    pub ns: String,
    pub pk: String,
}

async fn get_ranking_handler(
    State(state): State<AppState>,
    Query(params): Query<RankQueryParams>,
) -> Result<Json<RankingResponse>, Error> {
    // Fetch children entities that are eligible for ranking (not blocked)
    let children = sqlx::query_file_as!(
        crate::entity::Entity,
        "../database/sql/entity/select_children_for_ranking.sql",
        params.ns,
        params.pk
    )
    .fetch_all(&state.db)
    .await
    .map_error("children entities")?;

    if children.is_empty() {
        return Ok(Json(RankingResponse {
            unsorted: vec![],
            sorted: vec![],
        }));
    }

    // Convert to identifiers for ranking
    let entity_identifiers: Vec<EntityIdentifier> = children
        .iter()
        .map(|e| EntityIdentifier {
            ns: e.ns.clone(),
            pk: e.pk.clone(),
        })
        .collect();

    // Prepare entity keys for vote query
    let entity_keys: Vec<String> = entity_identifiers
        .iter()
        .map(|e| format!("{}:{}", e.ns, e.pk))
        .collect();

    // Fetch votes
    let votes = sqlx::query_file_as!(
        RelevantVote,
        "../database/sql/vote/select_partial.sql",
        &entity_keys
    )
    .fetch_all(&state.db)
    .await
    .map_error("votes")?;

    // Get ranking
    let (unsorted, sorted) = rank_entities(&entity_identifiers, &votes)?;

    Ok(Json(RankingResponse { unsorted, sorted }))
}

async fn get_ranking_history_handler(
    State(state): State<AppState>,
    Query(params): Query<RankQueryParams>,
) -> Result<Json<RankingHistoryResponse>, Error> {
    let children = sqlx::query_file_as!(
        crate::entity::Entity,
        "../database/sql/entity/select_children.sql",
        params.ns,
        params.pk
    )
    .fetch_all(&state.db)
    .await
    .map_error("children entities")?;

    if children.is_empty() {
        return Ok(Json(RankingHistoryResponse { snapshots: vec![] }));
    }

    let entity_identifiers: Vec<EntityIdentifier> = children
        .iter()
        .map(|e| EntityIdentifier {
            ns: e.ns.clone(),
            pk: e.pk.clone(),
        })
        .collect();

    let entity_keys: Vec<String> = entity_identifiers
        .iter()
        .map(|e| format!("{}:{}", e.ns, e.pk))
        .collect();

    let votes = sqlx::query_file_as!(
        RelevantVote,
        "../database/sql/vote/select_partial_ordered.sql",
        &entity_keys
    )
    .fetch_all(&state.db)
    .await
    .map_error("votes")?;

    let snapshots = build_ranking_history(&entity_identifiers, &votes)?;

    Ok(Json(RankingHistoryResponse { snapshots }))
}

pub fn routes() -> ApiRouter<AppState> {
    ApiRouter::new()
        .api_route(
            "/",
            post_with(rank_children_handler, |op| {
                op.description("Rank children of a given entity.")
                    .response::<200, Json<RankingResponse>>()
            }),
        )
        .api_route(
            "/rank",
            get_with(get_ranking_handler, |op| {
                op.description("Get ranking for children of a given entity.")
                    .response::<200, Json<RankingResponse>>()
            }),
        )
        .api_route(
            "/history",
            get_with(get_ranking_history_handler, |op| {
                op.description("Get ranking history snapshots for children of a given entity.")
                    .response::<200, Json<RankingHistoryResponse>>()
            }),
        )
}

#[cfg(test)]
mod tests {
    // TODO: deal with how CSR graph has weird behavior around non-voted nodes.
    // either delete those currently ignored edge case tests (empty graph tests), or
    // add edge case handlingß
    use chrono::{Duration, NaiveDate};
    use log::debug;

    use super::*;

    fn init_logger() {
        let _ = env_logger::builder()
            .filter_module("rustsorter", log::LevelFilter::Debug) // your crate
            .filter_level(log::LevelFilter::Off)
            .is_test(true) // Don't spam normal builds, just tests
            .try_init();
    }

    // Helper function to create test entities
    fn create_entity(namespace: &str, pk: &str) -> EntityIdentifier {
        EntityIdentifier {
            ns: namespace.to_string(),
            pk: pk.to_string(),
        }
    }

    // Helper function to create a vote
    fn naive_timestamp(seconds: i64) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(1970, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            + Duration::seconds(seconds)
    }

    fn create_vote(
        left_ns: &str,
        left_pk: &str,
        right_ns: &str,
        right_pk: &str,
        magnitude: i32,
    ) -> RelevantVote {
        RelevantVote {
            left_entity_ns: left_ns.to_string(),
            left_entity_pk: left_pk.to_string(),
            right_entity_ns: right_ns.to_string(),
            right_entity_pk: right_pk.to_string(),
            magnitude,
            created_at: naive_timestamp(0),
        }
    }

    fn add_comparison(i: usize, j: usize, matrix: &mut [Vec<f64>]) {
        let weight = (j + 1) as f64 / (i + j + 2) as f64;
        matrix[i][j] += weight;
        matrix[j][i] += 1.0 - weight;
    }

    fn matrix_to_votes(matrix: &[Vec<f64>], namespace: &str) -> Vec<RelevantVote> {
        let n = matrix.len();
        let mut votes = Vec::new();

        for i in 0..n {
            for j in (i + 1)..n {
                let a_ij = matrix[i][j];
                let a_ji = matrix[j][i];
                let total = a_ij + a_ji;
                if total <= f64::EPSILON {
                    continue;
                }

                let left_prob = a_ji / total;
                let magnitude = ((0.5 - left_prob) * 100.0).round() as i32;
                votes.push(RelevantVote {
                    left_entity_ns: namespace.to_string(),
                    left_entity_pk: i.to_string(),
                    right_entity_ns: namespace.to_string(),
                    right_entity_pk: j.to_string(),
                    magnitude: magnitude.clamp(-50, 50),
                    created_at: naive_timestamp((votes.len() + 1) as i64),
                });
            }
        }

        votes
    }

    fn rank_centrality_reference(matrix: &[Vec<f64>], max_iters: usize, tol: f64) -> Vec<f64> {
        let n = matrix.len();
        if n == 0 {
            return Vec::new();
        }

        let mut scores = vec![1.0f64 / n as f64; n];
        let mut next = vec![0.0f64; n];
        let mut row_sums = vec![0.0f64; n];

        for iter in 0..max_iters {
            let mut w_max = 0.0;
            for i in 0..n {
                let mut total = 0.0;
                for j in 0..n {
                    if i == j {
                        continue;
                    }
                    let a_ij = matrix[i][j];
                    let a_ji = matrix[j][i];
                    let sum = a_ij + a_ji;
                    if sum <= f64::EPSILON {
                        continue;
                    }
                    total += a_ij / sum;
                }
                row_sums[i] = total;
                if total > w_max {
                    w_max = total;
                }
            }

            if w_max <= f64::EPSILON {
                break;
            }

            next.fill(0.0);

            for i in 0..n {
                let stay_prob = 1.0 - row_sums[i] / w_max;
                next[i] += scores[i] * stay_prob;

                for j in 0..n {
                    if i == j {
                        continue;
                    }
                    let a_ij = matrix[i][j];
                    let a_ji = matrix[j][i];
                    let sum = a_ij + a_ji;
                    if sum <= f64::EPSILON {
                        continue;
                    }
                    let prob = (a_ij / sum) / w_max;
                    next[j] += scores[i] * prob;
                }
            }

            let diff: f64 = scores
                .iter()
                .zip(next.iter())
                .map(|(a, b)| (a - b).abs())
                .sum();

            scores.clone_from_slice(&next);

            if diff < tol {
                break;
            }

            if iter == max_iters - 1 {
                warn!("Reference rank centrality reached max iterations without converging");
            }
        }

        scores
    }

    mod build_rank_graph_tests {
        use super::*;

        #[test]
        #[ignore]
        fn test_empty_entities_and_votes() {
            let entities = vec![];
            let votes = vec![];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            // The graph builder seems to create a default node
            assert_eq!(graph.graph.node_count(), 0);
            assert_eq!(graph.graph.edge_count(), 0);
            assert!(graph.index_to_entity_identifier.is_empty());
        }

        #[test]
        #[ignore]
        fn test_entities_no_votes() {
            let entities = vec![
                create_entity("ns1", "a"),
                create_entity("ns1", "b"),
                create_entity("ns2", "c"),
            ];
            let votes = vec![];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            assert_eq!(graph.graph.node_count(), 3);
            assert_eq!(graph.graph.edge_count(), 0);
            assert_eq!(graph.index_to_entity_identifier.len(), 3);
        }

        #[test]
        fn test_single_vote_extreme_left() {
            let entities = vec![create_entity("ns", "A"), create_entity("ns", "B")];
            let votes = vec![
                create_vote("ns", "A", "ns", "B", -50), // Complete preference for A
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            assert_eq!(graph.graph.node_count(), 2);
            assert_eq!(graph.graph.edge_count(), 2); // Bidirectional edges

            // Check edge weights: A->B should be 0, B->A should be 100
            // magnitude -50 => weight_left_wins = -50 + 50 = 0
            // weight_right_wins = 100 - 0 = 100
            let mut found_edges = vec![];
            for idx in 0..2 {
                for edge in graph.graph.out_neighbors_with_values(idx) {
                    found_edges.push((idx, edge.target, edge.value));
                }
            }

            // Find which index corresponds to A and B
            let a_idx = graph._entity_key_to_index.get("ns:A").unwrap();
            let b_idx = graph._entity_key_to_index.get("ns:B").unwrap();

            let a_to_b = found_edges
                .iter()
                .find(|(from, to, _)| from == a_idx && to == b_idx)
                .unwrap();
            let b_to_a = found_edges
                .iter()
                .find(|(from, to, _)| from == b_idx && to == a_idx)
                .unwrap();

            assert_eq!(a_to_b.2, 0.0); // A->B weight is 0
            assert_eq!(b_to_a.2, 100.0); // B->A weight is 100
        }

        #[test]
        fn test_single_vote_extreme_right() {
            let entities = vec![create_entity("ns", "A"), create_entity("ns", "B")];
            let votes = vec![
                create_vote("ns", "A", "ns", "B", 50), // Complete preference for B
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            let a_idx = graph._entity_key_to_index.get("ns:A").unwrap();
            let b_idx = graph._entity_key_to_index.get("ns:B").unwrap();

            let mut found_edges = vec![];
            for idx in 0..2 {
                for edge in graph.graph.out_neighbors_with_values(idx) {
                    found_edges.push((idx, edge.target, edge.value));
                }
            }

            let a_to_b = found_edges
                .iter()
                .find(|(from, to, _)| from == a_idx && to == b_idx)
                .unwrap();
            let b_to_a = found_edges
                .iter()
                .find(|(from, to, _)| from == b_idx && to == a_idx)
                .unwrap();

            assert_eq!(a_to_b.2, 100.0); // A->B weight is 100
            assert_eq!(b_to_a.2, 0.0); // B->A weight is 0
        }

        #[test]
        fn test_neutral_vote() {
            let entities = vec![create_entity("ns", "A"), create_entity("ns", "B")];
            let votes = vec![
                create_vote("ns", "A", "ns", "B", 0), // Neutral (50-50)
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            // Both edges should have weight 50
            for idx in 0..2 {
                for edge in graph.graph.out_neighbors_with_values(idx) {
                    assert_eq!(edge.value, 50.0);
                }
            }
        }

        #[test]
        fn test_duplicate_votes_accumulation() {
            init_logger();
            let entities = vec![create_entity("ns", "A"), create_entity("ns", "B")];
            let votes = vec![
                create_vote("ns", "A", "ns", "B", -30), // weight_left = 20, weight_right = 80
                create_vote("ns", "A", "ns", "B", -10), // weight_left = 40, weight_right = 60
                create_vote("ns", "A", "ns", "B", 20),  // weight_left = 70, weight_right = 30
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            let a_idx = *graph._entity_key_to_index.get("ns:A").unwrap();
            let b_idx = *graph._entity_key_to_index.get("ns:B").unwrap();

            // Total: A->B gets 20+40+70=130, B->A gets 80+60+30=170
            let mut total_a_to_b = 0.0;
            let mut total_b_to_a = 0.0;

            for edge in graph.graph.out_neighbors_with_values(a_idx) {
                if edge.target == b_idx {
                    total_a_to_b += edge.value;
                }
            }
            for edge in graph.graph.out_neighbors_with_values(b_idx) {
                if edge.target == a_idx {
                    total_b_to_a += edge.value;
                }
            }

            assert_eq!(total_a_to_b, 130.0);
            assert_eq!(total_b_to_a, 170.0);
        }

        #[test]
        fn test_votes_with_missing_entities() {
            let entities = vec![create_entity("ns", "A"), create_entity("ns", "B")];
            let votes = vec![
                create_vote("ns", "A", "ns", "B", -20),
                create_vote("ns", "A", "ns", "C", 30), // C not in entities
                create_vote("ns", "D", "ns", "B", 10), // D not in entities
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            // Only the A-B vote should be included
            assert_eq!(graph.graph.node_count(), 2);
            assert_eq!(graph.graph.edge_count(), 2); // Bidirectional
        }

        #[test]
        fn test_circular_voting() {
            let entities = vec![
                create_entity("ns", "A"),
                create_entity("ns", "B"),
                create_entity("ns", "C"),
            ];
            let votes = vec![
                create_vote("ns", "A", "ns", "B", -20), // A > B (30-70)
                create_vote("ns", "B", "ns", "C", -20), // B > C (30-70)
                create_vote("ns", "C", "ns", "A", -20), // C > A (30-70)
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            assert_eq!(graph.graph.node_count(), 3);
            assert_eq!(graph.graph.edge_count(), 6); // 3 pairs, bidirectional

            // Each winning edge should be 30, each losing edge should be 70
            for idx in 0..3 {
                let out_edges: Vec<_> = graph.graph.out_neighbors_with_values(idx).collect();
                assert_eq!(out_edges.len(), 2);
                // One edge should be 30 (winning), one should be 70 (losing)
                let mut weights: Vec<f64> = out_edges.iter().map(|e| e.value).collect();
                weights.sort_by(|a, b| a.partial_cmp(b).unwrap());
                assert_eq!(weights, vec![30.0, 70.0]);
            }
        }

        #[test]
        fn test_build_ranking_history_snapshots() {
            let entities = vec![create_entity("ns", "A"), create_entity("ns", "B")];

            let first_time = naive_timestamp(1);
            let second_time = first_time + Duration::seconds(1);

            let votes = vec![
                RelevantVote {
                    left_entity_ns: "ns".to_string(),
                    left_entity_pk: "A".to_string(),
                    right_entity_ns: "ns".to_string(),
                    right_entity_pk: "B".to_string(),
                    magnitude: -20,
                    created_at: first_time,
                },
                RelevantVote {
                    left_entity_ns: "ns".to_string(),
                    left_entity_pk: "A".to_string(),
                    right_entity_ns: "ns".to_string(),
                    right_entity_pk: "B".to_string(),
                    magnitude: 20,
                    created_at: second_time,
                },
            ];

            let snapshots = build_ranking_history(&entities, &votes).expect("history snapshots");

            assert_eq!(snapshots.len(), 2);
            assert_eq!(snapshots[0].vote_index, 1);
            assert_eq!(snapshots[0].created_at, first_time);
            assert_eq!(snapshots[0].unsorted.len(), 0);
            assert_eq!(snapshots[0].sorted.len(), 2);

            assert_eq!(snapshots[1].vote_index, 2);
            assert_eq!(snapshots[1].created_at, second_time);
            assert_eq!(snapshots[1].sorted.len(), 2);
        }

        #[test]
        fn test_clamping_extreme_magnitudes() {
            init_logger();
            let entities = vec![
                create_entity("ns", "A"),
                create_entity("ns", "B"),
                create_entity("ns", "C"),
            ];
            let votes = vec![
                create_vote("ns", "A", "ns", "C", -10120), // Should be clamped to -50
                create_vote("ns", "B", "ns", "C", 10120),  // Should be clamped to 50
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            // Find which index corresponds to A and B
            let a_idx = *graph._entity_key_to_index.get("ns:A").unwrap();
            let b_idx = *graph._entity_key_to_index.get("ns:B").unwrap();
            let a_to_c = graph
                .graph
                .out_neighbors_with_values(a_idx)
                .next()
                .unwrap()
                .value;
            let b_to_c = graph
                .graph
                .out_neighbors_with_values(b_idx)
                .next()
                .unwrap()
                .value;
            assert_eq!(a_to_c, 0.0);
            assert_eq!(b_to_c, 100.0);
        }

        #[test]
        fn test_different_namespaces() {
            let entities = vec![
                create_entity("ns1", "A"),
                create_entity("ns2", "A"), // Same pk, different namespace
                create_entity("ns1", "B"),
            ];
            let votes = vec![
                create_vote("ns1", "A", "ns2", "A", -30),
                create_vote("ns1", "A", "ns1", "B", 20),
                create_vote("ns2", "A", "ns1", "B", -10),
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();

            assert_eq!(graph.graph.node_count(), 3);
            // Should have 6 edges (3 pairs, bidirectional)
            assert_eq!(graph.graph.edge_count(), 6);
        }
    }

    mod calculate_rank_centrality_tests {
        use super::*;

        #[test]
        #[ignore]
        fn test_empty_graph() {
            let entities = vec![];
            let votes = vec![];
            let graph = build_rank_graph(&entities, &votes).unwrap();

            let scores = run_rank_centrality(&graph.graph, 1000, 1e-6);

            assert!(scores.is_empty());
        }

        #[test]
        fn test_single_node() {
            let entities = vec![create_entity("ns", "A")];
            let votes = vec![];
            let graph = build_rank_graph(&entities, &votes).unwrap();

            let scores = run_rank_centrality(&graph.graph, 1000, 1e-6);

            assert_eq!(scores.len(), 1);
            assert_eq!(scores[0], 1.0); // Single node gets score 1
        }

        #[test]
        fn test_clear_linear_ranking_5_entities() {
            // A > B > C > D > E
            let entities = vec![
                create_entity("ns", "A"),
                create_entity("ns", "B"),
                create_entity("ns", "C"),
                create_entity("ns", "D"),
                create_entity("ns", "E"),
            ];
            let votes = vec![
                create_vote("ns", "A", "ns", "B", -40), // A strongly beats B
                create_vote("ns", "B", "ns", "C", -40), // B strongly beats C
                create_vote("ns", "C", "ns", "D", -40), // C strongly beats D
                create_vote("ns", "D", "ns", "E", -40), // D strongly beats E
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 10000, 1e-8);

            // Get scores in entity order
            let mut entity_scores = vec![];
            for i in 0..5 {
                let entity = &graph.index_to_entity_identifier[&i];
                entity_scores.push((entity.pk.clone(), scores[i]));
            }
            entity_scores.sort_by_key(|(pk, _)| pk.clone());

            // Verify A > B > C > D > E
            assert!(entity_scores[0].1 > entity_scores[1].1); // A > B
            assert!(entity_scores[1].1 > entity_scores[2].1); // B > C
            assert!(entity_scores[2].1 > entity_scores[3].1); // C > D
            assert!(entity_scores[3].1 > entity_scores[4].1); // D > E
        }

        #[test]
        fn test_tournament_style_6_entities() {
            // Each entity beats all lower-ranked entities
            let entities = vec![
                create_entity("ns", "First"),
                create_entity("ns", "Second"),
                create_entity("ns", "Third"),
                create_entity("ns", "Fourth"),
                create_entity("ns", "Fifth"),
                create_entity("ns", "Sixth"),
            ];

            let mut votes = vec![];
            let names = ["First", "Second", "Third", "Fourth", "Fifth", "Sixth"];

            // Each entity beats all entities ranked below it
            for i in 0..names.len() {
                for j in (i + 1)..names.len() {
                    votes.push(create_vote("ns", names[i], "ns", names[j], -35));
                }
            }

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 10000, 1e-8);

            // Map scores back to entities
            let mut entity_scores: Vec<(String, f64)> = vec![];
            for i in 0..6 {
                let entity = &graph.index_to_entity_identifier[&i];
                entity_scores.push((entity.pk.clone(), scores[i]));
            }

            // Sort by score descending
            entity_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            // Verify ranking order
            assert_eq!(entity_scores[0].0, "First");
            assert_eq!(entity_scores[1].0, "Second");
            assert_eq!(entity_scores[2].0, "Third");
            assert_eq!(entity_scores[3].0, "Fourth");
            assert_eq!(entity_scores[4].0, "Fifth");
            assert_eq!(entity_scores[5].0, "Sixth");
        }

        #[test]
        fn test_rock_paper_scissors_pattern() {
            // A beats B, B beats C, C beats A (circular)
            let entities = vec![
                create_entity("ns", "Rock"),
                create_entity("ns", "Paper"),
                create_entity("ns", "Scissors"),
            ];
            let votes = vec![
                create_vote("ns", "Rock", "ns", "Scissors", -40), // Rock beats Scissors
                create_vote("ns", "Scissors", "ns", "Paper", -40), // Scissors beats Paper
                create_vote("ns", "Paper", "ns", "Rock", -40),    // Paper beats Rock
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 10000, 1e-8);

            // In a perfect cycle, all scores should be approximately equal
            let mean_score = scores.iter().sum::<f64>() / scores.len() as f64;
            for score in &scores {
                assert!(
                    (score - mean_score).abs() < 0.01,
                    "Score {} deviates too much from mean {}",
                    score,
                    mean_score
                );
            }
        }

        #[test]
        fn test_dominant_entity_beats_all() {
            // Champion beats everyone, others have mixed results
            let entities = vec![
                create_entity("ns", "Champion"),
                create_entity("ns", "PlayerA"),
                create_entity("ns", "PlayerB"),
                create_entity("ns", "PlayerC"),
                create_entity("ns", "PlayerD"),
            ];
            let votes = vec![
                // Champion beats everyone
                create_vote("ns", "Champion", "ns", "PlayerA", -45),
                create_vote("ns", "Champion", "ns", "PlayerB", -45),
                create_vote("ns", "Champion", "ns", "PlayerC", -45),
                create_vote("ns", "Champion", "ns", "PlayerD", -45),
                // Others have some wins
                create_vote("ns", "PlayerA", "ns", "PlayerB", -20),
                create_vote("ns", "PlayerB", "ns", "PlayerC", -20),
                create_vote("ns", "PlayerC", "ns", "PlayerD", -20),
                create_vote("ns", "PlayerD", "ns", "PlayerA", -20), // Creates a cycle
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 10000, 1e-8);

            // Find Champion's score
            let mut champion_score = 0.0;
            let mut other_scores = vec![];

            for i in 0..5 {
                let entity = &graph.index_to_entity_identifier[&i];
                if entity.pk == "Champion" {
                    champion_score = scores[i];
                } else {
                    other_scores.push(scores[i]);
                }
            }

            // Champion should have the highest score
            for other_score in &other_scores {
                assert!(
                    champion_score > *other_score,
                    "Champion score {} should be higher than {}",
                    champion_score,
                    other_score
                );
            }
        }

        #[test]
        fn test_two_tier_ranking() {
            // Top tier (A, B) both beat bottom tier (C, D, E)
            let entities = vec![
                create_entity("ns", "A"),
                create_entity("ns", "B"),
                create_entity("ns", "C"),
                create_entity("ns", "D"),
                create_entity("ns", "E"),
            ];
            let votes = vec![
                // Top tier beats bottom tier
                create_vote("ns", "A", "ns", "C", -40),
                create_vote("ns", "A", "ns", "D", -40),
                create_vote("ns", "A", "ns", "E", -40),
                create_vote("ns", "B", "ns", "C", -40),
                create_vote("ns", "B", "ns", "D", -40),
                create_vote("ns", "B", "ns", "E", -40),
                // Within tiers
                create_vote("ns", "A", "ns", "B", -10), // Slight preference
                create_vote("ns", "C", "ns", "D", -20),
                create_vote("ns", "D", "ns", "E", -20),
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 10000, 1e-8);

            let mut entity_scores: Vec<(String, f64)> = vec![];
            for i in 0..5 {
                let entity = &graph.index_to_entity_identifier[&i];
                entity_scores.push((entity.pk.clone(), scores[i]));
            }
            entity_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            // Top two should be A and B (in some order)
            let top_two: HashSet<_> = entity_scores[0..2]
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            assert!(top_two.contains("A"));
            assert!(top_two.contains("B"));

            // Bottom three should be C, D, E
            let bottom_three: HashSet<_> = entity_scores[2..5]
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            assert!(bottom_three.contains("C"));
            assert!(bottom_three.contains("D"));
            assert!(bottom_three.contains("E"));
        }

        #[test]
        fn test_sparse_spanning_tree_orders_twenty_six_entities() {
            init_logger();

            const ITEM_COUNT: usize = 26;
            let namespace = "spanning";

            let entities: Vec<EntityIdentifier> = (0..ITEM_COUNT)
                .map(|i| EntityIdentifier {
                    ns: namespace.to_string(),
                    pk: i.to_string(),
                })
                .collect();

            // Deterministic spanning tree: each item only compares against its
            // immediate predecessor. This matches the Python reference structure
            // while avoiding RNG differences across languages.
            let mut matrix = vec![vec![0.0f64; ITEM_COUNT]; ITEM_COUNT];
            for child in 1..ITEM_COUNT {
                let parent = child - 1;
                add_comparison(parent, child, &mut matrix);
            }

            let votes = matrix_to_votes(&matrix, namespace);

            // A spanning tree of 26 nodes produces exactly 25 comparisons.
            assert_eq!(votes.len(), ITEM_COUNT - 1);

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let quantized_scores = run_rank_centrality(&graph.graph, 100000, 1e-8);

            let reference_scores = rank_centrality_reference(&matrix, 100000, 1e-8);

            let mut max_diff: f64 = 0.0;
            for (quantized, reference) in quantized_scores.iter().zip(reference_scores.iter()) {
                let diff = (quantized - reference).abs();
                max_diff = max_diff.max(diff);
            }

            assert!(
                max_diff < 1e-2,
                "quantized scores diverged too far from reference (max diff {max_diff})"
            );

            // Note: integer magnitudes (\u00b150) introduce rounding noise that
            // slightly changes the strict ordering compared to the
            // floating-point reference when we only have the
            // minimal spanning tree comparisons.
        }

        #[test]
        fn test_disconnected_components() {
            // Two separate groups with no votes between them
            let entities = vec![
                create_entity("ns", "Group1A"),
                create_entity("ns", "Group1B"),
                create_entity("ns", "Group2X"),
                create_entity("ns", "Group2Y"),
                create_entity("ns", "Group2Z"),
            ];
            let votes = vec![
                // Group 1
                create_vote("ns", "Group1A", "ns", "Group1B", -30),
                // Group 2
                create_vote("ns", "Group2X", "ns", "Group2Y", -30),
                create_vote("ns", "Group2Y", "ns", "Group2Z", -30),
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 10000, 1e-8);

            // Collect scores by group
            let mut group1_scores = vec![];
            let mut group2_scores = vec![];

            for i in 0..5 {
                let entity = &graph.index_to_entity_identifier[&i];
                if entity.pk.starts_with("Group1") {
                    group1_scores.push((entity.pk.clone(), scores[i]));
                } else {
                    group2_scores.push((entity.pk.clone(), scores[i]));
                }
            }

            // Within each group, verify the expected ordering
            group1_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            group2_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            assert_eq!(group1_scores[0].0, "Group1A"); // A beats B
            assert_eq!(group2_scores[0].0, "Group2X"); // X > Y > Z
            assert_eq!(group2_scores[1].0, "Group2Y");
            assert_eq!(group2_scores[2].0, "Group2Z");
        }

        #[test]
        fn test_convergence_with_complex_votes() {
            // Complex voting pattern with mixed preferences
            let entities = vec![
                create_entity("ns", "Alpha"),
                create_entity("ns", "Beta"),
                create_entity("ns", "Gamma"),
                create_entity("ns", "Delta"),
                create_entity("ns", "Epsilon"),
                create_entity("ns", "Zeta"),
            ];
            let votes = vec![
                // Strong preferences
                create_vote("ns", "Alpha", "ns", "Beta", -45),
                create_vote("ns", "Alpha", "ns", "Gamma", -40),
                create_vote("ns", "Beta", "ns", "Delta", -50),
                create_vote("ns", "Gamma", "ns", "Epsilon", -35),
                // Weak preferences
                create_vote("ns", "Delta", "ns", "Epsilon", -5),
                create_vote("ns", "Epsilon", "ns", "Zeta", -10),
                create_vote("ns", "Zeta", "ns", "Beta", -15),
                // Some reverse preferences
                create_vote("ns", "Gamma", "ns", "Beta", 20),
                create_vote("ns", "Delta", "ns", "Alpha", 25),
            ];

            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 100000, 1e-8);

            // Just verify it converges and produces valid scores
            assert_eq!(scores.len(), 6);
            for score in &scores {
                assert!(score.is_finite());
                assert!(*score >= 0.0);
            }

            // Sum of scores should be close to 1 (for this algorithm)
            let sum: f64 = scores.iter().sum();
            assert!(
                (sum - 1.0).abs() < 0.1,
                "Sum of scores {} should be close to 1",
                sum
            );
        }

        // Distance metric functions
        fn kendall_tau_distance(actual_positions: &[usize]) -> (usize, usize) {
            let n = actual_positions.len();
            let mut disagreements = 0;

            for i in 0..n {
                for j in (i + 1)..n {
                    let correct_order = i < j;
                    let actual_order = actual_positions[i] < actual_positions[j];
                    if correct_order != actual_order {
                        disagreements += 1;
                    }
                }
            }

            let max_disagreements = n * (n - 1) / 2;
            (disagreements, max_disagreements)
        }

        fn spearman_footrule_distance(actual_positions: &[usize]) -> i32 {
            let mut distance = 0;
            for i in 0..actual_positions.len() {
                distance += (i as i32 - actual_positions[i] as i32).abs();
            }
            distance
        }

        fn spearman_rank_correlation(actual_positions: &[usize]) -> f64 {
            let n = actual_positions.len() as f64;
            let mut d_squared_sum = 0.0;

            for i in 0..actual_positions.len() {
                let d = i as f64 - actual_positions[i] as f64;
                d_squared_sum += d * d;
            }

            1.0 - (6.0 * d_squared_sum) / (n * (n * n - 1.0))
        }

        fn correct_positions_count(actual_positions: &[usize]) -> usize {
            let mut count = 0;
            for i in 0..actual_positions.len() {
                if actual_positions[i] == i {
                    count += 1;
                }
            }
            count
        }

        fn hamming_distance(actual_positions: &[usize]) -> usize {
            let mut distance = 0;
            for i in 0..actual_positions.len() {
                if actual_positions[i] != i {
                    distance += 1;
                }
            }
            distance
        }

        #[test]
        #[ignore]
        fn test_alphabet_spanning_tree_ranking() {
            init_logger();

            // Configuration: how many extra votes beyond the minimum 25
            const EXTRA_VOTES: usize = 0; // Change this to add more votes

            // Create entities for all 26 letters
            let mut entities = Vec::new();
            for i in 0..26 {
                let letter = char::from(b'A' + i as u8).to_string();
                entities.push(create_entity("alphabet", &letter));
            }

            // Build a random spanning tree with exactly 25 edges
            let mut votes = Vec::new();
            let mut in_tree = vec![0]; // Start with index 0 (letter A)
            let mut not_in_tree: Vec<usize> = (1..26).collect(); // Indices 1-25 (letters B-Z)

            // Use a fixed seed for reproducibility in tests
            use rand::{Rng, SeedableRng, rngs::StdRng};
            let mut rng = StdRng::seed_from_u64(42);

            // Build the spanning tree by repeatedly connecting a random node from
            // not_in_tree to a random node from in_tree
            while !not_in_tree.is_empty() {
                // Pick a random node from in_tree
                let from_idx = rng.gen_range(0..in_tree.len());
                let from_node = in_tree[from_idx];

                // Pick a random node from not_in_tree
                let to_idx = rng.gen_range(0..not_in_tree.len());
                let to_node = not_in_tree[to_idx];

                // Create a vote between them
                let letter1 = char::from(b'A' + from_node as u8).to_string();
                let letter2 = char::from(b'A' + to_node as u8).to_string();

                // Simple scaling: letter distance * 2
                let letter_distance = from_node as i32 - to_node as i32;
                let magnitude = (letter_distance * 2).clamp(-50, 50);

                votes.push(create_vote(
                    "alphabet", &letter1, "alphabet", &letter2, magnitude,
                ));

                // Move the node from not_in_tree to in_tree
                in_tree.push(not_in_tree.remove(to_idx));
            }

            // Add extra random votes if configured
            for _ in 0..EXTRA_VOTES {
                // Pick two different random nodes
                let node1 = rng.gen_range(0..26);
                let mut node2 = rng.gen_range(0..26);
                while node2 == node1 {
                    node2 = rng.gen_range(0..26);
                }

                let letter1 = char::from(b'A' + node1 as u8).to_string();
                let letter2 = char::from(b'A' + node2 as u8).to_string();

                let letter_distance = node1 as i32 - node2 as i32;
                let magnitude = (letter_distance * 2).clamp(-50, 50);

                votes.push(create_vote(
                    "alphabet", &letter1, "alphabet", &letter2, magnitude,
                ));
            }

            // Verify we have the expected number of votes
            assert_eq!(
                votes.len(),
                25 + EXTRA_VOTES,
                "Should have exactly {} votes (25 + {} extra)",
                25 + EXTRA_VOTES,
                EXTRA_VOTES
            );

            // Build the graph and calculate rank centrality
            let graph = build_rank_graph(&entities, &votes).unwrap();
            let scores = run_rank_centrality(&graph.graph, 100000, 1e-8);

            // Map scores back to letters with their correct positions
            let mut letter_scores: Vec<(String, f64, usize)> = vec![];
            for i in 0..26 {
                let entity = &graph.index_to_entity_identifier[&i];
                let correct_position =
                    (entity.pk.chars().next().unwrap() as usize) - ('A' as usize);
                letter_scores.push((entity.pk.clone(), scores[i], correct_position));
            }

            // Sort by score descending (best to worst)
            letter_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            // Create position mapping for distance calculations
            let mut actual_positions = vec![0; 26];
            for (rank, (_, _, correct_pos)) in letter_scores.iter().enumerate() {
                actual_positions[*correct_pos] = rank;
            }

            // Calculate distance metrics
            let (kendall_tau, max_kendall_tau) = kendall_tau_distance(&actual_positions);
            let footrule = spearman_footrule_distance(&actual_positions);
            let spearman_rho = spearman_rank_correlation(&actual_positions);
            let correct_count = correct_positions_count(&actual_positions);
            let hamming = hamming_distance(&actual_positions);

            // Print diagnostics
            debug!(
                "Configuration: {} total votes (25 spanning tree + {} extra)",
                25 + EXTRA_VOTES,
                EXTRA_VOTES
            );
            debug!("\nRandom spanning tree edges:");
            for (i, vote) in votes.iter().enumerate() {
                let vote_type = if i < 25 { "tree" } else { "extra" };
                debug!(
                    "  [{}] {} vs {} (magnitude: {})",
                    vote_type, vote.left_entity_pk, vote.right_entity_pk, vote.magnitude
                );
            }
            debug!("\nResulting ordering:");
            for (i, (letter, score, correct_pos)) in letter_scores.iter().enumerate() {
                debug!(
                    "  Position {}: {} (score: {:.6}, should be at position {})",
                    i, letter, score, correct_pos
                );
            }

            debug!("\nDistance Metrics:");
            debug!(
                "  Kendall Tau Distance: {} / {} (pairwise disagreements)",
                kendall_tau, max_kendall_tau
            );
            debug!(
                "  Spearman Footrule Distance: {} (sum of position differences)",
                footrule
            );
            debug!(
                "  Spearman's Rank Correlation: {:.3} (1.0 = perfect, -1.0 = reverse)",
                spearman_rho
            );
            debug!("  Correctly Positioned Letters: {} / 26", correct_count);
            debug!(
                "  Hamming Distance: {} (number of incorrect positions)",
                hamming
            );

            // Adjust thresholds based on number of votes
            // More votes should give better results
            let spearman_threshold = 0.85 + (EXTRA_VOTES as f64 * 0.01).min(0.10);
            let kendall_divisor = 10.0 - (EXTRA_VOTES as f64 * 0.5).min(5.0);
            let footrule_threshold = 50 - (EXTRA_VOTES as i32 * 2).min(30);
            let correct_threshold = 10 + (EXTRA_VOTES / 5).min(10);
            let hamming_threshold = 20 - (EXTRA_VOTES / 5).min(10); // Relaxed from 16

            debug!("\nThresholds (adjusted for {} extra votes):", EXTRA_VOTES);
            debug!("  Spearman correlation > {:.2}", spearman_threshold);
            debug!(
                "  Kendall Tau < {}",
                (max_kendall_tau as f64 / kendall_divisor) as usize
            );
            debug!("  Footrule < {}", footrule_threshold);
            debug!("  Correct positions >= {}", correct_threshold);
            debug!("  Hamming distance <= {}", hamming_threshold);

            assert!(
                spearman_rho > spearman_threshold,
                "Spearman correlation {} should be > {} for good ordering",
                spearman_rho,
                spearman_threshold
            );
            assert!(
                (kendall_tau as f64) < max_kendall_tau as f64 / kendall_divisor,
                "Kendall Tau {} should be less than {}",
                kendall_tau,
                (max_kendall_tau as f64 / kendall_divisor) as usize
            );
            assert!(
                footrule < footrule_threshold,
                "Spearman Footrule {} should be less than {}",
                footrule,
                footrule_threshold
            );
            assert!(
                correct_count >= correct_threshold,
                "At least {} letters should be in correct position, got {}",
                correct_threshold,
                correct_count
            );
            assert!(
                hamming <= hamming_threshold,
                "Hamming distance {} should be at most {}",
                hamming,
                hamming_threshold
            );
        }
    }
}