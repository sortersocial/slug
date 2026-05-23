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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingest_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompileError {
    pub ok: bool,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
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
    pub parse_error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub ok: bool,
    pub path: String,
    pub total_lines: usize,
    pub parsed_events: usize,
    pub ingest_events: usize,
    pub bad_json_lines: Vec<BadJsonLine>,
    pub malformed_ingests: Vec<MalformedIngest>,
}

#[derive(Debug)]
pub enum CompileIngestError {
    NotFound(String),
    Io(std::io::Error),
    Compile(CompileError),
}

impl CompileIngestError {
    pub fn into_compile_error(self) -> CompileError {
        match self {
            Self::NotFound(id) => CompileError {
                ok: false,
                error: format!("ingest not found: {id}"),
                hint: Some("pass the ingest event id from events.jsonl".into()),
                parse_error: None,
            },
            Self::Io(e) => CompileError {
                ok: false,
                error: format!("io error: {e}"),
                hint: None,
                parse_error: None,
            },
            Self::Compile(e) => e,
        }
    }
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
                .content_for_scope(scope)
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

fn compile_document_inner(
    base: &ReducerState,
    room: &str,
    text: &str,
    ingest_id: Option<String>,
    ingest_line: Option<usize>,
) -> Result<CompileResult, CompileError> {
    let room_key = room.trim();
    let scope = scope_from_room_wire(room_key);
    let validated = validate_ingest_document(base, text, &scope).map_err(|(_, message, hint)| {
        let parse_error = dsl::parse_full(text).err().map(|e| e.to_string());
        CompileError {
            ok: false,
            error: message,
            hint,
            parse_error,
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
        ingest_id,
        ingest_line,
    })
}

/// Validate and simulate one `.sorter` document against optional base reducer state.
pub fn compile_document(
    base: &ReducerState,
    room: &str,
    text: &str,
) -> Result<CompileResult, CompileError> {
    compile_document_inner(base, room, text, None, None)
}

fn ingest_parse_error(raw: &str) -> Option<String> {
    dsl::parse_full(raw).err().map(|e| e.to_string())
}

type JsonlEventsLoad = Result<(usize, Vec<(usize, Event)>, Vec<BadJsonLine>), std::io::Error>;

fn load_events_from_jsonl(path: &Path) -> JsonlEventsLoad {
    let text = std::fs::read_to_string(path)?;
    let total_lines = text.lines().count();
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
    Ok((total_lines, events, bad_json_lines))
}

fn replay_events(events: &[(usize, Event)]) -> ReducerState {
    let mut state = ReducerState::default();
    for (_line_no, ev) in events {
        state.apply_event(ev.clone());
    }
    state
}

/// Replay a JSONL event log into reducer state (same rules as server boot).
pub fn load_reducer_from_jsonl(path: &Path) -> Result<(ReducerState, Vec<BadJsonLine>), std::io::Error> {
    let (_total_lines, events, bad_json_lines) = load_events_from_jsonl(path)?;
    Ok((replay_events(&events), bad_json_lines))
}

/// Find one ingest in a log and compile it against all prior events as base state.
pub fn compile_ingest_from_log(path: &Path, ingest_id: &str) -> Result<CompileResult, CompileIngestError> {
    let (_total_lines, events, bad_json_lines) = load_events_from_jsonl(path).map_err(CompileIngestError::Io)?;
    if !bad_json_lines.is_empty() {
        return Err(CompileIngestError::Compile(CompileError {
            ok: false,
            error: format!("jsonl has {} corrupt line(s)", bad_json_lines.len()),
            hint: Some("fix the log or use `sorterc scan`".into()),
            parse_error: None,
        }));
    }

    let needle = ingest_id.trim();
    let mut found: Option<(usize, Ingest)> = None;
    let mut prior: Vec<(usize, Event)> = Vec::new();

    for (line_no, ev) in events {
        if let Event::Ingest(ref ing) = ev {
            if ing.id == needle {
                found = Some((line_no, ing.clone()));
                break;
            }
        }
        prior.push((line_no, ev));
    }

    let (line_no, ing) = found.ok_or_else(|| CompileIngestError::NotFound(needle.to_string()))?;
    let base = replay_events(&prior);
    compile_document_inner(&base, &ing.room_id, &ing.raw, Some(ing.id.clone()), Some(line_no))
        .map_err(CompileIngestError::Compile)
}

/// Scan an events.jsonl for corrupt JSON lines and ingests whose DSL fails to parse.
///
/// This does not replay the log (which would run rank centrality on every ingest and
/// can take minutes on real logs). It matches what the server skips on boot: parse failure.
pub fn scan_jsonl(path: &Path) -> Result<ScanResult, std::io::Error> {
    let (total_lines, events, bad_json_lines) = load_events_from_jsonl(path)?;

    let mut malformed_ingests = Vec::new();
    let mut ingest_events = 0usize;

    for (line_no, ev) in &events {
        if let Event::Ingest(ing) = ev {
            ingest_events += 1;
            if let Some(parse_error) = ingest_parse_error(&ing.raw) {
                malformed_ingests.push(MalformedIngest {
                    line: *line_no,
                    id: ing.id.clone(),
                    room_id: ing.room_id.clone(),
                    thread_tag: ing.thread_tag.clone(),
                    parse_error,
                });
            }
        }
    }

    let ok = bad_json_lines.is_empty() && malformed_ingests.is_empty();

    Ok(ScanResult {
        ok,
        path: path.display().to_string(),
        total_lines,
        parsed_events: events.len(),
        ingest_events,
        bad_json_lines,
        malformed_ingests,
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

    #[test]
    fn scan_reports_dsl_parse_error_detail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ingest = serde_json::json!({
            "type": "ingest",
            "ts": 1,
            "id": "bad-ingest-id",
            "raw": "{ no closing brace\n~/a { body }\n~/a 1:0 ~/b",
            "principal": "test",
            "room_id": "public",
            "thread_tag": "t",
        });
        std::fs::write(&path, format!("{ingest}\n")).unwrap();
        let report = scan_jsonl(&path).unwrap();
        assert!(!report.ok);
        assert_eq!(report.malformed_ingests.len(), 1);
        assert_eq!(report.malformed_ingests[0].id, "bad-ingest-id");
        assert!(report.malformed_ingests[0].parse_error.contains("parse error"));
    }
}
