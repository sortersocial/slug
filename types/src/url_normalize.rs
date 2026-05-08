//! Normalization for external `http(s)://` item identity (not slug tilde ontology).
//!
//! Policy (intentional, extend here as new domains need treatment):
//! - Query pairs are navigation/filter state by default and are stripped from identity.
//! - Known query-primary resources may canonicalize to path-based short links (YouTube `youtu.be/:id`).
//! - Fragments are stripped from identity.
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
    u.set_fragment(None);
    rewrite_youtube(&mut u);
    normalize_github(&mut u);
    u.set_query(None);
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
            let id = path
                .trim_start_matches('/')
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                return;
            }
            let Ok(out) = Url::parse(&format!("https://youtu.be/{id}")) else {
                return;
            };
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
                let Ok(out) = Url::parse(&format!("https://youtu.be/{id}")) else {
                    return;
                };
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
                let Ok(out) = Url::parse(&format!("https://youtu.be/{id}")) else {
                    return;
                };
                *u = out;
                return;
            }
            if path.starts_with("/watch") {
                let _ = u.set_host(Some("www.youtube.com"));
                if let Some((_, id)) = u.query_pairs().find(|(k, _)| k.eq_ignore_ascii_case("v")) {
                    if !id.is_empty() {
                        if let Ok(out) = Url::parse(&format!("https://youtu.be/{id}")) {
                            *u = out;
                        }
                    }
                }
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
                let Ok(out) = Url::parse(&format!("https://www.youtube.com/shorts/{id}")) else {
                    return;
                };
                *u = out;
                return;
            }
            let _ = u.set_host(Some("www.youtube.com"));
        }
        "music.youtube.com" => {}
        _ => {}
    }
}

