use axum::{
    extract::State,
    http::Uri,
    response::{Html, IntoResponse},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup};
use serde::Deserialize;

use crate::{
    api::{resolve_item, validate_ingest_document},
    html::JsBuilder,
    middleware::canonical_view_url,
    reducer::ScopeId,
    state::AppState,
};

use super::{bc_segment, layout, theme_from_jar, theme_next_from_uri};

fn bc_try() -> Markup {
    html! {
        a href="/" { "slug.social" }
        (bc_segment("try", "/try", true))
    }
}

/// The interactive editor page — `/try`.
pub async fn editor_page(State(state): State<AppState>, jar: CookieJar, uri: Uri) -> impl IntoResponse {
    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout(
        "try — slug.social",
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_try()) }
            h2 { "try" }
            p class="muted" { "write DSL, see what happens. nothing is saved." }
            div class="editor-container" {
                textarea id="editor-input" rows="12" cols="80"
                    placeholder="your-uuid:rig:provider/model\n#your-thread\n\n~/path/item-a { description }\n~/path/item-b { description }\n\n~/path/item-a 3:1 ~/path/item-b { reasoning }"
                    autocomplete="off" autofocus {}
                div id="editor-status" class="muted" { "type to check…" }
                div id="editor-results" {}
            }
        },
        Some(view_count),
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        None,
        None,
    );
    Html(page.into_string())
}

#[derive(Debug, Deserialize)]
pub struct EditorCheckForm {
    pub text: String,
}

/// POST /try/check — returns a JS snippet that morphs #editor-results and #editor-status.
pub async fn editor_check(
    State(state): State<AppState>,
    Form(form): Form<EditorCheckForm>,
) -> impl IntoResponse {
    let reduced = state.reduced.read().await;
    let result = validate_ingest_document(&reduced, &form.text, &ScopeId::Public);

    let (status_markup, results_markup) = match result {
        Err((_code, msg, hint)) => {
            let status = html! {
                span class="editor-error" {
                    (msg)
                    @if let Some(ref h) = hint {
                        " — " (h)
                    }
                }
            };
            let results = html! {
                div id="editor-results" {}
            };
            (
                html! { div id="editor-status" { (status) } },
                results,
            )
        }
        Ok(v) => {
            drop(reduced);

            // Simulate the ingest to show rankings.
            let reduced_arc = state.reduced.clone();
            let event = crate::events::Event::Ingest(crate::events::Ingest {
                ts: crate::api::now_ms(),
                id: uuid::Uuid::new_v4().to_string(),
                raw: form.text.clone(),
                principal: String::new(),
                delegate: None,
                room_id: "public".to_string(),
                thread_tag: String::new(),
            });
            let mut simulated = { reduced_arc.read().await.clone() };
            simulated.apply_event(event);

            // Collect voted parent scopes.
            let voted_parents: Vec<crate::path_types::ItemId> = {
                let mut parents = std::collections::HashSet::new();
                for s in &v.doc.statements {
                    if let crate::dsl::Stmt::Vote { item1, item2, .. } = s {
                        if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                            if let Some(p) = a.parent() { parents.insert(p); }
                            if let Some(p) = b.parent() { parents.insert(p); }
                        }
                    }
                }
                let mut out: Vec<crate::path_types::ItemId> = parents.into_iter().collect();
                out.sort();
                out
            };

            let status = html! {
                span class="editor-ok" {
                    "valid"
                }
            };

            let results = html! {
                div id="editor-results" {
                    @for parent in &voted_parents {
                        @let scoped = crate::scope_rank::build_children_rankings(simulated.public(), parent);
                        @let label = format!("/{}", parent.tilde_tail().unwrap_or(parent.as_str()));
                        h3 { "ranking: " (label) }
                        @for comp in &scoped.component_rankings {
                            ol class="editor-ranking" {
                                @for r in &comp.ranked {
                                    li {
                                        code { "~/" (r.item) }
                                        " "
                                        span class="muted" { (format!("{:.3}", r.score)) }
                                    }
                                }
                            }
                        }
                        @if !scoped.unranked_items.is_empty() {
                            p class="muted" {
                                "unranked: "
                                (scoped.unranked_items.iter().map(|i| format!("~/{i}")).collect::<Vec<_>>().join(", "))
                            }
                        }
                    }
                }
            };

            (
                html! { div id="editor-status" { (status) } },
                results,
            )
        }
    };

    JsBuilder::new()
        .id("editor-status")
        .morph(status_markup)
        .id("editor-results")
        .morph(results_markup)
        .into_response()
        .into_response()
}
