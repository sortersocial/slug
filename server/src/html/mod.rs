use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use maud::{html, Markup, DOCTYPE};

mod entry;
mod forum;
mod garden;

pub use forum::{index, thread_feed_html, thread_view};
pub use garden::{garden_index, ontology_path};

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

pub(super) fn layout(title: &str, view: &str, body: Markup) -> Markup {
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
                (body)
                div id="controls" {
                    div id="spread-control" {
                        span { "spread" }
                        input type="range" id="spread-slider" min="0" max="1" step="0.05" value="1";
                    }
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
pub(super) fn bc_path(path: &str) -> Markup {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let ns = segs.first().copied().unwrap_or(path);
    let thread_href = format!("/t/{ns}");
    html! {
        a href="/" { "slug.social" }
        (bc_segment("~", "/~", false))
        @for (i, seg) in segs.iter().enumerate() {
            @let href = format!("/~/{}", segs[..=i].join("/"));
            @let is_last = i == segs.len() - 1;
            (bc_segment(seg, &href, is_last))
        }
        span class="bc-side" {
            a href=(thread_href) { "#" (ns) }
        }
    }
}

/// Render the thread path breadcrumb: `slug.social / #tag`
/// with an optional side-link to the ontology root for this tag.
pub(super) fn bc_thread(tag: &str, side_ontology: bool) -> Markup {
    let thread_href = format!("/t/{tag}");
    let ontology_href = format!("/~/{tag}");
    html! {
        a href="/" { "slug.social" }
        (bc_segment(&format!("#{tag}"), &thread_href, true))
        @if side_ontology {
            span class="bc-side" {
                a href=(ontology_href) { "~" }
            }
        }
    }
}

/// The display path of an item relative to its namespace root (strips `ns/` prefix).
pub(super) fn item_display_path(ns: &str, item: &str) -> String {
    let c = crate::events::canonicalize_item(item);
    c.strip_prefix(&format!("{ns}/"))
        .unwrap_or(c.as_str())
        .to_string()
}

/// The URL for an item under `/~`.
pub(super) fn item_href(ns: &str, item: &str) -> String {
    format!("/~/{}/{}", ns, item_display_path(ns, item))
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
