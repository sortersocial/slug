use axum::{
    body::Body,
    extract::Path,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    Form,
};
use axum_extra::extract::cookie::CookieJar;
use maud::{html, Markup, DOCTYPE};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

mod auth;
mod breadcrumb_path;
mod editor;
mod forum;
mod garden;
pub mod routing;
mod search;
pub mod ui_action;
use breadcrumb_path::{ExternalOntologyPath, OntologyPath};

pub use auth::{
    auth_complete_page, auth_signed_in_fragment, choose_username_error_fragment,
    choose_username_page,
};
pub use editor::{editor_check, editor_page};
pub use forum::{
    redirect_forum_index, room_page, room_thread_post_view, room_thread_view, thread_feed_html,
    thread_feed_html_for_room, thread_index, thread_post_view, thread_view, ThreadNav,
};
pub(crate) use forum::{
    thread_latest_page_region, thread_region_page_morphs, ThreadRegionPageMorphs,
};

pub use forum::user_can_view_room;
pub use forum::user_profile_page;
pub(crate) use forum::{
    fragment_new_thread_slot, login_to_post_hint_markup, room_members_section_markup,
    thread_ui_collapse_redacted_post, thread_ui_copy_thread, thread_ui_expand_post_full,
    thread_ui_expand_redacted_post, user_can_post_room,
};
pub(crate) use garden::{
    encode_pin_cookie_value, external_resolver_status_markup, garden_ui_copy_rank,
    vote_compare_post_success_js, GARDEN_PIN_COOKIE,
};
pub use garden::{
    external_garden_index, external_ontology_path, garden_index, ontology_path,
    redirect_strip_trailing_slash, room_external_garden_index, room_external_ontology_path,
    room_garden_index, room_ontology_path, room_vote_compare_page, vote_compare_page,
};
pub use routing::RouteContext;
pub use search::{search_page, search_results_fragment};
pub use ui_action::{parse_html_ui_from_form, HtmlUiAction, HtmlUiParseError, UI_RPC_FIELD};

/// Public profile URL path for a stored username (no `@`).
pub(crate) fn profile_href(username: &str) -> String {
    format!("/u/{username}")
}

/// Cookie name for UI theme (must match document.cookie migration in [`layout`]).
pub const SLUG_THEME_COOKIE: &str = "slug-theme";

/// Normalize a requested theme id to a known stylesheet key.
pub fn normalize_theme(raw: &str) -> &'static str {
    match raw {
        "retro" => "retro",
        "retro_craft" => "retro_craft",
        _ => "default",
    }
}

/// Resolved theme for rendering and cookie re-issue.
pub fn theme_from_jar(jar: &CookieJar) -> &'static str {
    jar.get(SLUG_THEME_COOKIE)
        .map(|c| normalize_theme(c.value()))
        .unwrap_or("default")
}

/// `Path` + optional `?query` for round-tripping after `POST /theme`.
pub fn theme_next_from_uri(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|pq| pq.as_str().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

/// `Set-Cookie` for a validated theme (ASCII cookie value).
pub fn theme_cookie_header_value(theme: &str) -> HeaderValue {
    let t = normalize_theme(theme);
    let s = format!("{SLUG_THEME_COOKIE}={t}; Path=/; SameSite=Lax; Max-Age=31536000");
    HeaderValue::from_str(&s).expect("theme cookie must be ASCII")
}

/// Re-issue theme cookie on responses that also set `slug_session`, so login does not drop theme.
pub fn theme_cookie_header_from_jar(jar: &CookieJar) -> Option<HeaderValue> {
    let c = jar.get(SLUG_THEME_COOKIE)?;
    Some(theme_cookie_header_value(c.value()))
}

fn sanitize_theme_next(next: Option<&str>) -> String {
    let s = next.unwrap_or("/").trim();
    if s.starts_with('/') && !s.starts_with("//") && s.len() < 8192 {
        s.to_string()
    } else {
        "/".to_string()
    }
}

#[derive(Debug, Deserialize)]
pub struct ThemeForm {
    theme: String,
    next: Option<String>,
}

/// `POST /theme` — set theme cookie and redirect back (full navigation, no fetch).
pub async fn post_theme(Form(form): Form<ThemeForm>) -> impl IntoResponse {
    let theme = normalize_theme(&form.theme);
    let next = sanitize_theme_next(form.next.as_deref());
    let loc =
        HeaderValue::try_from(next.as_str()).unwrap_or_else(|_| HeaderValue::from_static("/"));
    Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, loc)
        .header(header::SET_COOKIE, theme_cookie_header_value(theme))
        .body(Body::empty())
        .expect("theme redirect response")
}

// Embed CSS and shared UI script at compile time
const THEME_DEFAULT_CSS: &str = include_str!("../../static/theme_default.css");
const THEME_RETRO_CSS: &str = include_str!("../../static/theme_retro.css");
const THEME_RETRO_CRAFT_CSS: &str = include_str!("../../static/theme_retro_craft.css");
const SLUG_UI_JS: &str = include_str!("../../static/slug_ui.js");

