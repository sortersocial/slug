//! Offline `.sorter` compilation and JSONL diagnostics (no network, no auth).

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use slug_types::{CheckScopeRanking, RankComponent, RankRow, paths::GardenItemUrl};

use crate::{
    api::{resolve_item, validate_ingest_document},
    dsl,
    events::{Event, Ingest},
    path_types::ItemId,
    reducer::{ReducerState, ScopeId, scope_from_room_wire},
    scope_rank::build_children_rankings,
};

#[derive(Debug, Clone, Serialize)]
pub struct CompileStats {
    pub items: usize,
    pub votes: usize,
    pub prose_blocks: usize,
}

#[derive(Debug, Serialize)]
pub struct CompileResult {
    pub ok: bool,
    pub threads: Vec<String>,
    pub rankings: Vec<CheckScopeRanking>,
    pub stats: CompileStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompileError {
    pub ok: bool,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BadJsonLine {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MalformedIngest {
    pub line: usize,
    pub id: String,
    pub room_id: String,
    pub thread_tag: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub ok: bool,
    pub path: String,
    pub total_lines: usize,
    pub parsed_events: usize,
    pub bad_json_lines: Vec<BadJsonLine>,
    pub malformed_ingests: Vec<MalformedIngest>,
    pub skipped_ingests: usize,
}

fn document_stats(doc: &dsl::Document) -> CompileStats {
    let mut items = 0usize;
    let mut votes = 0usize;
    let mut prose_blocks = 0usize;
    for stmt in &doc.statements {
        match stmt {
            dsl::Stmt::Item { .. } => items += 1,
            dsl::Stmt::Vote { .. } => votes += 1,
            dsl::Stmt::Prose { .. } => prose_blocks += 1,
        }
    }
    CompileStats {
        items,
        votes,
        prose_blocks,
    }
}

fn threads_in_document(text: &str) -> Vec<String> {
    let mut out = HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') {
            continue;
        }
        let rest = trimmed.trim_start_matches('#').trim();
        if rest.is_empty() {
            continue;
        }
        let tag = rest.split_whitespace().next().unwrap_or(rest);
        let tag = tag.split(':').next().unwrap_or(tag).trim();
        if tag.is_empty() {
            continue;
        }
        out.insert(format!("#{}", crate::canonical_path::canonicalize_tag(tag)));
    }
    let mut tags: Vec<String> = out.into_iter().collect();
    tags.sort();
    tags
}

fn voted_parent_scopes(doc: &dsl::Document) -> Vec<ItemId> {
    let mut parents = HashSet::new();
    for stmt in &doc.statements {
        if let dsl::Stmt::Vote { item1, item2, .. } = stmt {
            if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                if let Some(p) = a.parent() {
                    parents.insert(p);
                }
                if let Some(p) = b.parent() {
                    parents.insert(p);
                }
            }
        }
    }
    let mut out: Vec<ItemId> = parents.into_iter().collect();
    out.sort();
    out
}

fn rankings_for_simulated(
    simulated: &ReducerState,
    scope: &ScopeId,
    room_wire: &str,
    doc: &dsl::Document,
) -> Vec<CheckScopeRanking> {
    voted_parent_scopes(doc)
        .iter()
        .map(|parent| {
            let scoped_content = simulated
                .content_for_scope(&scope)
                .unwrap_or_else(|| simulated.public());
            let scoped = build_children_rankings(scoped_content, parent);
            let components: Vec<RankComponent> = scoped
                .component_rankings
                .into_iter()
                .map(|comp| RankComponent {
                    pairs: comp.pairs,
                    ranking: comp
                        .ranked
                        .into_iter()
                        .map(|r| RankRow {
                            item: GardenItemUrl::from_stored(&r.item, room_wire),
                            score: r.score,
                            percent: None,
                        })
                        .collect(),
                })
                .collect();
            CheckScopeRanking {
                parent: GardenItemUrl::from_stored(parent, room_wire).into_inner(),
                components,
                unranked_items: scoped
                    .unranked_items
                    .into_iter()
                    .map(|it| GardenItemUrl::from_stored(&it, room_wire))
                    .collect(),
            }
        })
        .collect()
}

