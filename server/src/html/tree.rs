use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    Form,
};
use axum_extra::extract::Query;
use maud::{html, Markup};
use serde::Deserialize;
use std::collections::BTreeSet;

use crate::{
    events::{canonicalize_item, path_owner_uuid},
    scope_rank::ChildrenRankings,
    state::AppState,
};

use super::{layout, render_linkified_with_embeds};

/// Query-state for the expandable tree UI.
/// Repeated keys are supported: `?open=...&open=...`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TreeQuery {
    #[serde(default)]
    pub open: Vec<String>,
    #[serde(default)]
    pub selected: Option<String>,
}

fn canon_open_set(q: &TreeQuery) -> BTreeSet<String> {
    q.open.iter().map(|s| canonicalize_item(s)).filter(|s| !s.is_empty()).collect()
}

fn canon_selected(q: &TreeQuery) -> Option<String> {
    q.selected.as_ref().map(|s| canonicalize_item(s)).filter(|s| !s.is_empty())
}

fn href_for(root: &str, open: &BTreeSet<String>, selected: Option<&str>) -> String {
    // root is already canonical; route path uses `/tree/*path` with raw segments, so we keep root canonical
    // as a query param only when root is empty. For now, root is encoded in the path.
    let mut parts: Vec<String> = Vec::new();
    for o in open {
        parts.push(format!("open={}", urlencoding::encode(o)));
    }
    if let Some(sel) = selected {
        parts.push(format!("selected={}", urlencoding::encode(sel)));
    }
    if parts.is_empty() {
        format!("/tree/{}", root.trim_start_matches("https://slug.social/~/"))
            .trim_end_matches('/')
            .to_string()
    } else {
        let base = format!("/tree/{}", root.trim_start_matches("https://slug.social/~/"))
            .trim_end_matches('/')
            .to_string();
        format!("{base}?{}", parts.join("&"))
    }
}

fn tree_root_from_path(path: Option<&str>) -> String {
    // Interpret /tree and /tree/*path as `~/...` under slug.social.
    // Empty path => "~/"
    match path {
        None => canonicalize_item("~/"),
        Some(p) if p.trim().is_empty() => canonicalize_item("~/"),
        Some(p) => canonicalize_item(&format!("~/{}", p.trim_start_matches('/'))),
    }
}

fn ranked_children_public(reduced: &crate::reducer::ReducerState, parent: &str) -> Vec<String> {
    let rankings: ChildrenRankings = crate::scope_rank::build_children_rankings(reduced, parent);
    let mut out: Vec<String> = Vec::new();
    for comp in rankings.component_rankings {
        for r in comp.ranked {
            if path_owner_uuid(&r.item).is_none() {
                out.push(r.item);
            }
        }
    }
    for it in rankings.unranked_items {
        if path_owner_uuid(&it).is_none() {
            out.push(it);
        }
    }
    out
}

fn node_id(path: &str) -> String {
    // Stable DOM id derived from canonical path.
    let mut h: u64 = 1469598103934665603; // FNV-1a 64-bit offset basis
    for b in path.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("node-{:016x}", h)
}