pub async fn serve_static(Path(filename): Path<String>) -> impl IntoResponse {
    if filename == "slug_ui.js" {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
            .header(header::CACHE_CONTROL, "public, max-age=3600")
            .body(SLUG_UI_JS.to_string())
            .unwrap()
            .into_response();
    }

    let theme = filename
        .strip_prefix("theme_")
        .and_then(|s| s.strip_suffix(".css"));

    let css = match theme {
        Some("default") => THEME_DEFAULT_CSS,
        Some("retro") => THEME_RETRO_CSS,
        Some("retro_craft") => THEME_RETRO_CRAFT_CSS,
        _ => return (StatusCode::NOT_FOUND, "static file not found").into_response(),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(css.to_string())
        .unwrap()
        .into_response()
}

pub(crate) fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s).expect("javascript string escaping")
}

/// `console.warn` as `text/javascript` — for `POST /ui` parse errors and inline morph failures.
pub(crate) fn ui_js_warn(msg: &str) -> Response {
    let js = format!("console.warn({});", js_string_literal(msg));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
        .body(Body::from(js))
        .unwrap()
}

pub(crate) struct JsBuilder {
    snippets: Vec<String>,
}

pub(crate) struct JsQueryBuilder {
    builder: JsBuilder,
    expr: String,
}

impl JsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            snippets: Vec::new(),
        }
    }

    pub(crate) fn morph_selector(self, selector: &str, markup: Markup) -> Self {
        self.morph_expr(
            &format!("document.querySelector({})", js_string_literal(selector)),
            markup,
            None,
        )
    }

    /// Morph **children** of `selector` so the outer element (e.g. `#new-thread-ui-slot`) keeps its `id`.
    pub(crate) fn morph_inner_selector(self, selector: &str, markup: Markup) -> Self {
        self.qs(selector).morph_inner(markup)
    }

    pub(crate) fn morph_expr(
        mut self,
        expr: &str,
        markup: Markup,
        morph_style: Option<&str>,
    ) -> Self {
        let html = js_string_literal(&markup.into_string());
        let opts = morph_style
            .map(|style| format!(", {{morphStyle: {}}}", js_string_literal(style)))
            .unwrap_or_default();
        self.snippets.push(format!(
            "var __slugEl = {expr}; if (__slugEl) {{ Idiomorph.morph(__slugEl, {html}{opts}); }}",
        ));
        self
    }

    pub(crate) fn qs(self, selector: &str) -> JsQueryBuilder {
        JsQueryBuilder {
            builder: self,
            expr: format!("document.querySelector({})", js_string_literal(selector)),
        }
    }

    pub(crate) fn id(self, id: &str) -> JsQueryBuilder {
        self.qs(&format!("#{id}"))
    }

    pub(crate) fn if_current_path_matches(
        mut self,
        path: &str,
        f: impl FnOnce(JsBuilder) -> JsBuilder,
    ) -> Self {
        let inner = f(JsBuilder::new()).build();
        self.snippets.push(format!(
            "var __slugHere = window.location.pathname + window.location.search; var __slugPath = {path}; if (__slugHere === __slugPath || __slugHere.indexOf(__slugPath + '?') === 0) {{ {inner} }}",
            path = js_string_literal(path),
        ));
        self
    }

    pub(crate) fn if_current_path_not_matches(
        mut self,
        path: &str,
        f: impl FnOnce(JsBuilder) -> JsBuilder,
    ) -> Self {
        let inner = f(JsBuilder::new()).build();
        self.snippets.push(format!(
            "var __slugHere = window.location.pathname + window.location.search; var __slugPath = {path}; if (!(__slugHere === __slugPath || __slugHere.indexOf(__slugPath + '?') === 0)) {{ {inner} }}",
            path = js_string_literal(path),
        ));
        self
    }

    /// Guard on the viewer's thread-page offset (`?offset=N`, default 0, snapped
    /// down to a [`forum::THREAD_PAGE_SIZE`] boundary — mirrors the server-side
    /// snapping in the thread GET view). Used so SSE pushes only touch the pages
    /// they belong to instead of overwriting whatever page the viewer selected.
    fn if_page_offset(
        mut self,
        cmp: &str,
        offset: usize,
        f: impl FnOnce(JsBuilder) -> JsBuilder,
    ) -> Self {
        let inner = f(JsBuilder::new()).build();
        self.snippets.push(format!(
            "var __slugOffRaw = new URLSearchParams(window.location.search).get('offset'); var __slugOffN = __slugOffRaw ? (parseInt(__slugOffRaw, 10) || 0) : 0; var __slugPageOff = __slugOffN - (__slugOffN % {page}); if (__slugPageOff {cmp} {offset}) {{ {inner} }}",
            page = forum::THREAD_PAGE_SIZE,
        ));
        self
    }

    /// Viewer is on the latest page (`>=` absorbs offsets past the end, which the
    /// GET view clamps to the latest page).
    pub(crate) fn if_page_offset_at_least(
        self,
        offset: usize,
        f: impl FnOnce(JsBuilder) -> JsBuilder,
    ) -> Self {
        self.if_page_offset(">=", offset, f)
    }

    pub(crate) fn if_page_offset_equals(
        self,
        offset: usize,
        f: impl FnOnce(JsBuilder) -> JsBuilder,
    ) -> Self {
        self.if_page_offset("===", offset, f)
    }

    pub(crate) fn if_page_offset_below(
        self,
        offset: usize,
        f: impl FnOnce(JsBuilder) -> JsBuilder,
    ) -> Self {
        self.if_page_offset("<", offset, f)
    }

    pub(crate) fn redirect(mut self, to: &str) -> Self {
        self.snippets
            .push(format!("window.location = {};", js_string_literal(to)));
        self
    }

    pub(crate) fn clipboard_write_text_and_label_btn(
        mut self,
        text: &str,
        btn_id: &str,
        copied_label: &str,
    ) -> Self {
        self.snippets.push(format!(
            "navigator.clipboard.writeText({text}).then(function(){{ var __slugCopyBtn = document.getElementById({btn_id}); if (__slugCopyBtn) {{ __slugCopyBtn.textContent = {label}; }} }}).catch(function(__slugErr){{ console.warn(__slugErr); }});",
            text = js_string_literal(text),
            btn_id = js_string_literal(btn_id),
            label = js_string_literal(copied_label),
        ));
        self
    }

    /// Focus first matching element (e.g. after morphing open a compose form).
    pub(crate) fn focus_selector(mut self, selector: &str) -> Self {
        self.snippets.push(format!(
            "var __slugF = document.querySelector({}); if (__slugF && __slugF.focus) {{ __slugF.focus(); }}",
            js_string_literal(selector),
        ));
        self
    }

    pub(crate) fn build(self) -> String {
        self.snippets.join(" ")
    }

    pub(crate) fn into_response(self) -> Response {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
            .body(Body::from(self.build()))
            .unwrap()
    }
}

