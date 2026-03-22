use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use maud::{html, Markup, DOCTYPE};
use std::collections::HashSet;

mod breadcrumb_path;
mod forum;
mod garden;
mod search;
use breadcrumb_path::OntologyPath;

pub use forum::{index, thread_feed_html, thread_post_expand, thread_post_view, thread_view};
pub use garden::{garden_index, ontology_path};
pub use search::{search_page, search_results_fragment};

// Embed CSS files at compile time
const THEME_DEFAULT_CSS: &str = include_str!("../../static/theme_default.css");
const THEME_RETRO_CSS: &str = include_str!("../../static/theme_retro.css");

pub async fn serve_theme_css(Path(filename): Path<String>) -> impl IntoResponse {
    // Extract theme name from filename like "theme_default.css" or "theme_retro.css"
    let theme = filename
        .strip_prefix("theme_")
        .and_then(|s| s.strip_suffix(".css"));

    let css = match theme {
        Some("default") => THEME_DEFAULT_CSS,
        Some("retro") => THEME_RETRO_CSS,
        _ => return (StatusCode::NOT_FOUND, "theme not found").into_response(),
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/css; charset=utf-8")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(css.to_string())
        .unwrap()
        .into_response()
}

pub(super) fn layout(title: &str, view: &str, body: Markup, views: Option<u64>) -> Markup {
    html! {
        (DOCTYPE)
        html {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href="/static/theme_default.css" id="theme-stylesheet";
                script src="https://unpkg.com/idiomorph@0.3.0/dist/idiomorph.min.js" {}
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
                    div id="theme-switcher" { "theme" }
                }
script { (maud::PreEscaped(r#"
                    (function() {
                        // Theme switching
                        const themes = ['default', 'retro'];
                        const storedTheme = localStorage.getItem('slug-theme') || 'default';
                        const switcher = document.getElementById('theme-switcher');
                        const stylesheet = document.getElementById('theme-stylesheet');

                        function setTheme(name) {
                            stylesheet.href = `/static/theme_${name}.css`;
                            localStorage.setItem('slug-theme', name);
                            switcher.textContent = name;
                            setTimeout(() => setSpread(parseFloat(slider.value)), 50);
                        }

                        setTheme(storedTheme);

                        switcher.addEventListener('click', function() {
                            const current = themes.indexOf(localStorage.getItem('slug-theme') || 'default');
                            const next = (current + 1) % themes.length;
                            setTheme(themes[next]);
                        });

                        // Spread control
                        const slider = document.getElementById('spread-slider');
                        const storedSpread = localStorage.getItem('slug-spread');

                        function setSpread(value) {
                            document.documentElement.style.setProperty('--spread', value);
                            localStorage.setItem('slug-spread', value);
                        }

                        if (storedSpread !== null) {
                            slider.value = storedSpread;
                            setSpread(parseFloat(storedSpread));
                        }

                        slider.addEventListener('input', function() {
                            setSpread(parseFloat(this.value));
                        });

                        // Poem: intercept POST forms, send via fetch, await SSE for DOM update.
                        document.addEventListener('submit', async (e) => {
                            const f = e.target;
                            if (!f || f.tagName !== 'FORM') return;
                            if ((f.method || 'get').toLowerCase() !== 'post') return;
                            e.preventDefault();
                            const btn = f.querySelector('button[type="submit"], input[type="submit"]');
                            if (btn) { btn.disabled = true; btn.textContent = '…'; }
                            await fetch(f.action, {
                                method: 'POST',
                                body: new URLSearchParams(new FormData(f)),
                                headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
                                credentials: 'same-origin',
                            });
                            if (btn) { btn.disabled = false; btn.textContent = 'submit'; }
                            f.reset();
                        });

                        // Search: debounced fetch + idiomorph.
                        (function() {
                            const si = document.getElementById('search-input');
                            if (!si) return;
                            let st;
                            si.addEventListener('input', () => {
                                clearTimeout(st);
                                st = setTimeout(async () => {
                                    const q = si.value.trim();
                                    const el = document.getElementById('search-results');
                                    if (!el) return;
                                    if (q.length < 2) { el.innerHTML = ''; return; }
                                    const r = await fetch('/search/results?q=' + encodeURIComponent(q));
                                    Idiomorph.morph(el, await r.text());
                                }, 150);
                            });
                            // Focus search input on / key
                            document.addEventListener('keydown', (e) => {
                                if (e.key === '/' && document.activeElement !== si) {
                                    e.preventDefault();
                                    si.focus();
                                }
                            });
                        })();

                        // Poem: SSE morph.
                        (function connectSSE() {
                            const es = new EventSource('/sse');
                            es.onmessage = (e) => {
                                const [sel, ...rest] = e.data.split('\n');
                                const el = document.querySelector(sel);
                                if (el) Idiomorph.morph(el, rest.join('\n'));
                            };
                            es.onerror = () => { es.close(); setTimeout(connectSSE, 3000); };
                        })();
                    })();
                "#)) }
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

/// Input is canonicalized without leading '@' (usually uuid:rig:provider/model).
pub(super) fn actor_label(actor: &str) -> String {
    let a = actor.trim_start_matches('@').trim();
    let parts: Vec<&str> = a.split(':').collect();
    if parts.len() >= 3 {
        let uuid = parts[0].trim();
        let rig = parts[1].trim();
        let model = parts[2].trim();
        let uuid8 = uuid.chars().take(8).collect::<String>();
        if !uuid8.is_empty() && !rig.is_empty() && !model.is_empty() {
            return format!("{uuid8}:{rig}:{model}");
        }
    }
    a.to_string()
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
pub(super) fn linkify_slugs(raw: &str) -> String {
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
                out.push_str(r#"<a href="/~/"#);
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

pub(super) fn render_linkified_with_embeds(raw: &str) -> Markup {
    let embeds = extract_embed_frames(raw);
    html! {
        pre { (maud::PreEscaped(linkify_slugs(raw))) }
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