fn render_tree_node(
    reduced: &crate::reducer::ReducerState,
    path: &str,
    root: &str,
    open: &BTreeSet<String>,
    selected: Option<&str>,
    depth: usize,
) -> Markup {
    let id = node_id(path);
    let is_open = open.contains(path);
    let is_selected = selected == Some(path);

    let children: Vec<String> = if is_open {
        ranked_children_public(reduced, path)
    } else {
        vec![]
    };

    let label = path
        .strip_prefix("https://slug.social/~/")
        .unwrap_or(path)
        .to_string();
    let has_children = reduced
        .item_children
        .get(path)
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    let indent_px = (depth * 14) as i32;
    let row_cls = if is_selected { "tree-row selected" } else { "tree-row" };
    let twist = if has_children {
        if is_open { "▾" } else { "▸" }
    } else {
        " "
    };

    // Build a POST form that returns a JS snippet we eval().
    let mut new_open = open.clone();
    if has_children {
        if is_open { new_open.remove(path); } else { new_open.insert(path.to_string()); }
    }
    let _next_url = href_for(root, &new_open, selected.or(Some(path)));

    html! {
        li id=(id) class=(row_cls) style={(format!("padding-left: {}px", indent_px))} {
            @if has_children {
                form method="post" action="/tree/toggle" class="tree-toggle-form" {
                    input type="hidden" name="root" value=(root);
                    input type="hidden" name="target" value=(path);
                    @for o in open.iter() {
                        input type="hidden" name="open" value=(o);
                    }
                    @if let Some(sel) = selected {
                        input type="hidden" name="selected" value=(sel);
                    }
                    button type="submit" class="tree-twist" title="toggle" { (twist) }
                }
            } @else {
                span class="tree-twist" { (twist) }
            }

            a class="tree-label" href={(href_for(root, open, Some(path)))} {
                code { "~/" (label) }
            }
        }

        @if is_open && !children.is_empty() {
            @for child in children {
                (render_tree_node(reduced, &child, root, open, selected, depth + 1))
            }
        }
    }
}

fn render_tree_pane(
    reduced: &crate::reducer::ReducerState,
    root: &str,
    open: &BTreeSet<String>,
    selected: Option<&str>,
) -> Markup {
    let roots: Vec<String> = ranked_children_public(reduced, root);
    html! {
        div id="tree-pane" {
            div class="muted" { "tree: " code { "~/" (root.strip_prefix("https://slug.social/~/").unwrap_or("")) } }
            ul class="tree-list" {
                @for r in roots {
                    (render_tree_node(reduced, &r, root, open, selected, 0))
                }
            }
        }
    }
}

fn render_detail_pane(reduced: &crate::reducer::ReducerState, selected: Option<&str>) -> Markup {
    let Some(sel) = selected else {
        return html! {
            div id="detail-pane" {
                p class="muted" { "select a node" }
            }
        };
    };
    let body = reduced.item_bodies.get(sel).cloned();
    html! {
        div id="detail-pane" {
            h3 { "selected" }
            p { code { (sel) } }
            @if let Some(b) = body {
                (render_linkified_with_embeds(&b))
            } @else {
                p class="muted" { "no body yet" }
            }
        }
    }
}

fn tree_page_js() -> String {
    // We intentionally do NOT rely on the global form POST interceptor in layout()
    // (it discards the response). Here we want POST -> JS -> eval().
    r#"
(function() {
  function interceptTreeForms() {
    document.querySelectorAll('form.tree-toggle-form').forEach((f) => {
      if (f.__tree_bound) return;
      f.__tree_bound = true;
      f.addEventListener('submit', async (e) => {
        e.preventDefault();
        const btn = f.querySelector('button[type="submit"]');
        if (btn) btn.disabled = true;
        const r = await fetch(f.action, {
          method: 'POST',
          body: new URLSearchParams(new FormData(f)),
          headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
          credentials: 'same-origin',
        });
        const js = await r.text();
        try { eval(js); } finally { if (btn) btn.disabled = false; }
      });
    });
  }

  // Initial bind
  interceptTreeForms();

  // Rebind after morph
  const mo = new MutationObserver(() => interceptTreeForms());
  mo.observe(document.body, { subtree: true, childList: true });
})();
"#
    .to_string()
}

pub async fn tree_root(
    State(state): State<AppState>,
    Query(q): Query<TreeQuery>,
) -> impl IntoResponse {
    tree_render(State(state), Path("".to_string()), Query(q)).await
}

pub async fn tree_path(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(q): Query<TreeQuery>,
) -> impl IntoResponse {
    tree_render(State(state), Path(path), Query(q)).await
}