/// Validate and simulate one `.sorter` document against optional base reducer state.
pub fn compile_document(
    base: &ReducerState,
    room: &str,
    text: &str,
) -> Result<CompileResult, CompileError> {
    let room_key = room.trim();
    let scope = scope_from_room_wire(room_key);
    let validated = validate_ingest_document(base, text, &scope).map_err(|(_, message, hint)| {
        CompileError {
            ok: false,
            error: message,
            hint,
        }
    })?;

    let event = Event::Ingest(Ingest {
        ts: validated.ts,
        id: uuid::Uuid::new_v4().to_string(),
        raw: validated.raw_text.clone(),
        principal: "offline".to_string(),
        delegate: None,
        room_id: room_key.to_string(),
        thread_tag: "offline".to_string(),
    });

    let mut simulated = base.clone();
    simulated.apply_event(event);

    Ok(CompileResult {
        ok: true,
        threads: threads_in_document(text),
        rankings: rankings_for_simulated(&simulated, &scope, room_key, &validated.doc),
        stats: document_stats(&validated.doc),
    })
}

fn ingest_parse_error(raw: &str) -> Option<String> {
    dsl::parse_full(raw).err().map(|e| e.to_string())
}

fn load_events_from_jsonl(path: &Path) -> Result<(Vec<(usize, Event)>, Vec<BadJsonLine>), std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    let mut events = Vec::new();
    let mut bad_json_lines = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(trimmed) {
            Ok(ev) => events.push((line_no, ev)),
            Err(e) => bad_json_lines.push(BadJsonLine {
                line: line_no,
                message: e.to_string(),
            }),
        }
    }
    Ok((events, bad_json_lines))
}

/// Replay a JSONL event log into reducer state (same rules as server boot).
pub fn load_reducer_from_jsonl(path: &Path) -> Result<(ReducerState, Vec<BadJsonLine>), std::io::Error> {
    let (events, bad_json_lines) = load_events_from_jsonl(path)?;
    let mut state = ReducerState::default();
    for (_line_no, ev) in events {
        state.apply_event(ev);
    }
    Ok((state, bad_json_lines))
}

/// Scan an events.jsonl for corrupt JSON lines and ingests that fail DSL replay.
pub fn scan_jsonl(path: &Path) -> Result<ScanResult, std::io::Error> {
    let text = std::fs::read_to_string(path)?;
    let total_lines = text.lines().count();
    let (events, bad_json_lines) = load_events_from_jsonl(path)?;

    let mut malformed_ingests = Vec::new();
    let mut skipped_ingests = 0usize;
    let mut state = ReducerState::default();
    let parsed_events = events.len();

    for (line_no, ev) in events {
        if let Event::Ingest(ref ing) = ev {
            if let Some(reason) = ingest_parse_error(&ing.raw) {
                malformed_ingests.push(MalformedIngest {
                    line: line_no,
                    id: ing.id.clone(),
                    room_id: ing.room_id.clone(),
                    thread_tag: ing.thread_tag.clone(),
                    reason,
                });
            }
            let before = state.ingests_by_id.len();
            state.apply_event(ev);
            if state.ingests_by_id.len() == before {
                skipped_ingests += 1;
            }
        } else {
            state.apply_event(ev);
        }
    }

    let ok = bad_json_lines.is_empty() && malformed_ingests.is_empty() && skipped_ingests == 0;

    Ok(ScanResult {
        ok,
        path: path.display().to_string(),
        total_lines,
        parsed_events,
        bad_json_lines,
        malformed_ingests,
        skipped_ingests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TUTORIAL: &str = include_str!("../tests/fixtures/tutorial.sorter");

    #[test]
    fn compile_tutorial_fixture_emits_rankings() {
        let result = compile_document(&ReducerState::default(), "public", TUTORIAL).unwrap();
        assert!(result.ok);
        assert!(!result.threads.is_empty());
        assert!(result.stats.items >= 6);
        assert!(result.stats.votes >= 6);
        assert!(!result.rankings.is_empty());
    }

    #[test]
    fn compile_rejects_vote_on_missing_item() {
        let err = compile_document(
            &ReducerState::default(),
            "public",
            "{ reason }\n~/missing/a 2:1 ~/missing/b",
        )
        .unwrap_err();
        assert!(!err.ok);
        assert!(err.error.contains("undefined"));
    }

    #[test]
    fn scan_empty_jsonl_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "").unwrap();
        let report = scan_jsonl(&path).unwrap();
        assert!(report.ok);
        assert!(report.bad_json_lines.is_empty());
    }

    #[test]
    fn scan_reports_bad_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "{not json}\n").unwrap();
        let report = scan_jsonl(&path).unwrap();
        assert!(!report.ok);
        assert_eq!(report.bad_json_lines.len(), 1);
    }
}
