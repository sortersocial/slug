//! Background import of external URL children via domain resolvers.

use crate::api::now_ms;
use crate::canonical_path::canonicalize_tag;
use crate::domain_resolver::ResolverRegistry;
use crate::path_types::ItemId;
use crate::state::AppState;
use crate::write_cmd::WriteCmd;

pub const RESOLVER_PRINCIPAL: &str = "system:resolver";
/// On-demand refresh interval (milliseconds).
pub const RESOLVER_STALE_MS: i64 = 5 * 60 * 1000;

fn sync_cache_key(room_wire: &str, item_storage: &str) -> String {
    format!("{room_wire}|{item_storage}")
}

fn import_thread_tag(display_path: &str) -> String {
    canonicalize_tag(&format!("import:{display_path}"))
}

fn resolver_dsl_block(display_dash_path: &str, body: &str) -> String {
    let mut safe: String = body
        .chars()
        .filter(|c| *c != '{' && *c != '}')
        .collect();
    safe = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    if safe.len() > 1800 {
        safe.truncate(1800);
        safe.push('…');
    }
    if safe.trim().is_empty() {
        safe = "(auto-import)".into();
    }
    format!("{}\n{{ {safe} }}\n", display_dash_path.trim())
}

fn web_host(item: &ItemId) -> Option<String> {
    let s = item.as_str();
    let rest = s.strip_prefix("https://").or_else(|| s.strip_prefix("http://"))?;
    let host = rest.split('/').next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// If the URL host supports resolver listing and the scope is stale or has no children, fetch
/// resolver output and append a synthetic ingest in a background task.
pub async fn maybe_spawn_resolver_import(
    state: AppState,
    room_wire: String,
    parent_item: ItemId,
    display_path: String,
    child_count: usize,
) {
    let Some(host) = web_host(&parent_item) else {
        return;
    };
    if !state.resolver_registry.has_automatic_listing(&host) {
        return;
    }
    let storage = parent_item.to_storage_string();
    if storage == "https://github.com" {
        return;
    }
    let key = sync_cache_key(&room_wire, &storage);
    let now = now_ms();
    let stale = {
        let map = state.resolver_last_sync_ms.read().await;
        map.get(&key).map(|t| now - *t > RESOLVER_STALE_MS).unwrap_or(true)
    };
    if child_count > 0 && !stale {
        return;
    }
    {
        let mut g = state.resolver_inflight.lock().await;
        if !g.insert(key.clone()) {
            return;
        }
    }
    let state_clone = state.clone();
    let key_clone = key.clone();
    tokio::spawn(async move {
        let res = run_resolver_import(
            &state_clone,
            &room_wire,
            &parent_item,
            &display_path,
            state_clone.resolver_registry.as_ref(),
        )
        .await;
        state_clone.resolver_inflight.lock().await.remove(&key_clone);
        if res.is_ok() {
            let mut m = state_clone.resolver_last_sync_ms.write().await;
            m.insert(key_clone, now_ms());
        }
    });
}

async fn run_resolver_import(
    state: &AppState,
    room_wire: &str,
    parent: &ItemId,
    display_path: &str,
    registry: &ResolverRegistry,
) -> Result<(), String> {
    let Some(host) = web_host(parent) else {
        return Ok(());
    };
    let r = registry.for_host(&host);
    let mut raw_parts: Vec<String> = Vec::new();
    match r.list_children(parent).await {
        Ok(children) if !children.is_empty() => {
            for c in children {
                let stored =
                    ItemId::parse(&c.url).map(|i| i.to_storage_string()).unwrap_or(c.url.clone());
                let Some(id) = ItemId::parse(&stored) else {
                    continue;
                };
                let dash = id.display_path().to_string();
                let body = c
                    .body
                    .as_deref()
                    .or(Some(c.title.as_str()))
                    .unwrap_or("(auto-import)");
                raw_parts.push(resolver_dsl_block(&dash, body));
            }
        }
        _ => {
            if let Ok(body) = r.fetch_body(parent).await {
                raw_parts.push(resolver_dsl_block(display_path, &body));
            }
        }
    }
    if raw_parts.is_empty() {
        return Ok(());
    }
    let raw = raw_parts.join("\n");
    let thread_tag = import_thread_tag(display_path);
    let (tx, rx) = tokio::sync::oneshot::channel();
    state
        .write_tx
        .send(WriteCmd::ResolverIngest {
            room: room_wire.to_string(),
            thread_tag,
            raw,
            reply: tx,
        })
        .await
        .map_err(|_| "write channel closed".to_string())?;
    rx.await
        .map_err(|_| "resolver ingest reply dropped".to_string())?
}