fn normalize_github(u: &mut Url) {
    let Some(host_raw) = u.host_str() else {
        return;
    };
    if host_raw.to_ascii_lowercase() != "github.com" {
        return;
    }

    let Some(segments) = u.path_segments() else {
        return;
    };
    let mut segments: Vec<String> = segments
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if let Some(repo) = segments.get_mut(1) {
        if let Some(stripped) = repo.strip_suffix(".git") {
            *repo = stripped.to_string();
        }
    }

    if segments.is_empty() {
        u.set_path("/");
    } else {
        u.set_path(&format!("/{}", segments.join("/")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_youtu_be_to_watch() {
        assert_eq!(
            normalize_http_identity_url("https://youtu.be/dQw4w9WgXcQ").as_deref(),
            Some("https://youtu.be/dQw4w9WgXcQ")
        );
    }

    #[test]
    fn youtube_watch_v_query_becomes_path_child() {
        assert_eq!(
            normalize_http_identity_url("https://youtube.com/watch?v=Z&a=1&b=2").as_deref(),
            Some("https://youtu.be/Z")
        );
        assert_eq!(
            normalize_http_identity_url("https://youtube.com/watch?b=2&a=1&v=Z").as_deref(),
            Some("https://youtu.be/Z")
        );
    }

    #[test]
    fn youtube_embed_to_watch() {
        assert_eq!(
            normalize_http_identity_url("https://www.youtube.com/embed/dQw4w9WgXcQ").as_deref(),
            Some("https://youtu.be/dQw4w9WgXcQ")
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
    fn arbitrary_query_is_stripped() {
        assert_eq!(
            normalize_http_identity_url("https://example.com/x?z=1&a=2").as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn fragment_is_stripped_from_identity() {
        assert_eq!(
            normalize_http_identity_url("https://example.com/path?b=2#a-section").as_deref(),
            Some("https://example.com/path")
        );
    }

    #[test]
    fn all_generic_query_params_are_stripped() {
        assert_eq!(
            normalize_http_identity_url(
                "https://example.com/x?z=1&utm_source=newsletter&FbClId=abc&ref=share&a=2"
            )
            .as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn stripping_tracking_params_removes_empty_query() {
        assert_eq!(
            normalize_http_identity_url("https://example.com/x?utm_medium=email&si=share")
                .as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn youtube_identity_query_is_promoted_before_query_cleanup() {
        assert_eq!(
            normalize_http_identity_url("https://youtu.be/dQw4w9WgXcQ?si=share&v=ignored&t=12")
                .as_deref(),
            Some("https://youtu.be/dQw4w9WgXcQ")
        );
    }

    #[test]
    fn github_repo_suffix_and_noise_query_are_normalized() {
        assert_eq!(
            normalize_http_identity_url("https://github.com/ORG/REPO.git?tab=readme&q=is%3Aopen")
                .as_deref(),
            Some("https://github.com/org/repo")
        );
    }
}

/// `url::Url` as a `HashMap` key: `Eq` / `Hash` behavior (baseline if we store external ids as `Url`).
#[cfg(test)]
mod url_identity_tests {
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use url::Url;

    fn hash_one(url: &Url) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        url.hash(&mut h);
        h.finish()
    }

    #[test]
    fn identical_parse_strings_are_eq_and_share_hash_bucket() {
        let a = Url::parse("https://example.com/path").unwrap();
        let b = Url::parse("https://example.com/path").unwrap();
        assert_eq!(a, b);
        assert_eq!(hash_one(&a), hash_one(&b));

        let mut m: HashMap<Url, u32> = HashMap::new();
        m.insert(a, 1);
        *m.entry(b).or_default() += 10;
        assert_eq!(m.len(), 1);
        assert_eq!(m[&Url::parse("https://example.com/path").unwrap()], 11);
    }

    #[test]
    fn host_is_ascii_lowercase_in_eq() {
        let lower = Url::parse("https://examplE.com/").unwrap();
        let upper = Url::parse("https://EXAMPLE.com/").unwrap();
        assert_eq!(lower, upper);
        assert_eq!(hash_one(&lower), hash_one(&upper));
    }

    #[test]
    fn path_space_normalizes_to_percent_encoding_so_forms_merge() {
        let encoded = Url::parse("https://example.com/a%20b").unwrap();
        let decoded = Url::parse("https://example.com/a b").unwrap();
        assert_eq!(encoded, decoded);
        assert_eq!(hash_one(&encoded), hash_one(&decoded));

        let mut m: HashMap<Url, &str> = HashMap::new();
        m.insert(encoded, "first");
        assert_eq!(m.insert(decoded, "second"), Some("first"));
        assert_eq!(m.len(), 1);
        assert_eq!(m.values().next().copied(), Some("second"));
    }

    #[test]
    fn encoded_slash_in_segment_stays_distinct_from_real_path_separator() {
        let encoded = Url::parse("https://example.com/a%2Fb").unwrap();
        let real_slash = Url::parse("https://example.com/a/b").unwrap();
        assert_ne!(encoded, real_slash);
        assert_ne!(hash_one(&encoded), hash_one(&real_slash));
    }

    #[test]
    fn trailing_slash_on_path_is_significant_for_eq() {
        let with_slash = Url::parse("https://example.com/foo/").unwrap();
        let no_slash = Url::parse("https://example.com/foo").unwrap();
        assert_ne!(with_slash, no_slash);
        assert_ne!(hash_one(&with_slash), hash_one(&no_slash));
    }

    #[test]
    fn default_http_port_80_is_normalized_in_representation() {
        let explicit = Url::parse("http://example.com:80/").unwrap();
        let implicit = Url::parse("http://example.com/").unwrap();
        assert_eq!(explicit, implicit);
        assert_eq!(hash_one(&explicit), hash_one(&implicit));
    }

    #[test]
    fn default_https_port_443_is_normalized() {
        let explicit = Url::parse("https://example.com:443/foo").unwrap();
        let implicit = Url::parse("https://example.com/foo").unwrap();
        assert_eq!(explicit, implicit);
    }

    #[test]
    fn non_default_port_is_part_of_identity() {
        let a = Url::parse("https://example.com:444/").unwrap();
        let b = Url::parse("https://example.com:445/").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn empty_path_vs_slash_only_path_may_differ() {
        let root = Url::parse("https://example.com").unwrap();
        let slash = Url::parse("https://example.com/").unwrap();
        assert_eq!(
            root, slash,
            "root and trailing-slash-only merge for this parser"
        );
    }

    #[test]
    fn scheme_case_is_normalized_to_lowercase() {
        let lower = Url::parse("https://example.com/").unwrap();
        let upper = Url::parse("HTTPS://example.com/").unwrap();
        assert_eq!(lower, upper);
    }

    #[test]
    fn fragment_is_part_of_eq_and_hash() {
        let no_frag = Url::parse("https://example.com/a").unwrap();
        let frag = Url::parse("https://example.com/a#section").unwrap();
        assert_ne!(
            no_frag, frag,
            "#fragment is included in PartialEq — anchors are different HashMap keys"
        );
        assert_ne!(hash_one(&no_frag), hash_one(&frag));
    }

    #[test]
    fn query_order_and_encoding_can_split_identity() {
        let a = Url::parse("https://example.com/?b=2&a=1").unwrap();
        let b = Url::parse("https://example.com/?a=1&b=2").unwrap();
        assert_ne!(a, b, "query pairs order is preserved in serialization");

        let plus = Url::parse("https://example.com/?q=a+b").unwrap();
        let encoded = Url::parse("https://example.com/?q=a%20b").unwrap();
        assert_ne!(
            plus, encoded,
            "space as + vs %20 — different keys unless normalized"
        );
    }
}
