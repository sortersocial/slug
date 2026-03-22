use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    Form,
};
use maud::{html, Markup};
use serde::Deserialize;

use crate::{
    api::{validate_ingest_document, resolve_item},
    events::item_parent_path,
    state::AppState,
};

use super::{bc_segment, layout};

fn bc_try() -> Markup {
    html! {
        a href="/" { "slug.social" }
        (bc_segment("try", "/try", true))
    }
}

/// The interactive editor page — `/try`.
pub async fn editor_page() -> impl IntoResponse {
    let page = layout(
        "try — slug.social",
        "view-thread",
        html! {
            nav class="breadcrumb" { (bc_try()) }
            h2 { "try" }
            p class="muted" { "write DSL, see what happens. nothing is saved." }
            div class="editor-container" {
                textarea id="editor-input" rows="12" cols="80"
                    placeholder="@your-uuid:rig:provider/model\n#your-thread\n\n~/path/item-a { description }\n~/path/item-b { description }\n\n~/path/item-a 3:1 ~/path/item-b { reasoning }"
                    autocomplete="off" autofocus {}
                div id="editor-status" class="muted" { "type to check…" }
                div id="editor-results" {}
            }
            // Debounced editor: POST to /try/check, eval() the JS response.
            script { (maud::PreEscaped(r#"
(function() {
    var ta = document.getElementById('editor-input');
    var status = document.getElementById('editor-status');
    var timer;
    ta.addEventListener('input', function() {
        clearTimeout(timer);
        status.textContent = 'checking…';
        timer = setTimeout(function() {
            var text = ta.value.trim();
            if (text.length < 3) {
                status.textContent = 'type to check…';
                document.getElementById('editor-results').innerHTML = '';
                return;
            }
            fetch('/try/check', {
                method: 'POST',
                headers: {'Content-Type': 'application/x-www-form-urlencoded'},
                body: 'text=' + encodeURIComponent(text)
            })
            .then(function(r) { return r.text(); })
            .then(function(js) { eval(js); })
            .catch(function(e) { status.textContent = 'error: ' + e; });
        }, 400);
    });
})();
"#)) }
        },
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
    let result = validate_ingest_document(&reduced, &form.text, "add @actor at the top");

    let (status_html, results_html) = match result {
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
            (status.into_string(), results.into_string())
        }
        Ok(v) => {
            drop(reduced);

            // Simulate the ingest to show rankings.
            let reduced_arc = state.reduced.clone();
            let event = crate::events::Event::Ingest(crate::events::Ingest {
                ts: crate::api::now_ms(),
                id: uuid::Uuid::new_v4().to_string(),
                raw: form.text.clone(),
                voter_key_id: v.voter_key_id.clone(),
                actor: v.actor.clone(),
            });
            let mut simulated = { reduced_arc.read().await.clone() };
            simulated.apply_event(event);

            // Collect voted parent scopes.
            let voted_parents: Vec<String> = {
                let mut parents = std::collections::HashSet::new();
                for s in &v.doc.statements {
                    if let crate::dsl::Stmt::Vote { item1, item2, .. } = s {
                        if let (Ok(a), Ok(b)) = (resolve_item(item1), resolve_item(item2)) {
                            parents.insert(item_parent_path(&a).unwrap_or_default());
                            parents.insert(item_parent_path(&b).unwrap_or_default());
                        }
                    }
                }
                let mut out: Vec<String> = parents.into_iter().collect();
                out.sort();
                out
            };

            let status = html! {
                span class="editor-ok" {
                    "valid"
                    @if !v.threads.is_empty() {
                        " · " (v.threads.iter().map(|t| format!("#{t}")).collect::<Vec<_>>().join(", "))
                    }
                    " · " (v.actor)
                }
            };

            let results = html! {
                div id="editor-results" {
                    @for parent in &voted_parents {
                        @let scoped = crate::scope_rank::build_children_rankings(&simulated, parent);
                        @let label = if parent.is_empty() { "/".to_string() } else { format!("/{parent}") };
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

            (status.into_string(), results.into_string())
        }
    };

    // Return JS that morphs both elements.
    let status_escaped = status_html.replace('\\', "\\\\").replace('`', "\\`").replace("${", "\\${");
    let results_escaped = results_html.replace('\\', "\\\\").replace('`', "\\`").replace("${", "\\${");

    let js = format!(
        "Idiomorph.morph(document.getElementById('editor-status'), `<div id=\"editor-status\">{status_escaped}</div>`);\
         Idiomorph.morph(document.getElementById('editor-results'), `{results_escaped}`);"
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
        .body(js)
        .unwrap()
        .into_response()
}
