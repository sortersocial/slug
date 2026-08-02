use maud::html;

use crate::{
    form_template::template_json_compact,
    html::{forum::ThreadNav, ui_action::{HtmlUiAction, UI_RPC_FIELD}},
    path_types::ItemId,
};

pub(super) fn external_source_href(item: &str) -> String {
    let Ok(mut url) = url::Url::parse(item) else {
        return item.to_string();
    };
    let is_youtube = url
        .host_str()
        .map(|h| h.eq_ignore_ascii_case("www.youtube.com"))
        .unwrap_or(false);
    if is_youtube {
        let segments: Vec<String> = url
            .path_segments()
            .map(|s| s.map(|seg| seg.to_string()).collect())
            .unwrap_or_default();
        if segments.len() == 3 && segments[0] == "watch" && segments[1] == "v" {
            let id = segments[2].clone();
            url.set_path("/watch");
            url.set_query(None);
            url.query_pairs_mut().append_pair("v", &id);
            return url.to_string();
        }
    }
    item.to_string()
}

pub(super) fn external_frame_allowed(item: &str) -> bool {
    let Ok(url) = url::Url::parse(item) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default();
    !matches!(host, "github.com" | "www.github.com")
}

pub(super) fn github_resolver_controls(item: &str, nav: &ThreadNav, next: &str) -> Option<maud::Markup> {
    let item_id = ItemId::parse(item)?.normalized_storage();
    let url = url::Url::parse(item_id.as_str()).ok()?;
    if !url
        .host_str()
        .map(|h| h.eq_ignore_ascii_case("github.com"))
        .unwrap_or(false)
    {
        return None;
    }
    let children_rpc = template_json_compact(&HtmlUiAction::ResolveExternal {
        room_wire: nav.room_wire.clone(),
        item_storage: item_id.as_str().to_string(),
        mode: "children".to_string(),
        next: next.to_string(),
        form_action: "/ui".to_string(),
    })
    .ok()?;
    let siblings_rpc = item_id.parent().and_then(|_| {
        template_json_compact(&HtmlUiAction::ResolveExternal {
            room_wire: nav.room_wire.clone(),
            item_storage: item_id.as_str().to_string(),
            mode: "siblings".to_string(),
            next: next.to_string(),
            form_action: "/ui".to_string(),
        })
        .ok()
    });

    Some(html! {
        section id="external-resolver-panel" class="ont-tab-panel ont-external-resolver" {
            h3 { "GitHub resolver" }
            p class="muted" {
                "Import GitHub neighbors on demand. Results are saved as system ingests."
            }
            div class="resolver-actions" {
                form method="POST" action="/ui" {
                    input type="hidden" name=(UI_RPC_FIELD) value=(children_rpc);
                    button type="submit" data-testid="github-resolve-children" { "Load / refresh children from GitHub" }
                }
                @if let Some(rpc) = siblings_rpc {
                    form method="POST" action="/ui" {
                        input type="hidden" name=(UI_RPC_FIELD) value=(rpc);
                        button type="submit" data-testid="github-resolve-siblings" { "Load siblings from GitHub" }
                    }
                }
                a class="resolver-refresh-link" href=(next) { "Refresh page" }
            }
            div id="external-resolver-status" class="resolver-status muted" aria-live="polite" {}
        }
    })
}

pub(crate) fn external_resolver_status_markup(
    imported: Result<crate::resolvers::GithubResolveStats, &str>,
    next: &str,
) -> maud::Markup {
    let next = if next.trim().starts_with('/') && !next.trim().starts_with("//") {
        next.trim()
    } else {
        "/"
    };
    html! {
        @match imported {
            Ok(stats) => {
                p class="resolver-status-ok" {
                    (github_resolve_status_text(stats))
                    " "
                    a href=(next) { "Refresh page" }
                    " to render the updated ontology."
                }
            }
            Err(msg) => {
                p class="resolver-status-error" { (msg) }
            }
        }
    }
}

fn github_resolve_status_text(stats: crate::resolvers::GithubResolveStats) -> String {
    if stats.total_touched() == 0 {
        return "No GitHub children found.".to_string();
    }
    let mut parts = Vec::new();
    if stats.imported > 0 {
        parts.push(format!(
            "Imported {} GitHub item{}.",
            stats.imported,
            if stats.imported == 1 { "" } else { "s" }
        ));
    }
    if stats.deleted > 0 {
        parts.push(format!(
            "Removed {} closed/stale item{}.",
            stats.deleted,
            if stats.deleted == 1 { "" } else { "s" }
        ));
    }
    if stats.kept > 0 {
        if stats.imported == 0 && stats.deleted == 0 {
            parts.push(format!(
                "Already up to date ({} open issue{}).",
                stats.kept,
                if stats.kept == 1 { "" } else { "s" }
            ));
        } else {
            parts.push(format!("Kept {} still-open.", stats.kept));
        }
    }
    if parts.is_empty() {
        "Updated GitHub items.".to_string()
    } else {
        parts.join(" ")
    }
}

