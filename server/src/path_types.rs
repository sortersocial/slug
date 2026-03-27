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

use crate::events::canonicalize_item;

/// Canonical item identifier as produced by `events::canonicalize_item`.
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

