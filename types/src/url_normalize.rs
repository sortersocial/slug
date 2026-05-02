//! Normalization for external `http(s)://` item identity (not slug tilde ontology).
//!
//! Policy (intentional, extend here as new domains need treatment):
//! - Query pairs sorted lexicographically by **lowercased** key, then value.
//! - YouTube family → stable `www.youtube.com` shapes where possible.

use url::Url;

/// Whether `-/` → `https://…` path segments should keep original casing (YouTube video IDs are
/// case-sensitive).
pub(crate) fn host_preserves_dash_path_case(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let h = h.strip_prefix("www.").unwrap_or(h.as_str());
    matches!(
        h,
        "youtu.be" | "youtube.com" | "m.youtube.com" | "music.youtube.com"
    )
}

/// Normalize external http(s) URLs for stable [`super::paths::canonicalize_item`] output.
pub fn normalize_http_identity_url(s: &str) -> Option<String> {
    let mut u = Url::parse(s).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    rewrite_youtube(&mut u);
    sort_query_pairs(&mut u);
    Some(u.to_string())
}

fn base_host(host: &str) -> String {
    let lower = host.to_ascii_lowercase();
    lower
        .strip_prefix("www.")
        .unwrap_or(lower.as_str())
        .to_string()
}

fn rewrite_youtube(u: &mut Url) {
    let Some(host_raw) = u.host_str() else {
        return;
    };
    let base = base_host(host_raw);
    let path = u.path().to_string();

    match base.as_str() {
        "youtu.be" => {
            let id = path.trim_start_matches('/').split('/').next().unwrap_or("").to_string();
            if id.is_empty() {
                return;
            }
            let saved: Vec<(String, String)> = u.query_pairs().into_owned().collect();
            let Ok(mut out) = Url::parse(&format!("https://www.youtube.com/watch?v={id}")) else {
                return;
            };
            {
                let mut q = out.query_pairs_mut();
                for (k, v) in saved {
                    if k.eq_ignore_ascii_case("v") {
                        continue;
                    }
                    q.append_pair(&k, &v);
                }
            }
            *u = out;
        }
        "youtube.com" | "m.youtube.com" => {
            if base == "m.youtube.com" {
                let _ = u.set_host(Some("www.youtube.com"));
            }
            if path.starts_with("/embed/") {
                let id = path
                    .strip_prefix("/embed/")
                    .unwrap_or("")
                    .trim_matches('/')
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    return;
                }
                let saved: Vec<(String, String)> = u.query_pairs().into_owned().collect();
                let Ok(mut out) = Url::parse(&format!("https://www.youtube.com/watch?v={id}"))
                else {
                    return;
                };
                {
                    let mut q = out.query_pairs_mut();
                    for (k, v) in saved {
                        if k.eq_ignore_ascii_case("v") {
                            continue;
                        }
                        q.append_pair(&k, &v);
                    }
                }
                *u = out;
                return;
            }
            if path.starts_with("/v/") {
                let id = path
                    .strip_prefix("/v/")
                    .unwrap_or("")
                    .trim_matches('/')
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    return;
                }
                let saved: Vec<(String, String)> = u.query_pairs().into_owned().collect();
                let Ok(mut out) = Url::parse(&format!("https://www.youtube.com/watch?v={id}"))
                else {
                    return;
                };
                {
                    let mut q = out.query_pairs_mut();
                    for (k, v) in saved {
                        if k.eq_ignore_ascii_case("v") {
                            continue;
                        }
                        q.append_pair(&k, &v);
                    }
                }
                *u = out;
                return;
            }
            if path.starts_with("/watch") {
                let _ = u.set_host(Some("www.youtube.com"));
                return;
            }
            if path.starts_with("/shorts/") {
                let id = path
                    .strip_prefix("/shorts/")
                    .unwrap_or("")
                    .trim_matches('/')
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    return;
                }
                let saved: Vec<(String, String)> = u.query_pairs().into_owned().collect();
                let Ok(mut out) = Url::parse(&format!("https://www.youtube.com/shorts/{id}"))
                else {
                    return;
                };
                {
                    let mut q = out.query_pairs_mut();
                    for (k, v) in saved {
                        q.append_pair(&k, &v);
                    }
                }
                *u = out;
                return;
            }
            let _ = u.set_host(Some("www.youtube.com"));
        }
        "music.youtube.com" => {}
        _ => {}
    }
}

fn sort_query_pairs(u: &mut Url) {
    let pairs: Vec<(String, String)> = u.query_pairs().into_owned().collect();
    if pairs.is_empty() {
        u.set_query(None);
        return;
    }
    let mut pairs = pairs;
    pairs.sort_by(|a, b| {
        a.0.to_ascii_lowercase()
            .cmp(&b.0.to_ascii_lowercase())
            .then_with(|| a.1.cmp(&b.1))
    });
    u.set_query(None);
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in pairs {
            q.append_pair(&k, &v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_youtu_be_to_watch() {
        assert_eq!(
            normalize_http_identity_url("https://youtu.be/dQw4w9WgXcQ").as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
    }

    #[test]
    fn youtube_watch_query_sorted() {
        assert_eq!(
            normalize_http_identity_url("https://youtube.com/watch?v=Z&a=1&b=2").as_deref(),
            Some("https://www.youtube.com/watch?a=1&b=2&v=Z")
        );
        assert_eq!(
            normalize_http_identity_url("https://youtube.com/watch?b=2&a=1&v=Z").as_deref(),
            Some("https://www.youtube.com/watch?a=1&b=2&v=Z")
        );
    }

    #[test]
    fn youtube_embed_to_watch() {
        assert_eq!(
            normalize_http_identity_url("https://www.youtube.com/embed/dQw4w9WgXcQ").as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
    }

    #[test]
    fn youtube_shorts_host() {
        assert_eq!(
            normalize_http_identity_url("https://youtube.com/shorts/AbCdEfGhIjK").as_deref(),
            Some("https://www.youtube.com/shorts/AbCdEfGhIjK")
        );
    }

    #[test]
    fn arbitrary_query_sorted() {
        assert_eq!(
            normalize_http_identity_url("https://example.com/x?z=1&a=2").as_deref(),
            Some("https://example.com/x?a=2&z=1")
        );
    }
}
