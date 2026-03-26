use crate::events::canonicalize_item;

/// Semantic view of an ontology path for rendering and routing decisions.
pub(super) struct OntologyPath {
    canonical: String,
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
        Self::from_canonical(canonicalize_item(&raw))
    }

    pub(super) fn from_canonical(path: String) -> Self {
        let segments = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
        Self {
            canonical: path,
            segments,
        }
    }

    pub(super) fn root() -> Self {
        Self::from_canonical(String::new())
    }

    pub(super) fn is_root(&self) -> bool {
        self.segments.is_empty()
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
        &self.canonical
    }
}
