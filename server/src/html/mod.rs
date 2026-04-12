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
use std::collections::HashSet;

mod auth;
mod breadcrumb_path;
mod editor;
mod forum;
mod garden;
mod search;
pub mod ui_action;
use breadcrumb_path::OntologyPath;

pub use auth::{auth_complete_page, auth_signed_in_fragment, choose_username_error_fragment, choose_username_page};
pub use editor::{editor_check, editor_page};
pub use forum::{
    home, room_page, room_thread_post_view, room_thread_view, thread_feed_html,
    thread_feed_html_for_room, thread_feed_region_markup, thread_post_view, thread_view, ThreadNav,
};

pub(crate) use forum::{
    fragment_public_new_thread_form, fragment_room_new_thread_form, login_to_post_hint_markup,
    room_members_section_markup, thread_ui_collapse_redacted_post, thread_ui_expand_post_full,
    thread_ui_expand_redacted_post, user_can_post_room, user_can_view_room,
};
pub use garden::{garden_index, ontology_path, room_garden_index, room_ontology_path};
pub use search::{search_page, search_results_fragment};
pub use forum::user_profile_page;
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
    let loc = HeaderValue::try_from(next.as_str())
        .unwrap_or_else(|_| HeaderValue::from_static("/"));
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
        Self { snippets: Vec::new() }
    }

    pub(crate) fn morph_selector(self, selector: &str, markup: Markup) -> Self {
        self.morph_expr(
            &format!("document.querySelector({})", js_string_literal(selector)),
            markup,
            None,
        )
    }

    pub(crate) fn morph_expr(mut self, expr: &str, markup: Markup, morph_style: Option<&str>) -> Self {
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

    pub(crate) fn if_current_path_matches(mut self, path: &str, f: impl FnOnce(JsBuilder) -> JsBuilder) -> Self {
        let inner = f(JsBuilder::new()).build();
        self.snippets.push(format!(
            "var __slugHere = window.location.pathname + window.location.search; var __slugPath = {path}; if (__slugHere === __slugPath || __slugHere.indexOf(__slugPath + '?') === 0) {{ {inner} }}",
            path = js_string_literal(path),
        ));
        self
    }

    pub(crate) fn if_current_path_not_matches(mut self, path: &str, f: impl FnOnce(JsBuilder) -> JsBuilder) -> Self {
        let inner = f(JsBuilder::new()).build();
        self.snippets.push(format!(
            "var __slugHere = window.location.pathname + window.location.search; var __slugPath = {path}; if (!(__slugHere === __slugPath || __slugHere.indexOf(__slugPath + '?') === 0)) {{ {inner} }}",
            path = js_string_literal(path),
        ));
        self
    }

    pub(crate) fn redirect(mut self, to: &str) -> Self {
        self.snippets
            .push(format!("window.location = {};", js_string_literal(to)));
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

pub(super) fn layout(title: &str, view: &str, body: Markup, views: Option<u64>, theme: &str, theme_next: &str) -> Markup {
    let theme = normalize_theme(theme);
    let css_href = format!("/static/theme_{theme}.css");
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href=(css_href) id="theme-stylesheet";
                script src="https://unpkg.com/idiomorph@0.3.0/dist/idiomorph.min.js" {}
                script src="/static/slug_ui.js" defer {}
            }
            body class=(view) {
                @if let Some(n) = views {
                    span class="view-meta muted" { " · " (n) " views" }
                }
                (body)
                div id="controls" {
                    a href="https://github.com/sortersocial/slug" id="src-link" { "src" }
                    div id="spread-control" {
                        span { "spread" }
                        input type="range" id="spread-slider" min="0" max="1" step="0.05" value="1";
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

/// Breadcrumb for any canonical path, e.g. "parables" or "parables/counting-the-cost".
fn bc_path(path: &OntologyPath) -> Markup {
    html! {
        a href=(path.slug_root_href()) { "slug.social" }
        (bc_segment("~", "/~", path.is_root()))
        @for (i, seg) in path.segments().iter().enumerate() {
            @let href = format!("/~/{}", path.segments()[..=i].join("/"));
            @let is_last = i == path.segments().len() - 1;
            (bc_segment(seg, &href, is_last))
        }
    }
}

/// Render the thread path breadcrumb: `slug.social / #tag`
/// Root link toggles to `/~` only at thread-root (`/`).
pub(super) fn bc_threads(thread_tag: Option<&str>) -> Markup {
    let root_href = if thread_tag.is_some() { "/" } else { "/~" };
    html! {
        @if thread_tag.is_none() {
            a href=(root_href) class="bc-current" { "slug.social" }
        } @else {
            a href=(root_href) { "slug.social" }
        }
        @if let Some(tag) = thread_tag {
            (bc_segment(&format!("#{tag}"), &format!("/t/{tag}"), true))
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

/// HTML attribution only: human `@name`, or agent `@@uuid8:rig:model` when a delegate is present.
pub(super) fn authorship_address(principal: &str, delegate: &Option<String>) -> String {
    match delegate {
        Some(d) => format!("@@{}", actor_label(d)),
        None => format!("@{}", principal),
    }
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

/// Replace ~/path slugs in raw text with clickable links.
pub(super) fn linkify_slugs_with_prefix(raw: &str, garden_prefix: &str) -> String {
    let escaped = escape_html(raw);
    let mut out = String::with_capacity(escaped.len() + 64);
    let mut i = 0;
    let s = escaped.as_str();
    while i < s.len() {
        let rest = &s[i..];
        if let Some(after_tilde) = rest.strip_prefix("~/") {
            let path_len = after_tilde
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-' || *c == '/')
                .map(|c| c.len_utf8())
                .sum::<usize>();
            if path_len > 0 {
                let path = &after_tilde[..path_len];
                out.push_str(r#"<a href=""#);
                out.push_str(&escape_html(garden_prefix.trim_end_matches('/')));
                out.push('/');
                out.push_str(path);
                out.push_str(r#"" class="pre-link">~/"#);
                out.push_str(path);
                out.push_str("</a>");
                i += 2 + path_len;
                continue;
            }
        }
        if let Some((j, c)) = rest.char_indices().next() {
            out.push(c);
            i += j + c.len_utf8();
        } else {
            break;
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
    let path = tail.split('#').next().unwrap_or(tail).split('?').next().unwrap_or(tail);
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
        clean_media_id(tail.split(['?', '#']).next().unwrap_or(tail).trim_matches('/'))
    } else if matches!(host.as_str(), "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com") {
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

pub(super) fn render_linkified_with_embeds_in_scope(raw: &str, garden_prefix: &str) -> Markup {
    let embeds = extract_embed_frames(raw);
    html! {
        pre { (maud::PreEscaped(linkify_slugs_with_prefix(raw, garden_prefix))) }
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

/// Small CLI hint panel showing how to look up this page from the terminal.
pub(super) fn cli_panel(cmd: &str) -> Markup {
    html! {
        div class="cli-panel" {
            span class="cli-panel-label muted" { "cli" }
            code class="cli-panel-cmd" { (cmd) }
            button
                class="cli-panel-copy"
                title="Copy to clipboard"
                onclick=(format!(r#"navigator.clipboard.writeText('{}'); this.textContent='✓'; setTimeout(() => this.textContent='copy', 2000);"#, cmd.replace("'", "\\'")))
            {
                "copy"
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
