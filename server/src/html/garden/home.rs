//! Public home: index of shareable `/q/` questions over garden scopes.

use axum::{
    extract::State,
    http::{HeaderMap, Uri},
    response::{Html, IntoResponse},
};
use axum_extra::extract::cookie::CookieJar;
use maud::html;

use crate::{
    api::optional_principal,
    canonical_path::canonicalize_tag,
    dsl::is_valid_aspect_slug,
    html::{
        forum::{auth_strip, ThreadNav},
        layout_with_post_stats, theme_from_jar, theme_next_from_uri,
    },
    middleware::canonical_view_url,
    path_types::ItemId,
    reducer::{ContentState, ReducerState, ScopeId},
    scope_rank::comparable_items,
    state::AppState,
};

use super::{
    access::content_for_garden_view,
    question::{
        build_question_ctx, parse_collection_leaf, question_headline, question_title,
    },
    question_body::aspects_for_scope,
    vote::{question_vote_panel, QuestionHeadline, VoteCompareDomIds},
};

const QUESTION_INDEX_CAP: usize = 50;

pub(super) struct QuestionIndexEntry {
    pub collection: ItemId,
    pub aspect: Option<String>,
    pub leaf: String,
    pub title: String,
    pub member_count: usize,
    pub question_path: String,
    pub garden_href: String,
    pub thread_href: String,
    pub headline: QuestionHeadline,
    pub dom_suffix: String,
}

fn is_listable_scope(id: &ItemId) -> bool {
    matches!(id.tilde_tail(), Some(t) if !t.is_empty() && !t.contains('/'))
}

fn question_activity(
    reduced: &ReducerState,
    content: &ContentState,
    collection: &ItemId,
    aspect: Option<&str>,
) -> i64 {
    let leaf = canonicalize_tag(&collection.last_segment());
    let thread_ts = reduced
        .forum_threads
        .get(&(ScopeId::Public, leaf))
        .map(|t| t.last_activity_ts)
        .unwrap_or(0);
    let vote_ts = if let Some(slug) = aspect {
        content
            .aspect_group(collection, slug)
            .and_then(|g| g.recent_votes.front().map(|v| v.ts))
            .unwrap_or(0)
    } else {
        let members: std::collections::HashSet<ItemId> =
            content.members_of(collection).into_iter().collect();
        content
            .ranking_group
            .recent_votes
            .iter()
            .filter(|v| members.contains(&v.a) && members.contains(&v.b))
            .map(|v| v.ts)
            .max()
            .unwrap_or(0)
    };
    thread_ts.max(vote_ts)
}

fn prompted_aspects(content: &ContentState, collection: &ItemId) -> Vec<String> {
    let mut slugs: Vec<String> = content
        .aspect_prompts
        .iter()
        .filter(|(_, p)| !p.trim().is_empty())
        .map(|(slug, _)| slug.clone())
        .filter(|slug| is_valid_aspect_slug(slug))
        .collect();
    for (slug, prompt) in aspects_for_scope(content, collection) {
        if prompt.as_deref().is_some_and(|p| !p.is_empty()) && is_valid_aspect_slug(&slug) {
            slugs.push(slug);
        }
    }
    slugs.sort();
    slugs.dedup();
    slugs
}

fn entry_from_scope(
    content: &ContentState,
    nav: &ThreadNav,
    collection: &ItemId,
    aspect: Option<&str>,
    member_count: usize,
) -> Option<QuestionIndexEntry> {
    let leaf = collection.last_segment().to_string();
    if parse_collection_leaf(&leaf).as_ref() != Some(collection) {
        return None;
    }
    if let Some(slug) = aspect {
        if !is_valid_aspect_slug(slug) {
            return None;
        }
    }
    let headline = question_headline(content, collection, aspect);
    let title = question_title(&headline);
    let ctx = build_question_ctx(nav, collection, aspect, headline.clone());
    let dom_suffix = match aspect {
        Some(a) => format!("{leaf}-{a}"),
        None => leaf.clone(),
    };
    Some(QuestionIndexEntry {
        collection: collection.clone(),
        aspect: aspect.map(str::to_string),
        leaf,
        title,
        member_count,
        question_path: ctx.question_path,
        garden_href: ctx.garden_href,
        thread_href: ctx.thread_href,
        headline,
        dom_suffix,
    })
}

