//! Normalization for thread tags and ontology item URLs (DSL ↔ stored canonical form).
//! Not event types — see `events` and `path_types`.

/// Thread / public tag: stored without leading `#`, lowercase.
pub fn canonicalize_tag(input: &str) -> String {
    input.trim().trim_start_matches('#').to_lowercase()
}

/// Ontology item reference → canonical absolute URL on the slug host.
pub fn canonicalize_item(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }

    if let Some(rest) = s.strip_prefix("https://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if tail.is_empty() {
            return format!("https://{}", host);
        } else {
            return format!("https://{}/{}", host, tail);
        }
    }
    if let Some(rest) = s.strip_prefix("http://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if tail.is_empty() {
            return format!("http://{}", host);
        } else {
            return format!("http://{}/{}", host, tail);
        }
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
        format!("https://slug.social/~/{}", tail)
    } else if tail.is_empty() {
        "https://slug.social".to_string()
    } else {
        format!("https://slug.social/{}", tail)
    }
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
