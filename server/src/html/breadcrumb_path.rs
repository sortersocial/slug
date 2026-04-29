use crate::path_types::CanonicalItemUrl;

/// Semantic view of an ontology path for rendering and routing decisions.
pub(super) struct OntologyPath {
    canonical: CanonicalItemUrl,
    /// Breadcrumb segments: for `~/a/b` this is `["a", "b"]` (leading `~` rendered separately).
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
        // tilde_segments() returns ["~", "a", "b"] but bc_path() renders "~" itself,
        // so we skip the leading "~" segment here.
        let segments = canonical
            .tilde_segments()
            .into_iter()
            .skip(1) // drop the leading "~"
            .map(|s| s.to_string())
            .collect();
        Self { canonical, segments }
    }

    pub(super) fn root() -> Self {
        Self::from_canonical(CanonicalItemUrl::parse("~/").unwrap())
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
        self.canonical.as_str()
    }
}

/// External `https://host/…` items addressed as `/-/host/…` in the URL bar.
pub(super) struct ExternalOntologyPath {
    canonical: CanonicalItemUrl,
    /// e.g. `["github.com", "org", "repo", "issues"]`
    segments: Vec<String>,
}

impl ExternalOntologyPath {
    pub(super) fn from_input(path: &str) -> Self {
        let p = path.trim_start_matches('/');
        let raw = if p.starts_with("http://") || p.starts_with("https://") {
            p.to_string()
        } else if p.is_empty() {
            "-/.".to_string()
        } else {
            format!("-/{}", p.trim_start_matches('/'))
        };
        let Some(canonical) = CanonicalItemUrl::parse(&raw) else {
            return Self::from_canonical(CanonicalItemUrl("https://.".to_string()));
        };
        Self::from_canonical(canonical)
    }

    pub(super) fn from_canonical(canonical: CanonicalItemUrl) -> Self {
        let s = canonical.as_str();
        let rest = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .unwrap_or("");
        let segments: Vec<String> = rest
            .split('/')
            .filter(|x| !x.is_empty())
            .map(|x| x.to_string())
            .collect();
        let segments = if segments == ["."] {
            vec![]
        } else {
            segments
        };
        Self { canonical, segments }
    }

    pub(super) fn is_root(&self) -> bool {
        self.segments.len() <= 1
    }

    pub(super) fn segments(&self) -> &[String] {
        &self.segments
    }

    pub(super) fn as_str(&self) -> &str {
        self.canonical.as_str()
    }
}
