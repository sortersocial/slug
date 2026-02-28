/// Semantic view of an ontology path for breadcrumb rendering.
pub(super) struct OntologyPath<'a> {
    segments: Vec<&'a str>,
}

impl<'a> OntologyPath<'a> {
    pub(super) fn parse(path: &'a str) -> Self {
        Self {
            segments: path.split('/').filter(|s| !s.is_empty()).collect(),
        }
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

    pub(super) fn segments(&self) -> &[&'a str] {
        &self.segments
    }
}
