use crate::path_types::{tilde_http_path_to_item_id, ItemId};
use slug_types::SLUG_TILDE_ONTOLOGY_ROOT;

/// Semantic view of an ontology path for rendering and routing decisions.
pub(super) struct OntologyPath {
    item: ItemId,
    /// Breadcrumb segments: for `~/a/b` this is `["a", "b"]` (leading `~` rendered separately).
    segments: Vec<String>,
}

impl OntologyPath {
    /// Path is the `*path` segment from `/~/*path` (e.g. `topic/a`). Always treat it as under `~/`
    /// so it resolves to `https://slug.social/~/…`, not the non-tilde site path.
    pub(super) fn from_input(path: &str) -> Self {
        let item = tilde_http_path_to_item_id(path);
        Self::from_item(item)
    }

    pub(super) fn from_item(item: ItemId) -> Self {
        // tilde_segments() returns ["~", "a", "b"] but bc_path() renders "~" itself,
        // so we skip the leading "~" segment here.
        let segments = item
            .tilde_segments()
            .into_iter()
            .skip(1) // drop the leading "~"
            .map(|s| s.to_string())
            .collect();
        Self { item, segments }
    }

    pub(super) fn root() -> Self {
        Self::from_item(ItemId::ontology_root())
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
        self.item.as_str()
    }
}

/// External `https://…` items use `/-/https://host/…` in the URL bar (full URL after the prefix).
pub(super) struct ExternalOntologyPath {
    item: ItemId,
    /// Outermost external identity first (e.g. `https://host`), ending with `item`.
    chain: Vec<ItemId>,
}

fn external_ancestor_chain(item: &ItemId) -> Vec<ItemId> {
    let s = item.as_str();
    // Sentinel used for the bare `/-/` index route.
    if s == "https://." {
        return vec![];
    }
    let mut rev = vec![item.clone()];
    loop {
        let cur = rev.last().expect("non-empty");
        let Some(p) = cur.parent() else {
            break;
        };
        let ps = p.as_str();
        if ps == SLUG_TILDE_ONTOLOGY_ROOT || ps.starts_with("https://slug.social/~/") {
            break;
        }
        if !(ps.starts_with("http://") || ps.starts_with("https://")) {
            break;
        }
        rev.push(p);
    }
    rev.reverse();
    rev
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
        let Some(parsed) = ItemId::parse(&raw) else {
            return Self::from_item(ItemId::opaque("https://.".to_string()));
        };
        Self::from_item(parsed)
    }

    pub(super) fn from_item(item: ItemId) -> Self {
        let chain = external_ancestor_chain(&item);
        Self { item, chain }
    }

    pub(super) fn is_root(&self) -> bool {
        self.chain.is_empty()
    }

    /// Ancestor [`ItemId`]s from host root to current (inclusive), for external breadcrumbs.
    pub(super) fn breadcrumb_chain(&self) -> &[ItemId] {
        &self.chain
    }

    pub(super) fn as_str(&self) -> &str {
        self.item.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_root_and_host_paths_are_distinct() {
        let root = ExternalOntologyPath::from_input("");
        assert!(root.is_root());
        assert!(root.breadcrumb_chain().is_empty());

        let host = ExternalOntologyPath::from_input("example.com");
        assert!(!host.is_root());
        assert_eq!(host.breadcrumb_chain().len(), 1);
        assert_eq!(
            host.breadcrumb_chain()[0].as_str(),
            "https://example.com"
        );
    }

    #[test]
    fn external_path_keeps_each_url_segment_for_breadcrumbs() {
        let path = ExternalOntologyPath::from_input("https://example.com/a/b");
        assert_eq!(path.breadcrumb_chain().len(), 3);
        assert_eq!(
            path.breadcrumb_chain()[0].as_str(),
            "https://example.com"
        );
        assert_eq!(
            path.breadcrumb_chain()[1].as_str(),
            "https://example.com/a"
        );
        assert_eq!(
            path.breadcrumb_chain()[2].as_str(),
            "https://example.com/a/b"
        );
    }
}