impl JsQueryBuilder {
    pub(crate) fn morph(mut self, markup: Markup) -> JsBuilder {
        let html = js_string_literal(&markup.into_string());
        self.builder.snippets.push(format!(
            "var __slugTarget = {expr}; if (__slugTarget) {{ Idiomorph.morph(__slugTarget, {html}); }}",
            expr = self.expr,
        ));
        self.builder
    }

    pub(crate) fn morph_inner(mut self, markup: Markup) -> JsBuilder {
        let html = js_string_literal(&markup.into_string());
        self.builder.snippets.push(format!(
            "var __slugTarget = {expr}; if (__slugTarget) {{ Idiomorph.morph(__slugTarget, {html}, {{morphStyle: 'innerHTML'}}); }}",
            expr = self.expr,
        ));
        self.builder
    }

    pub(crate) fn reset(mut self) -> JsBuilder {
        self.builder.snippets.push(format!(
            "var __slugTarget = {expr}; if (__slugTarget) {{ __slugTarget.reset(); }}",
            expr = self.expr,
        ));
        self.builder
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn layout(
    title: &str,
    view: &str,
    body: Markup,
    views: Option<u64>,
    theme: &str,
    theme_next: &str,
    garden_room_wire: Option<&str>,
    garden_path_prefix: Option<&str>,
) -> Markup {
    layout_embed_controls(
        title,
        view,
        body,
        views,
        None,
        theme,
        theme_next,
        garden_room_wire,
        garden_path_prefix,
        true,
    )
}

/// Like [`layout`], but also shows public human/AI post counts in the header meta.
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_with_post_stats(
    title: &str,
    view: &str,
    body: Markup,
    views: Option<u64>,
    post_stats: slug_types::PostStats,
    theme: &str,
    theme_next: &str,
    garden_room_wire: Option<&str>,
    garden_path_prefix: Option<&str>,
) -> Markup {
    layout_embed_controls(
        title,
        view,
        body,
        views,
        Some(post_stats),
        theme,
        theme_next,
        garden_room_wire,
        garden_path_prefix,
        true,
    )
}

/// Minimal document shell: no bottom controls, no garden HUD data attributes (`data-garden-room` /
/// `data-garden-prefix` empty). For routes that own the full viewport (e.g. vote compare).
pub(super) fn layout_full_bleed_chromeless(
    title: &str,
    view: &str,
    body: Markup,
    views: Option<u64>,
    theme: &str,
    theme_next: &str,
) -> Markup {
    layout_embed_controls(
        title, view, body, views, None, theme, theme_next, None, None, false,
    )
}

#[allow(clippy::too_many_arguments)]
fn layout_embed_controls(
    title: &str,
    view: &str,
    body: Markup,
    views: Option<u64>,
    post_stats: Option<slug_types::PostStats>,
    theme: &str,
    theme_next: &str,
    garden_room_wire: Option<&str>,
    garden_path_prefix: Option<&str>,
    show_controls: bool,
) -> Markup {
    let theme = normalize_theme(theme);
    let css_href = format!("/static/theme_{theme}.css");
    let post_stats_line = post_stats.map(|s| s.format_line());
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href=(css_href) id="theme-stylesheet";
                script src="https://unpkg.com/idiomorph@0.3.0/dist/idiomorph.min.js" {}
            }
            body class=(view)
                data-garden-room=(garden_room_wire.unwrap_or(""))
                data-garden-prefix=(garden_path_prefix.unwrap_or("")) {
                @if post_stats_line.is_some() || views.is_some() {
                    span class="view-meta muted" {
                        @if let Some(ref line) = post_stats_line {
                            (line)
                        }
                        @if post_stats_line.is_some() && views.is_some() {
                            " · "
                        }
                        @if let Some(n) = views {
                            (n) " views"
                        }
                    }
                }
                div id="errors" {}
                (body)
                @if show_controls {
                div id="controls" {
                    a href="https://github.com/sortersocial/slug" id="src-link" { "src" }
                    div id="spread-control" {
                        span { "spread" }
                        input type="range" id="spread-slider" min="0" max="1" step="0.05" value="1";
                    }
                    @if let (Some(gr), Some(ref gpx)) = (garden_room_wire, garden_path_prefix) {
                        @if !gr.is_empty() {
                            div id="slug-pin-hud" class="slug-pin-hud" data-garden-prefix=(gpx) {}
                        }
                    }
                    a id="search-btn" href="/search" { "search" }
                    form id="slug-theme-form" method="post" action="/theme" {
                        input type="hidden" name="next" value=(theme_next);
                        select id="theme-select" name="theme" onchange="this.form.submit()" aria-label="Theme" {
                            @for (val, label) in [("default", "default"), ("retro", "retro"), ("retro_craft", "craft")] {
                                @if theme == val {
                                    option value=(val) selected { (label) }
                                } @else {
                                    option value=(val) { (label) }
                                }
                            }
                        }
                    }
                }
                }
                script src="/static/slug_ui.js" {}
            }
        }
    }
}

