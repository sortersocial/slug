//! Path representation types.
//!
//! The codebase currently treats item identifiers as strings in a few different
//! encodings:
//! - user/DSL input like `~/a/b`
//! - canonical item URLs like `https://slug.social/~/a/b`
//! - relative paths within a rooted tree view (e.g. `llms/openai` under a root)
//!
//! This module adds lightweight newtypes so code can be explicit about what it
//! expects without changing core storage formats.

use std::borrow::Borrow;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::canonical_path::canonicalize_item;

/// Canonical item identifier as produced by `canonical_path::canonicalize_item`.
///
/// In practice this is usually:
/// - `https://slug.social/~/...` for ontology items, or
/// - `https://...` / `http://...` for URL items.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CanonicalItemUrl(pub String);

impl CanonicalItemUrl {
    pub fn parse(input: &str) -> Option<Self> {
        let c = canonicalize_item(input);
        if c.is_empty() {
            None
        } else {
            Some(Self(c))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the `~/...` tail for ontology items (`https://slug.social/~/...`).
    pub fn tilde_tail(&self) -> Option<&str> {
        self.0.strip_prefix("https://slug.social/~/")
    }

    /// Returns the final non-empty `/`-separated segment of the path.
    ///
    /// `https://slug.social/~/a/b/c` → `"c"`
    /// `https://slug.social/~/a`     → `"a"`
    pub fn last_segment(&self) -> &str {
        self.0
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(self.0.as_str())
    }

    /// The ontology root key as stored in `item_children`: `"https://slug.social/~"`.
    /// Use this (not `parse("~/")`) when looking up top-level children.
    pub fn ontology_root() -> Self {
        Self("https://slug.social/~".to_string())
    }

    /// Returns the parent of this canonical item URL by stripping the last
    /// path segment, or `None` if there is no parent (already at root).
    ///
    /// `https://slug.social/~/a/b/c` → `Some("https://slug.social/~/a/b")`
    /// `https://slug.social/~/a`     → `Some("https://slug.social/~")`
    /// `https://slug.social/~/`      → `None`  (tilde root)
    pub fn parent(&self) -> Option<Self> {
        // tilde_tail() is None for non-ontology URLs and "" for the root ~/
        if self.tilde_tail().map(|t| t.is_empty()).unwrap_or(true) {
            return None;
        }
        // Strip everything from the last '/' onwards.
        let last_slash = self.0.rfind('/')?;
        let parent_str = &self.0[..last_slash];
        if parent_str.is_empty() {
            None
        } else {
            Some(Self(parent_str.to_string()))
        }
    }

    /// Segments of an ontology path suitable for breadcrumb rendering.
    /// Strips the `https://slug.social` prefix and returns the `~/…` parts.
    ///
    /// `https://slug.social/~/a/b` → `["~", "a", "b"]`
    /// `https://slug.social/~/`    → `["~"]`
    pub fn tilde_segments(&self) -> Vec<&str> {
        match self.tilde_tail() {
            Some(tail) if !tail.is_empty() => {
                std::iter::once("~")
                    .chain(tail.split('/').filter(|s| !s.is_empty()))
                    .collect()
            }
            Some(_) => vec!["~"],
            None => vec![],
        }
    }
}

impl fmt::Display for CanonicalItemUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Allow `HashMap<CanonicalItemUrl, _>` to be searched by `&str`.
impl Borrow<str> for CanonicalItemUrl {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for CanonicalItemUrl {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for CanonicalItemUrl {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for CanonicalItemUrl {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// A `~/...` input path (as used in the DSL and UX).
///
/// This is not canonicalized; it is a presentation/input form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TildePath(pub String);

impl TildePath {
    pub fn new(input: &str) -> Option<Self> {
        let s = input.trim();
        if s.starts_with("~/") && s.len() > 2 {
            Some(Self(s.to_string()))
        } else if s == "~/" {
            Some(Self("~/".to_string()))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn canonicalize(&self) -> Option<CanonicalItemUrl> {
        CanonicalItemUrl::parse(&self.0)
    }
}

impl fmt::Display for TildePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A path relative to a chosen root in a tree UI.
///
/// This is intended for compact state encodings (blobs). It must be joined to a
/// root `CanonicalItemUrl` (typically an ontology root) to become a full item.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RelativePath(pub String);

impl RelativePath {
    pub fn new(input: &str) -> Option<Self> {
        let s = input.trim().trim_matches('/');
        if s.is_empty() {
            Some(Self(String::new()))
        } else {
            // Keep this permissive: the DSL parser is the main gatekeeper.
            Some(Self(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Join this relative path under a canonical ontology root
    /// (`https://slug.social/~/...`) to form a canonical item URL.
    pub fn join_under_ontology_root(&self, root: &CanonicalItemUrl) -> Option<CanonicalItemUrl> {
        let base = root.tilde_tail()?;
        // base is the tail after https://slug.social/~/, e.g. "models" or "models/llms"
        let joined = if base.is_empty() {
            if self.0.is_empty() {
                "~/".to_string()
            } else {
                format!("~/{}", self.0)
            }
        } else if self.0.is_empty() {
            format!("~/{}", base)
        } else {
            format!("~/{}/{}", base.trim_end_matches('/'), self.0)
        };
        CanonicalItemUrl::parse(&joined)
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_parent_deep() {
        let c = CanonicalItemUrl::parse("~/a/b/c").unwrap();
        assert_eq!(c.parent().unwrap().as_str(), "https://slug.social/~/a/b");
    }

    #[test]
    fn canonical_parent_one_level() {
        let c = CanonicalItemUrl::parse("~/a").unwrap();
        assert_eq!(c.parent().unwrap().as_str(), "https://slug.social/~");
    }

    #[test]
    fn canonical_parent_root_is_none() {
        let root = CanonicalItemUrl::parse("~/").unwrap();
        assert!(root.parent().is_none());
    }

    #[test]
    fn tilde_segments_deep() {
        let c = CanonicalItemUrl::parse("~/a/b").unwrap();
        assert_eq!(c.tilde_segments(), vec!["~", "a", "b"]);
    }

    #[test]
    fn tilde_segments_root() {
        let c = CanonicalItemUrl::parse("~/").unwrap();
        assert_eq!(c.tilde_segments(), vec!["~"]);
    }

    #[test]
    fn tilde_segments_non_ontology_is_empty() {
        let c = CanonicalItemUrl::parse("https://example.com/foo").unwrap();
        assert_eq!(c.tilde_segments(), Vec::<&str>::new());
    }
}
