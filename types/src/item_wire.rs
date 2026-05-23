//! Wire normalization for item identity strings (`canonicalize_item`, path segments).
//! Split from `paths` so [`crate::item_id::ItemId`] can depend on this without import cycles.

use crate::url_normalize::{host_preserves_dash_path_case, normalize_http_identity_url};

/// Canonical absolute URL for the tilde ontology **root** (`~/` in UI).
pub const SLUG_TILDE_ONTOLOGY_ROOT: &str = "https://slug.social/~";

/// Collapse legacy or parser variants of the ontology root to [`SLUG_TILDE_ONTOLOGY_ROOT`].
pub fn normalize_slug_ontology_storage_url(s: &str) -> String {
    if s == "https://slug.social/~/" {
        SLUG_TILDE_ONTOLOGY_ROOT.to_string()
    } else {
        s.to_string()
    }
}

fn finalize_external_identity_url(s: String) -> String {
    if s.starts_with("https://slug.social/") {
        return s;
    }
    let normalized = normalize_http_identity_url(&s).unwrap_or_else(|| s.clone());
    strip_redundant_root_slash(&normalized).unwrap_or(normalized)
}

/// `url::Url` serializes bare hosts with a `/` path; we keep host-only items slash-free for stable
/// keys matching the pre-normalizer spellings.
fn strip_redundant_root_slash(s: &str) -> Option<String> {
    let u = url::Url::parse(s).ok()?;
    if u.path() == "/" && u.query().is_none() && u.fragment().is_none() {
        let scheme = u.scheme();
        let host = u.host_str()?;
        return Some(match u.port() {
            Some(p) => format!("{scheme}://{host}:{p}"),
            None => format!("{scheme}://{host}"),
        });
    }
    None
}

/// Ontology item reference → canonical absolute URL on the slug host.
pub fn canonicalize_item(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }

    if let Some(rest) = s.strip_prefix("-/") {
        let (host, tail) = rest
            .split_once('/')
            .map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if host.is_empty() {
            return String::new();
        }
        let preserve_case = host_preserves_dash_path_case(&host);
        return if tail.is_empty() {
            finalize_external_identity_url(format!("https://{host}"))
        } else {
            let path = tail
                .trim_start_matches('/')
                .trim_end_matches('/')
                .split('/')
                .filter_map(|seg| {
                    let t = seg.trim();
                    if t.is_empty() {
                        None
                    } else if preserve_case {
                        Some(t.to_string())
                    } else {
                        Some(t.to_lowercase())
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            finalize_external_identity_url(format!("https://{host}/{path}"))
        };
    }

    if let Some(rest) = s.strip_prefix("https://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        return finalize_external_identity_url(if tail.is_empty() {
            format!("https://{host}")
        } else {
            format!("https://{host}/{tail}")
        });
    }
    if let Some(rest) = s.strip_prefix("http://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        return finalize_external_identity_url(if tail.is_empty() {
            format!("http://{host}")
        } else {
            format!("http://{host}/{tail}")
        });
    }

    let is_tilde = s.starts_with("~/");
    let rest = s.strip_prefix("~/").or_else(|| s.strip_prefix("/")).unwrap_or(s);

    let tail = rest
        .split('/')
        .filter_map(|seg| {
            let t = seg.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_lowercase())
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    if is_tilde {
        if tail.is_empty() {
            return SLUG_TILDE_ONTOLOGY_ROOT.to_string();
        }
        // `~/github.com/…` is a common mistake for external `-/github.com/…`.
        if tail
            .split('/')
            .next()
            .is_some_and(|seg| seg.contains('.'))
        {
            return finalize_external_identity_url(format!("https://{tail}"));
        }
        format!("https://slug.social/~/{tail}")
    } else if tail.is_empty() {
        "https://slug.social".to_string()
    } else if tail
        .split('/')
        .next()
        .is_some_and(|seg| seg.contains('.'))
    {
        finalize_external_identity_url(format!("https://{tail}"))
    } else {
        format!("https://slug.social/{tail}")
    }
}

/// True when user input refers to an off-site item (`-/…`, URL, or `host.tld/…`).
pub fn is_external_path_input(input: &str) -> bool {
    let s = input.trim();
    if s.starts_with("-/")
        || s.starts_with("http://")
        || s.starts_with("https://")
    {
        return true;
    }
    let rest = s
        .strip_prefix("~/")
        .or_else(|| s.strip_prefix('/'))
        .unwrap_or(s);
    rest.split('/')
        .next()
        .is_some_and(|seg| !seg.is_empty() && seg.contains('.'))
}

/// CLI / browse display path (`~/…` native, `-/…` external) from a stored item id string.
pub fn garden_display_path(stored: &str) -> String {
    crate::ItemId::parse(stored)
        .map(|id| id.display_path())
        .unwrap_or_else(|| stored.to_string())
}

pub fn item_path_segments(input: &str) -> Vec<String> {
    let canonical = canonicalize_item(input);
    if canonical.is_empty() {
        return vec![];
    }

    if let Some(rest) = canonical.strip_prefix("https://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let mut out = vec![format!("https://{}", host)];
        out.extend(tail.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()));
        return out;
    }
    if let Some(rest) = canonical.strip_prefix("http://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let mut out = vec![format!("http://{}", host)];
        out.extend(tail.split('/').filter(|s| !s.is_empty()).map(|s| s.to_string()));
        return out;
    }

    canonical
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

pub fn item_parent_path(input: &str) -> Option<String> {
    let segs = item_path_segments(input);
    if segs.len() <= 1 {
        return None;
    }
    Some(segs[..segs.len() - 1].join("/"))
}

pub(crate) fn external_display_dash_prefix(host_and_path: &str) -> String {
    let (host, path) = host_and_path
        .split_once('/')
        .map_or((host_and_path, ""), |(h, p)| (h, p));
    let host = host.trim().to_lowercase();
    let path = path
        .trim_end_matches('/')
        .split('/')
        .filter_map(|seg| {
            let t = seg.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_lowercase())
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if path.is_empty() {
        format!("-/{host}")
    } else {
        format!("-/{host}/{path}")
    }
}