async fn tree_render(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(q): Query<TreeQuery>,
) -> axum::response::Response {
    let root = tree_root_from_path(Some(&path));
    if path.trim().is_empty() {
        // /tree maps to "~/"
    }
    if path_owner_uuid(&root).is_some() {
        return (StatusCode::FORBIDDEN, "private namespace").into_response();
    }

    let open = canon_open_set(&q);
    let selected = canon_selected(&q);

    let reduced = state.reduced.read().await;
    let tree = render_tree_pane(&reduced, &root, &open, selected.as_deref());
    let detail = render_detail_pane(&reduced, selected.as_deref());

    let page = layout(
        "tree — slug.social",
        "view-tree",
        html! {
            style { (maud::PreEscaped(r#"
              .tree-shell { display: grid; grid-template-columns: minmax(320px, 1fr) minmax(320px, 1fr); gap: 18px; padding: 14px; }
              .tree-list { list-style: none; margin: 10px 0 0 0; padding: 0; }
              .tree-row { display: flex; align-items: center; gap: 8px; min-height: 22px; position: relative; }
              .tree-row.selected { background: rgba(255,255,255,0.04); }
              .tree-twist { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
                           background: transparent; border: 0; padding: 0 4px; cursor: pointer; color: inherit; }
              .tree-label code { font-size: 12px; }
              .tree-toggle-form { display: inline; margin: 0; }
              #detail-pane pre { white-space: pre-wrap; }
            "#)) }
            h2 { "tree" }
            p class="muted" { "expand in place; refreshable via query params: " code { "?open=...&open=...&selected=..." } }
            div class="tree-shell" {
                (tree)
                (detail)
            }
            script { (maud::PreEscaped(tree_page_js())) }
        },
        None,
    );

    Html(page.into_string()).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ToggleForm {
    pub root: String,
    pub target: String,
    #[serde(default)]
    pub open: Vec<String>,
    #[serde(default)]
    pub selected: Option<String>,
}

/// POST /tree/toggle
/// Returns JS that morphs the tree pane and updates the URL.
pub async fn tree_toggle(
    State(state): State<AppState>,
    Form(f): Form<ToggleForm>,
) -> impl IntoResponse {
    let root = canonicalize_item(&f.root);
    let target = canonicalize_item(&f.target);

    if root.is_empty() || target.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing root/target").into_response();
    }
    if path_owner_uuid(&root).is_some() || path_owner_uuid(&target).is_some() {
        return (StatusCode::FORBIDDEN, "private namespace").into_response();
    }

    let mut open: BTreeSet<String> = f
        .open
        .iter()
        .map(|s| canonicalize_item(s))
        .filter(|s| !s.is_empty())
        .collect();

    if open.contains(&target) {
        open.remove(&target);
    } else {
        open.insert(target.clone());
    }

    let selected = f
        .selected
        .as_ref()
        .map(|s| canonicalize_item(s))
        .filter(|s| !s.is_empty())
        .or(Some(target.clone()));

    let reduced = state.reduced.read().await;
    let new_tree_html = render_tree_pane(&reduced, &root, &open, selected.as_deref()).into_string();
    let new_detail_html = render_detail_pane(&reduced, selected.as_deref()).into_string();
    drop(reduced);

    let next_url = href_for(&root, &open, selected.as_deref());

    // Escape for template literal
    let esc = |s: String| s.replace('\\', "\\\\").replace('`', "\\`").replace("${", "\\${");
    let tree_esc = esc(new_tree_html);
    let detail_esc = esc(new_detail_html);
    let url_esc = next_url.replace('\\', "\\\\").replace('`', "\\`").replace("'", "\\'");

    let js = format!(
        "Idiomorph.morph(document.getElementById('tree-pane'), `{tree_esc}`);\
         Idiomorph.morph(document.getElementById('detail-pane'), `{detail_esc}`);\
         history.replaceState(null, '', '{url_esc}');"
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
        .body(js)
        .unwrap()
        .into_response()
}