/// Public scopes with ≥2 comparable members, plus prompted aspects under those scopes.
/// Bump-ordered by thread / vote activity, then member count descending.
pub(super) fn collect_question_entries(
    reduced: &ReducerState,
    content: &ContentState,
    nav: &ThreadNav,
) -> Vec<QuestionIndexEntry> {
    let mut scopes: Vec<(ItemId, usize)> = content
        .members_by_scope
        .keys()
        .filter(|id| is_listable_scope(id))
        .filter_map(|id| {
            let n = comparable_items(content, content.members_of(id)).len();
            (n >= 2).then_some((id.clone(), n))
        })
        .collect();
    scopes.sort_by(|a, b| {
        let act_a = question_activity(reduced, content, &a.0, None);
        let act_b = question_activity(reduced, content, &b.0, None);
        act_b
            .cmp(&act_a)
            .then(b.1.cmp(&a.1))
            .then(a.0.as_str().cmp(b.0.as_str()))
    });

    let mut entries = Vec::new();
    for (scope, member_count) in scopes {
        if let Some(e) = entry_from_scope(content, nav, &scope, None, member_count) {
            entries.push(e);
        }
        for slug in prompted_aspects(content, &scope) {
            if let Some(e) = entry_from_scope(content, nav, &scope, Some(&slug), member_count) {
                entries.push(e);
            }
        }
        if entries.len() >= QUESTION_INDEX_CAP {
            break;
        }
    }
    entries.truncate(QUESTION_INDEX_CAP);
    entries
}

fn question_row_markup(entry: &QuestionIndexEntry, panel: maud::Markup) -> maud::Markup {
    let row_id = format!("question-row-{}", entry.dom_suffix);
    html! {
        details class="question-row" id=(row_id) data-testid="question-row"
            data-question-leaf=(entry.leaf.as_str())
            data-question-aspect=(entry.aspect.as_deref().unwrap_or("")) {
            summary class="question-row-summary" {
                span class="question-row-title" { (entry.title) }
                @if let Some(aspect) = &entry.aspect {
                    span class="muted question-row-aspect" { " · :" (aspect) }
                }
                span class="muted question-row-count" { " · " (entry.member_count) }
            }
            p class="muted question-row-links" {
                a href=(entry.question_path) { "full question" }
                " · "
                a href=(entry.garden_href) { "garden" }
                " · "
                a href=(entry.thread_href) { "thread" }
            }
            (panel)
        }
    }
}

/// `GET /` — public question index.
pub async fn home(
    State(state): State<AppState>,
    headers: HeaderMap,
    jar: CookieJar,
    uri: Uri,
) -> impl IntoResponse {
    let nav = ThreadNav::public();
    let reduced = state.reduced.read().await;
    let user = optional_principal(&headers, &jar, &reduced);
    let logged_in = user.is_some();
    let content = content_for_garden_view(&reduced, &nav.scope());
    let entries = collect_question_entries(&reduced, content, &nav);
    let post_stats = reduced.public_post_stats();
    let strip = auth_strip(&headers, &jar, &reduced);

    let mut rows = Vec::with_capacity(entries.len());
    for entry in &entries {
        let ctx = build_question_ctx(
            &nav,
            &entry.collection,
            entry.aspect.as_deref(),
            entry.headline.clone(),
        );
        let ids = VoteCompareDomIds::with_suffix(&entry.dom_suffix);
        let panel = question_vote_panel(
            content,
            &nav,
            &entry.collection,
            entry.aspect.as_deref(),
            logged_in,
            logged_in,
            &entry.question_path,
            Some(entry.question_path.as_str()),
            Some(&ctx),
            false,
            &ids,
        );
        rows.push(question_row_markup(entry, panel));
    }
    drop(reduced);

    let url_key = canonical_view_url(&uri);
    let view_count = state.views.get_views(&url_key);

    let page = layout_with_post_stats(
        "slug.social",
        "view-thread view-question-index view-ontology",
        html! {
            (strip)
            nav class="breadcrumb" {
                a href="/" class="bc-current" { "slug.social" }
            }
            header class="home-intro" {
                h1 { "slug.social" }
                p class="home-lede" {
                    "pairwise questions over the public garden"
                }
                p class="muted home-nav-links" {
                    a href="/t" { "threads" }
                    " · "
                    a href="/~" { "garden" }
                }
            }
            @if rows.is_empty() {
                p class="muted" data-testid="question-index-empty" {
                    "no questions yet — a garden scope needs two items with bodies."
                }
            } @else {
                div class="question-index" data-testid="question-index" {
                    @for row in rows {
                        (row)
                    }
                }
            }
        },
        Some(view_count),
        post_stats,
        theme_from_jar(&jar),
        &theme_next_from_uri(&uri),
        None,
        None,
    );
    Html(page.into_string())
}