pub(super) fn now_ms() -> i64 {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.as_millis() as i64
}

pub(super) fn ratio_pct(left: i32, right: i32) -> f64 {
    let l = (left.max(0)) as f64;
    let r = (right.max(0)) as f64;
    let denom = l + r;
    if denom <= 0.0 {
        return 50.0;
    }
    (l / denom) * 100.0
}

/// Display form of a vote ratio, reduced by GCD (e.g. `50:50` → `1:1`).
pub(super) fn format_ratio(left: i32, right: i32) -> String {
    let (l, r) = crate::dsl::reduce_ratio(left, right);
    format!("{l}:{r}")
}

/// Vote ratio markup with the side matching the current garden item bolded.
pub(super) fn format_ratio_current_markup(
    left: i32,
    right: i32,
    bold_left: bool,
    bold_right: bool,
) -> Markup {
    let (l, r) = crate::dsl::reduce_ratio(left, right);
    html! {
        span class="vote-ratio" {
            @if bold_left {
                strong class="vote-ratio-current" { (l) }
            } @else {
                (l)
            }
            ":"
            @if bold_right {
                strong class="vote-ratio-current" { (r) }
            } @else {
                (r)
            }
        }
    }
}

/// Render a single breadcrumb segment with `/` separator.
pub(super) fn bc_segment(label: &str, href: &str, is_current: bool) -> Markup {
    html! {
        span class="bc-sep" { " / " }
        @if is_current {
            a href=(href) class="bc-current" { (label) }
        } @else {
            a href=(href) { (label) }
        }
    }
}

/// Breadcrumb for external ontology `/-/https://…`
fn bc_path_external(path: &ExternalOntologyPath) -> Markup {
    html! {
        a href="/" { "slug.social" }
        (bc_segment("-", "/-", path.is_root()))
        @for (i, id) in path.breadcrumb_chain().iter().enumerate() {
            @let disp = id.display_path();
            @let tail = disp.strip_prefix("-/").unwrap_or(disp.as_str());
            @let href = format!("/-/{}", tail);
            @let is_last = i + 1 == path.breadcrumb_chain().len();
            (bc_segment(id.last_segment(), &href, is_last))
        }
    }
}

/// Breadcrumb for the garden root (`/~`, the only caller). Deeper tilde pages render
/// the containment breadcrumb (`scoped_bc_containment`); URL segments are not
/// hierarchy — nested inputs redirect to the leaf before any page renders.
fn bc_path(path: &OntologyPath) -> Markup {
    html! {
        a href=(path.slug_root_href()) { "slug.social" }
        (bc_segment("~", "/~", true))
    }
}

