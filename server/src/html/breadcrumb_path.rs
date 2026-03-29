use crate::path_types::CanonicalItemUrl;

/// Semantic view of an ontology path for rendering and routing decisions.
pub(super) struct OntologyPath {
    canonical: CanonicalItemUrl,
    /// Breadcrumb segments: for `~/a/b` this is `["~", "a", "b"]`.
    segments: Vec<String>,
}

impl OntologyPath {
    /// Path is the `*path` segment from `/~/*path` (e.g. `topic/a`). Always treat it as under `~/`
    /// so it canonicalizes to `https://slug.social/~/…`, not the non-tilde site path.
    pub(super) fn from_input(path: &str) -> Self {
        let p = path.trim_start_matches('/');
        let raw = if p.starts_with("http://") || p.starts_with("https://") {
            p.to_string()
        } else if p.is_empty() {
            "~/".to_string()
        } else {
            format!("~/{}", p)
        };
        let canonical = CanonicalItemUrl::parse(&raw)
            .unwrap_or_else(|| CanonicalItemUrl::parse("~/").unwrap());
        Self::from_canonical(canonical)
    }

    pub(super) fn from_canonical(canonical: CanonicalItemUrl) -> Self {
        // Use tilde_segments() so we get ["~", "a", "b"] rather than splitting
        // the full https://slug.social/~/a/b URL (which would include "https:" etc.).
        let segments = canonical
            .tilde_segments()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        Self { canonical, segments }
    }

    pub(super) fn root() -> Self {
        Self::from_canonical(CanonicalItemUrl::parse("~/").unwrap())
    }

    pub(super) fn is_root(&self) -> bool {
        self.segments.len() <= 1 // just ["~"] or empty
    }

    /// At ontology root, allow mode toggle to forum (`/`).
    /// In deeper ontology views, keep root breadcrumb within garden (`/~`).
    pub(super) fn slug_root_href(&self) -> &'static str {
        if self.is_root() {
            "/"
        } else {
            "/~"
        }
    }

    pub(super) fn segments(&self) -> &[String] {
        &self.segments
    }

    pub(super) fn as_str(&self) -> &str {
        self.canonical.as_str()
    }
}
