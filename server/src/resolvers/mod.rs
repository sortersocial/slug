//! Domain resolvers (GitHub, are.na, …) and matching HTML renderers for imported item bodies.
//!
//! Resolver output is ingested as DSL; bodies may embed a card fence
//! (base64-encoded card JSON, e.g. `slug-github-card` / `slug-arena-card`)
//! envelope that [`crate::html::render_item_body_in_scope`] renders instead of a raw `<pre>`.

pub mod arena;
pub mod default_external;
pub mod github;

pub use arena::{resolve_arena_children, ArenaImportCard, ArenaImportKind, ArenaResolver};
pub use default_external::DefaultExternalResolver;
pub use github::{
    resolve_github_children, try_render_github_import_markup, ExternalResolver, GitHubResolver,
    GithubImportCard, GithubImportKind, ResolvedChild,
};

use tokio::sync::oneshot;

use crate::{path_types::ItemId, state::AppState, write_cmd::WriteCmd};

/// Extension point: add more `try_render_*` calls here as new resolvers ship.
pub fn try_render_resolver_item_body(raw: &str) -> Option<maud::Markup> {
    github::try_render_github_import_markup(raw)
        .or_else(|| arena::try_render_arena_import_markup(raw))
}

/// Dispatch an on-demand children resolve to the resolver matching the item's host.
pub async fn resolve_external_children(
    state: &AppState,
    room: &str,
    item: &ItemId,
) -> Result<ResolveStats, String> {
    let host = url::Url::parse(item.as_str())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .unwrap_or_default();
    match host.as_str() {
        "github.com" => resolve_github_children(state, room, item).await,
        "www.are.na" | "are.na" => resolve_arena_children(state, room, item).await,
        _ => Err("no external resolver for this item".to_string()),
    }
}

/// Human-readable source label for resolver status text, keyed by item host.
pub fn resolver_source_label(item: &ItemId) -> Option<&'static str> {
    let host = url::Url::parse(item.as_str())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))?;
    match host.as_str() {
        "github.com" => Some("GitHub"),
        "www.are.na" | "are.na" => Some("Are.na"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveStats {
    pub imported: usize,
    pub deleted: usize,
    pub kept: usize,
}

impl ResolveStats {
    pub fn total_touched(&self) -> usize {
        self.imported + self.deleted + self.kept
    }
}

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub(crate) fn resolver_cooldown_ms(env_var: &str, default_ms: i64) -> i64 {
    std::env::var(env_var)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n >= 0)
        .unwrap_or(default_ms)
}

/// Append a system-principal ingest through the serialized writer.
pub(crate) async fn system_ingest(
    state: &AppState,
    room: &str,
    thread_tag: &str,
    text: String,
    principal: &str,
) -> Result<(), String> {
    let (tx, rx) = oneshot::channel();
    state
        .write_tx
        .send(WriteCmd::SystemIngest {
            room: room.to_string(),
            thread_tag: thread_tag.to_string(),
            text,
            principal: principal.to_string(),
            reply: tx,
        })
        .await
        .map_err(|_| "writer unavailable".to_string())?;
    rx.await
        .map_err(|_| "writer dropped".to_string())?
        .map_err(|(msg, hint)| hint.map_or(msg.clone(), |h| format!("{msg}: {h}")))?;
    Ok(())
}

/// Redact a post as a system principal through the serialized writer.
pub(crate) async fn system_redact(
    state: &AppState,
    post_id: &str,
    principal: &str,
) -> Result<(), String> {
    let (tx, rx) = oneshot::channel();
    state
        .write_tx
        .send(WriteCmd::SystemRedact {
            post_id: post_id.to_string(),
            principal: principal.to_string(),
            reply: tx,
        })
        .await
        .map_err(|_| "writer unavailable".to_string())?;
    rx.await
        .map_err(|_| "writer dropped".to_string())?
        .map_err(|(msg, hint)| hint.map_or(msg.clone(), |h| format!("{msg}: {h}")))?;
    Ok(())
}

pub(crate) fn normalized_child_url(url: &str) -> Option<String> {
    ItemId::parse(url).map(|id| id.normalized_storage().as_str().to_string())
}

/// URLs of direct children of `parent` declared as item statements in an ingest body.
pub(crate) fn child_urls_declared_in_ingest(raw: &str, parent: &ItemId) -> Vec<String> {
    let Ok(doc) = crate::dsl::parse_full(raw) else {
        return Vec::new();
    };
    let parent = parent.clone().normalized_storage();
    let mut out = Vec::new();
    for stmt in doc.statements {
        let crate::dsl::Stmt::Item { title, .. } = stmt else {
            continue;
        };
        let Some(item) = ItemId::parse(&title).map(|i| i.normalized_storage()) else {
            continue;
        };
        let Some(p) = item.parent() else {
            continue;
        };
        if p.normalized_storage() == parent {
            out.push(item.as_str().to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Extract the payload of a leading ```lang fence from an item body.
pub(crate) fn extract_fence<'a>(body: &'a str, lang: &str) -> Option<&'a str> {
    let b = body.trim();
    let prefix = format!("```{lang}");
    let rest = b.strip_prefix(prefix.as_str())?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix('\r'))
        .unwrap_or(rest);
    let end = rest.find("\n```")?;
    Some(rest[..end].trim())
}