/// Render the thread path breadcrumb: `slug.social` on `/`, or `… / threads / #tag` / `… / post #N`.
/// Brand always points at home (`/`, the thread index). When focusing a post, `#tag` links
/// to the thread page that contains it (`?offset=` + `#post-N`).
pub(super) fn bc_threads(thread_tag: Option<&str>, focused_post: Option<usize>) -> Markup {
    let nav = ThreadNav::public();
    html! {
        @if let Some(tag) = thread_tag {
            a href="/" { "slug.social" }
            (bc_segment("threads", "/", false))
            @if let Some(idx) = focused_post {
                (bc_segment(
                    &format!("#{tag}"),
                    &nav.thread_url_for_post(tag, idx),
                    false,
                ))
                (bc_segment(&format!("post #{idx}"), &nav.post_url(tag, idx), true))
            } @else {
                (bc_segment(&format!("#{tag}"), &nav.thread_url(tag), true))
            }
        } @else {
            a href="/" class="bc-current" { "slug.social" }
        }
    }
}

/// Short display label for a stored agent id (`uuid:rig:model`, no `@`).
pub(super) fn actor_label(agent_naked: &str) -> String {
    let a = agent_naked.trim();
    let parts: Vec<&str> = a.split(':').collect();
    if parts.len() >= 3 {
        let rig = parts[1].trim();
        let model = parts[2].trim();
        // X actors: show @handle instead of uuid hash.
        if rig == "x.com" {
            return format!("@{model}");
        }
        let uuid = parts[0].trim();
        let uuid8 = uuid.chars().take(8).collect::<String>();
        if !uuid8.is_empty() && !rig.is_empty() && !model.is_empty() {
            return format!("{uuid8}:{rig}:{model}");
        }
    }
    a.to_string()
}

/// Primary HTML attribution plus optional hover (human username when showing a real AI delegate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorshipAttr {
    /// Visible label: `@principal` or `@@uuid8:rig:model`.
    pub label: String,
    /// Hover on the author link when `label` is the AI delegate (`Some("@principal")`).
    pub author_title: Option<String>,
    /// Deterministic CSS color for the author link (`hsl(...)`), seeded from uuid / username.
    pub color: String,
}

/// Stable hue from a seed string (delegate uuid or human username).
fn identity_color_css(seed: &str) -> String {
    // FNV-1a 32-bit — cheap, stable across runs/platforms.
    let mut hash: u32 = 0x811c_9dc5;
    for b in seed.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let hue = hash % 360;
    // Readable on dark theme surfaces used by default / craft.
    format!("hsl({hue}, 62%, 66%)")
}

/// Seed for author color: AI → delegate uuid; human (no delegate) → principal username.
fn authorship_color_seed<'a>(principal: &'a str, delegate: &'a Option<String>) -> &'a str {
    match delegate {
        Some(d) => d
            .split(':')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(d.as_str()),
        None => principal,
    }
}

/// Prefer the AI delegate in attribution; humans post with no delegate and show `@username`.
pub(crate) fn authorship_attr(principal: &str, delegate: &Option<String>) -> AuthorshipAttr {
    let color = identity_color_css(authorship_color_seed(principal, delegate));
    match delegate {
        Some(d) => AuthorshipAttr {
            label: format!("@@{}", actor_label(d)),
            author_title: Some(format!("@{principal}")),
            color,
        },
        None => AuthorshipAttr {
            label: format!("@{principal}"),
            author_title: None,
            color,
        },
    }
}

/// HTML attribution only: human `@name`, or agent `@@uuid8:rig:model` when a real AI delegate is present.
pub(super) fn authorship_address(principal: &str, delegate: &Option<String>) -> String {
    authorship_attr(principal, delegate).label
}

/// Escape HTML special chars for safe injection.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Max characters from an item body placed in a `title` tooltip (native hover).
const ITEM_LINK_TITLE_MAX_CHARS: usize = 500;

fn collapse_whitespace_for_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(ITEM_LINK_TITLE_MAX_CHARS + 8));
    let mut last_was_space = true;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

fn item_body_title_snippet(body: &str) -> Option<String> {
    let s = collapse_whitespace_for_title(body);
    if s.is_empty() {
        return None;
    }
    let truncated: String = s.chars().take(ITEM_LINK_TITLE_MAX_CHARS).collect();
    let ellipsis = if s.chars().count() > ITEM_LINK_TITLE_MAX_CHARS {
        "…"
    } else {
        ""
    };
    Some(format!("{truncated}{ellipsis}"))
}

fn garden_href_for_item_ref(
    raw_ref: &str,
    garden_prefix: &str,
) -> Option<(crate::path_types::ItemId, String)> {
    let key = slug_types::canonicalize_item(raw_ref);
    let id = crate::path_types::ItemId::parse(&key)?.ontology_leaf();
    let href = if let Some(tail) = id.tilde_tail() {
        if tail.is_empty() {
            garden_prefix.trim_end_matches('/').to_string()
        } else {
            format!("{}/{}", garden_prefix.trim_end_matches('/'), tail)
        }
    } else if id.as_str().starts_with("https://") || id.as_str().starts_with("http://") {
        let display = id.display_path();
        let rest = display.strip_prefix("-/").unwrap_or(display.as_str());
        let ext_prefix = format!("{}-", garden_prefix.trim_end_matches('~'));
        format!("{ext_prefix}/{rest}")
    } else {
        return None;
    };
    Some((id, href))
}

