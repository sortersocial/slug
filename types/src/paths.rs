//! Canonical paths, storage ids, and JSON href newtypes. All normalization and
//! room-aware URL rules for items live here.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Normalization (moved from server `canonical_path`)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Storage + input path newtypes
// ---------------------------------------------------------------------------

/// Canonical item identifier as produced by [`canonicalize_item`].
///
/// Shared across all scopes; room is not embedded. Usually
/// `https://slug.social/~/…` or an external `http(s)://…` URL item.
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

    pub fn tilde_tail(&self) -> Option<&str> {
        self.0.strip_prefix("https://slug.social/~/")
    }

    pub fn last_segment(&self) -> &str {
        self.0
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(self.0.as_str())
    }

    pub fn ontology_root() -> Self {
        Self("https://slug.social/~".to_string())
    }

    pub fn parent(&self) -> Option<Self> {
        if self.tilde_tail().map(|t| t.is_empty()).unwrap_or(true) {
            return None;
        }
        let last_slash = self.0.rfind('/')?;
        let parent_str = &self.0[..last_slash];
        if parent_str.is_empty() {
            None
        } else {
            Some(Self(parent_str.to_string()))
        }
    }

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

    /// `~/…` list label for ontology items (paths index, CLI).
    pub fn tilde_list_label(&self) -> TildeOntologyPath {
        TildeOntologyPath::from_stored(self)
    }

    /// Absolute href for JSON/RPC and browsers for this stored id in `room`.
    pub fn json_href(&self, room_wire: &str) -> GardenItemUrl {
        GardenItemUrl::from_stored(self, room_wire)
    }
}

impl fmt::Display for CanonicalItemUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RelativePath(pub String);

impl RelativePath {
    pub fn new(input: &str) -> Option<Self> {
        let s = input.trim().trim_matches('/');
        if s.is_empty() {
            Some(Self(String::new()))
        } else {
            Some(Self(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn join_under_ontology_root(&self, root: &CanonicalItemUrl) -> Option<CanonicalItemUrl> {
        let base = root.tilde_tail()?;
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

// ---------------------------------------------------------------------------
// Wire / JSON: correct-by-construction hrefs
// ---------------------------------------------------------------------------

fn api_path_or_url(item: &str) -> String {
    if item.starts_with("http://") || item.starts_with("https://") {
        item.to_string()
    } else {
        format!("/{}", item)
    }
}

/// Ontology item as serialized in JSON (absolute URL or `/`-prefixed path).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GardenItemUrl(pub String);

impl GardenItemUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    /// Stored canonical id + RPC `room` field (`"public"` or `"short/slug"`).
    pub fn from_stored(stored: &CanonicalItemUrl, room_wire: &str) -> Self {
        Self(garden_href_string(stored.as_str(), room_wire))
    }

    /// Like [`Self::from_stored`] but accepts a string that may already be canonical.
    pub fn from_storage_str(stored: &str, room_wire: &str) -> Self {
        Self(garden_href_string(stored, room_wire))
    }
}

impl fmt::Display for GardenItemUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for GardenItemUrl {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn garden_href_string(item: &str, room_wire: &str) -> String {
    let room = room_wire.trim();
    if room.is_empty() || room == "public" {
        return api_path_or_url(item);
    }
    let Some((short, slug)) = room.split_once('/') else {
        return api_path_or_url(item);
    };
    if short.is_empty() || slug.is_empty() {
        return api_path_or_url(item);
    }
    let Some(c) = CanonicalItemUrl::parse(item) else {
        return api_path_or_url(item);
    };
    let root = CanonicalItemUrl::ontology_root();
    let item_norm = c.as_str().trim_end_matches('/');
    let root_norm = root.as_str().trim_end_matches('/');
    if let Some(tail) = c.tilde_tail() {
        return if tail.is_empty() {
            format!("https://slug.social/r/{short}/{slug}/~")
        } else {
            format!("https://slug.social/r/{short}/{slug}/~/{}", tail)
        };
    }
    if item_norm == root_norm {
        return format!("https://slug.social/r/{short}/{slug}/~");
    }
    api_path_or_url(item)
}

/// Forum thread URL for JSON (`/t/…` or `/r/…/t/…` on slug.social).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ForumThreadUrl(pub String);

impl ForumThreadUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn from_room_tag(room_wire: &str, thread_tag: &str) -> Self {
        let room = room_wire.trim();
        let tag = thread_tag.trim().trim_start_matches('#');
        Self(if room.is_empty() || room == "public" {
            format!("https://slug.social/t/{tag}")
        } else if let Some((short, slug)) = room.split_once('/') {
            if short.is_empty() || slug.is_empty() {
                format!("https://slug.social/t/{tag}")
            } else {
                format!("https://slug.social/r/{short}/{slug}/t/{tag}")
            }
        } else {
            format!("https://slug.social/t/{tag}")
        })
    }
}

impl fmt::Display for ForumThreadUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for ForumThreadUrl {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// `~/a/b` style path for list UIs (paths index `path` field).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TildeOntologyPath(pub String);

impl TildeOntologyPath {
    pub fn from_stored(c: &CanonicalItemUrl) -> Self {
        let s = match c.tilde_tail() {
            Some(tail) if !tail.is_empty() => format!("~/{}", tail),
            Some(_) => "~/".to_string(),
            None => c.to_string(),
        };
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TildeOntologyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for TildeOntologyPath {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
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

    #[test]
    fn garden_item_url_deref_to_str() {
        let g = GardenItemUrl::from_storage_str("https://slug.social/~/x", "public");
        let s: &str = &*g;
        assert_eq!(s, "https://slug.social/~/x");
    }

    #[test]
    fn garden_public_passthrough_https() {
        let u = "https://slug.social/~/a/b";
        assert_eq!(GardenItemUrl::from_storage_str(u, "public").as_str(), u);
    }

    #[test]
    fn garden_private_room_prefixes_ontology() {
        assert_eq!(
            GardenItemUrl::from_storage_str("https://slug.social/~/topic/x", "9ab12cd/my-room").as_str(),
            "https://slug.social/r/9ab12cd/my-room/~/topic/x"
        );
    }

    #[test]
    fn garden_private_room_ontology_root() {
        assert_eq!(
            GardenItemUrl::from_storage_str("https://slug.social/~", "9ab12cd/my-room").as_str(),
            "https://slug.social/r/9ab12cd/my-room/~"
        );
        assert_eq!(
            GardenItemUrl::from_storage_str("https://slug.social/~/", "9ab12cd/my-room").as_str(),
            "https://slug.social/r/9ab12cd/my-room/~"
        );
    }

    #[test]
    fn garden_external_url_untouched_in_private_room() {
        let u = "https://example.com/z";
        assert_eq!(GardenItemUrl::from_storage_str(u, "9ab12cd/my-room").as_str(), u);
    }

    #[test]
    fn forum_web_public_vs_room() {
        assert_eq!(
            ForumThreadUrl::from_room_tag("public", "debate").as_str(),
            "https://slug.social/t/debate"
        );
        assert_eq!(
            ForumThreadUrl::from_room_tag("9ab12cd/my-room", "#debate").as_str(),
            "https://slug.social/r/9ab12cd/my-room/t/debate"
        );
    }
}