fn push_item_ref_anchor(
    out: &mut String,
    raw_ref: &str,
    garden_prefix: &str,
    item_bodies: Option<&HashMap<crate::path_types::ItemId, String>>,
) -> bool {
    let Some((id, href)) = garden_href_for_item_ref(raw_ref, garden_prefix) else {
        return false;
    };
    out.push_str(r#"<a href=""#);
    out.push_str(&escape_html(&href));
    out.push('"');
    out.push_str(r#" class="pre-link""#);
    if let Some(bodies) = item_bodies {
        if let Some(body) = bodies.get(&id) {
            if let Some(snippet) = item_body_title_snippet(body) {
                out.push_str(r#" title=""#);
                out.push_str(&escape_html(&snippet));
                out.push('"');
            }
        }
    }
    out.push('>');
    let label = if id.tilde_tail().is_some() {
        id.display_path()
    } else {
        raw_ref.to_string()
    };
    out.push_str(&escape_html(&label));
    out.push_str("</a>");
    true
}

/// Replace item refs in raw prose with clickable garden links.
///
/// When `item_bodies` is set, matching ontology items get a `title` attribute with a truncated
/// body preview for native browser tooltips (forum posts, item pages).
pub(super) fn linkify_slugs_with_prefix(
    raw: &str,
    garden_prefix: &str,
    item_bodies: Option<&HashMap<crate::path_types::ItemId, String>>,
) -> String {
    let mut out = String::with_capacity(raw.len() + 64);
    for token in crate::dsl::tokenize_prose_item_refs(raw) {
        match token {
            crate::dsl::ProseToken::Text(text) => out.push_str(&escape_html(&text)),
            crate::dsl::ProseToken::ItemRef(raw_ref) => {
                if !push_item_ref_anchor(&mut out, &raw_ref, garden_prefix, item_bodies) {
                    out.push_str(&escape_html(&raw_ref));
                }
            }
        }
    }
    out
}

#[derive(Clone)]
struct EmbedFrame {
    src: String,
    title: String,
    provider_class: &'static str,
}

fn clean_media_id(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == name {
            return Some(v.to_string());
        }
    }
    None
}

fn spotify_embed_src(url: &str) -> Option<EmbedFrame> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_lowercase();
    if !(host == "open.spotify.com" || host == "www.open.spotify.com") {
        return None;
    }
    let path = tail
        .split('#')
        .next()
        .unwrap_or(tail)
        .split('?')
        .next()
        .unwrap_or(tail);
    let mut segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.first().is_some_and(|s| s.starts_with("intl-")) {
        segs.remove(0);
    }
    if segs.len() < 2 {
        return None;
    }
    let kind = segs[0];
    let id = clean_media_id(segs[1]);
    if id.is_empty() {
        return None;
    }
    let allowed = matches!(kind, "track" | "album" | "playlist" | "episode" | "show");
    if !allowed {
        return None;
    }
    Some(EmbedFrame {
        src: format!("https://open.spotify.com/embed/{kind}/{id}"),
        title: format!("Spotify {kind}"),
        provider_class: "embed-spotify",
    })
}

fn youtube_embed_src(url: &str) -> Option<EmbedFrame> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_lowercase();

    let video_id = if host == "youtu.be" || host == "www.youtu.be" {
        clean_media_id(
            tail.split(['?', '#'])
                .next()
                .unwrap_or(tail)
                .trim_matches('/'),
        )
    } else if matches!(
        host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com"
    ) {
        let path = format!("/{}", tail.split('#').next().unwrap_or(tail));
        if path.starts_with("/watch") {
            clean_media_id(&query_param(url, "v")?)
        } else if let Some(id) = path.strip_prefix("/shorts/") {
            clean_media_id(id.split(['?', '/']).next().unwrap_or(id))
        } else if let Some(id) = path.strip_prefix("/embed/") {
            clean_media_id(id.split(['?', '/']).next().unwrap_or(id))
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    if video_id.is_empty() {
        return None;
    }
    Some(EmbedFrame {
        src: format!("https://www.youtube.com/embed/{video_id}"),
        title: "YouTube video".to_string(),
        provider_class: "embed-youtube",
    })
}

fn extract_embed_frames(raw: &str) -> Vec<EmbedFrame> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for token in raw.split_whitespace() {
        let token = token
            .trim_matches(|c: char| c == '(' || c == '[' || c == '{' || c == '"' || c == '\'')
            .trim_end_matches(|c: char| ".,;:!?)]}\"'".contains(c));
        if !(token.starts_with("https://") || token.starts_with("http://")) {
            continue;
        }
        let embed = spotify_embed_src(token).or_else(|| youtube_embed_src(token));
        if let Some(frame) = embed {
            if seen.insert(frame.src.clone()) {
                out.push(frame);
            }
        }
    }
    out
}

pub(super) fn render_linkified_with_embeds_in_scope(
    raw: &str,
    garden_prefix: &str,
    item_bodies: Option<&HashMap<crate::path_types::ItemId, String>>,
) -> Markup {
    let embeds = extract_embed_frames(raw);
    html! {
        pre { (maud::PreEscaped(linkify_slugs_with_prefix(raw, garden_prefix, item_bodies))) }
        @if !embeds.is_empty() {
            div class="rich-embeds" {
                @for e in embeds {
                    div class=(format!("rich-embed {}", e.provider_class)) {
                        iframe
                            src=(e.src)
                            title=(e.title)
                            loading="lazy"
                            allow="autoplay; clipboard-write; encrypted-media; fullscreen; picture-in-picture; web-share"
                            referrerpolicy="strict-origin-when-cross-origin"
                            allowfullscreen {}
                    }
                }
            }
        }
    }
}

/// Item page / thread body: resolver-specific rich HTML, else linkified `<pre>` + media embeds.
pub(super) fn render_item_body_in_scope(
    raw: &str,
    garden_prefix: &str,
    item_bodies: Option<&HashMap<crate::path_types::ItemId, String>>,
) -> Markup {
    if let Some(m) = crate::resolvers::try_render_resolver_item_body(raw) {
        return html! {
            div class="item-body-rich" { (m) }
        };
    }
    render_linkified_with_embeds_in_scope(raw, garden_prefix, item_bodies)
}

/// CLI strings are embedded in a single-quoted JS literal; they must never need escaping.
fn assert_cli_panel_cmd_js_single_quote_safe(s: &str) {
    assert!(
        !s.contains('\\') && !s.contains('\'') && !s.contains('\n') && !s.contains('\r'),
        "cli_panel cmd must not contain `\\`, `'`, or newlines (got {s:?})"
    );
}

/// Small CLI hint panel: one border and title; each line is hover-highlighted and copies on click.
pub(super) fn cli_panel<I: AsRef<str>>(cmds: &[I]) -> Markup {
    if cmds.is_empty() {
        return html! {};
    }
    for cmd in cmds {
        assert_cli_panel_cmd_js_single_quote_safe(cmd.as_ref());
    }
    html! {
        div class="cli-panel" {
            span class="cli-panel-label muted" { "cli" }
            div class="cli-panel-cmds" {
                @for cmd in cmds {
                    @let s = cmd.as_ref();
                    button type="button" class="cli-panel-row" title="Copy command" onclick=(format!(
                        r#"navigator.clipboard.writeText('{}');"#,
                        s
                    )) {
                        code class="cli-panel-cmd" { (s) }
                    }
                }
            }
        }
    }
}

/// Age bucket for recency coloring of thread entries.
pub(super) fn recency_class(now_ms: i64, ts_ms: i64) -> &'static str {
    let age_ms = now_ms.saturating_sub(ts_ms);
    let age_secs = age_ms / 1000;
    if age_secs < 3600 {
        "age-fresh" // < 1 hour
    } else if age_secs < 86400 {
        "age-recent" // < 1 day
    } else if age_secs < 86400 * 7 {
        "age-week" // < 1 week
    } else {
        "age-old" // >= 1 week
    }
}

/// Freshness in 0..=100 for proportional timestamp color (100 = just now, 0 = ≥1 week).
/// Linear fade over 7 days so color tracks age continuously.
pub(super) fn recency_freshness_pct(now_ms: i64, ts_ms: i64) -> u8 {
    const WEEK_SECS: i64 = 7 * 24 * 3600;
    let age_secs = now_ms.saturating_sub(ts_ms).saturating_div(1000).max(0);
    if age_secs >= WEEK_SECS {
        return 0;
    }
    let remaining = WEEK_SECS - age_secs;
    ((remaining * 100) / WEEK_SECS) as u8
}

/// Inline `color-mix` style for timestamps: fresher → `--age-fresh`, older → `--age-old`.
pub(super) fn recency_color_style(now_ms: i64, ts_ms: i64) -> String {
    let pct = recency_freshness_pct(now_ms, ts_ms);
    format!("color:color-mix(in srgb,var(--age-fresh) {pct}%,var(--age-old))")
}

#[cfg(test)]
mod authorship_tests {
    use super::*;

    #[test]
    fn recency_freshness_is_proportional() {
        let now = 7 * 24 * 3600 * 1000;
        assert_eq!(recency_freshness_pct(now, now), 100);
        assert_eq!(recency_freshness_pct(now, now - 1000), 99); // ~1s old
                                                                // Mid-week ≈ 50%
        assert_eq!(
            recency_freshness_pct(now, now - 3 * 24 * 3600 * 1000 - 12 * 3600 * 1000),
            50
        );
        assert_eq!(recency_freshness_pct(now, now - 7 * 24 * 3600 * 1000), 0);
        assert_eq!(recency_freshness_pct(now, now - 14 * 24 * 3600 * 1000), 0);
        let style = recency_color_style(now, now);
        assert!(style.contains("100%"), "{style}");
        assert!(style.contains("--age-fresh"), "{style}");
    }

    #[test]
    fn human_or_missing_delegate_shows_username() {
        let a = authorship_attr("alice", &None);
        assert_eq!(a.label, "@alice");
        assert_eq!(a.author_title, None);
        assert!(a.color.starts_with("hsl("));
    }

    #[test]
    fn real_ai_delegate_shows_short_agent_with_username_hover() {
        let d = Some(
            "7a3b9c2d-1234-5678-90ab-cdef12345678:claudecode:anthropic/claude-sonnet-4.5"
                .to_string(),
        );
        let a = authorship_attr("alice", &d);
        assert_eq!(a.label, "@@7a3b9c2d:claudecode:anthropic/claude-sonnet-4.5");
        assert_eq!(a.author_title.as_deref(), Some("@alice"));
        assert_eq!(
            authorship_address("alice", &d),
            "@@7a3b9c2d:claudecode:anthropic/claude-sonnet-4.5"
        );
    }

    #[test]
    fn author_color_is_stable_and_seeded_by_uuid_or_username() {
        let d1 = Some(
            "7a3b9c2d-1234-5678-90ab-cdef12345678:claudecode:anthropic/claude-sonnet-4.5"
                .to_string(),
        );
        let d2 =
            Some("7a3b9c2d-1234-5678-90ab-cdef12345678:cursor:anthropic/claude-opus-4".to_string());
        let other =
            Some("aec1e31c-36db-4a58-a53e-43525337f6b4:cursor:anthropic/claude-opus-4".to_string());
        let a1 = authorship_attr("alice", &d1);
        let a2 = authorship_attr("alice", &d2);
        let a3 = authorship_attr("bob", &other);
        let human = authorship_attr("alice", &None);
        // Same uuid → same color even if rig/model differs.
        assert_eq!(a1.color, a2.color);
        assert_ne!(a1.color, a3.color);
        assert_ne!(a1.color, human.color);
        assert_eq!(human.color, authorship_attr("alice", &None).color);
        assert_ne!(human.color, authorship_attr("bob", &None).color);
    }
}

#[cfg(test)]
mod linkify_title_tests {
    use super::*;
    use crate::path_types::ItemId;
    use std::collections::HashMap;

    #[test]
    fn tilde_link_gets_title_from_item_bodies() {
        let mut bodies = HashMap::new();
        let key = ItemId::parse(&slug_types::canonicalize_item("~/foo/bar"))
            .unwrap()
            .ontology_leaf();
        bodies.insert(key, "Hello  world\nline".to_string());
        let html = linkify_slugs_with_prefix("see ~/foo/bar ok", "/r/x/~", Some(&bodies));
        assert!(html.contains("title=\"Hello world line\""));
        assert!(html.contains("href=\"/r/x/~/bar\""));
        assert!(html.contains(">~/bar<"), "leaf display text, got {html}");
    }

    #[test]
    fn raw_url_links_to_public_external_garden_page() {
        let html = linkify_slugs_with_prefix("see https://example.com/z.", "/~", None);
        assert!(html.contains(
            r#"<a href="/-/https://example.com/z" class="pre-link">https://example.com/z</a>."#
        ));
    }

    #[test]
    fn dash_ref_links_to_room_external_garden_page_with_title() {
        let mut bodies = HashMap::new();
        let key = ItemId::parse(&slug_types::canonicalize_item("-/example.com/z")).unwrap();
        bodies.insert(key, "External body\npreview".to_string());
        let html =
            linkify_slugs_with_prefix("see -/example.com/z", "/r/9ab12cdroom/~", Some(&bodies));
        assert!(html.contains(r#"href="/r/9ab12cdroom/-/https://example.com/z""#));
        assert!(html.contains(r#"title="External body preview""#));
    }

    #[test]
    fn code_fence_urls_are_not_linkified() {
        let html = linkify_slugs_with_prefix(
            "```json\n{\"url\":\"https://example.com/z\"}\n```\nthen https://example.com/a",
            "/~",
            None,
        );
        assert!(!html.contains(r#"href="/-/https://example.com/z""#));
        assert!(html.contains(r#"href="/-/https://example.com/a""#));
    }

    #[test]
    fn no_title_when_body_missing_or_empty() {
        let html = linkify_slugs_with_prefix("x ~/a/b y", "/~", Some(&HashMap::new()));
        assert!(!html.contains(" title="));
    }
}

#[cfg(test)]
mod vote_ratio_markup_tests {
    use super::*;

    #[test]
    fn format_ratio_current_markup_bolds_matching_side() {
        let left = format_ratio_current_markup(75, 25, true, false).into_string();
        assert!(
            left.contains(r#"<strong class="vote-ratio-current">3</strong>:1"#),
            "expected left side bolded, got {left}"
        );

        let right = format_ratio_current_markup(75, 25, false, true).into_string();
        assert!(
            right.contains(r#"3:<strong class="vote-ratio-current">1</strong>"#),
            "expected right side bolded, got {right}"
        );

        let neither = format_ratio_current_markup(50, 50, false, false).into_string();
        assert!(
            neither.contains(">1:1<") || neither.contains("1:1"),
            "expected plain reduced ratio, got {neither}"
        );
        assert!(
            !neither.contains("vote-ratio-current"),
            "no side should be bold when item is not in the pair: {neither}"
        );
    }
}
