//! Canonical paths, storage ids, and JSON href newtypes. All normalization and
//! room-aware URL rules for items live here.
//!
//! ## String kinds (parse in this module only)
//!
//! - **[`canonicalize_item`] / [`CanonicalItemUrl`]** — graph storage key and DSL form; tilde
//!   ontology root is always [`SLUG_TILDE_ONTOLOGY_ROOT`] (no `…/~/` trailing slash only).
//! - **[`TildeHttpPathTail`]** — capture from `GET /~/*path` or `…/r/{short}{slug}/~/…` (the `*path` segment).
//! - **`-/…` wire form** — external items; see [`canonicalize_item`] dash branch.
//! - **[`GardenItemUrl`], [`ForumThreadUrl`]** — JSON / browser href surfaces.

use std::borrow::Borrow;
use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

use crate::room_route::room_route_segment;
use crate::url_normalize::{host_preserves_dash_path_case, normalize_http_identity_url};

// ---------------------------------------------------------------------------
// Slug tilde ontology (single storage form for `~/`)
// ---------------------------------------------------------------------------

/// Canonical absolute URL for the tilde ontology **root** (`~/` in UI). Used as the
/// `item_children` parent key for top-level items and must match [`CanonicalItemUrl::ontology_root`].
pub const SLUG_TILDE_ONTOLOGY_ROOT: &str = "https://slug.social/~";

/// Collapse legacy or parser variants of the ontology root to [`SLUG_TILDE_ONTOLOGY_ROOT`].
pub fn normalize_slug_ontology_storage_url(s: &str) -> String {
    if s == "https://slug.social/~/" {
        SLUG_TILDE_ONTOLOGY_ROOT.to_string()
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// Normalization (moved from server `canonical_path`)
// ---------------------------------------------------------------------------

/// Thread / public tag: stored without leading `#`, lowercase.
pub fn canonicalize_tag(input: &str) -> String {
    input.trim().trim_start_matches('#').to_lowercase()
}

fn finalize_external_identity_url(s: String) -> String {
    if s.starts_with("https://slug.social/") {
        return s;
    }
    let normalized = normalize_http_identity_url(&s).unwrap_or_else(|| s.clone());
    strip_redundant_root_slash(&normalized).unwrap_or(normalized)
}

/// `url::Url` serializes bare hosts with a `/` path; we keep host-only items slash-free for stable
/// keys matching the pre-normalizer spellings.
fn strip_redundant_root_slash(s: &str) -> Option<String> {
    let u = url::Url::parse(s).ok()?;
    if u.path() == "/" && u.query().is_none() && u.fragment().is_none() {
        let scheme = u.scheme();
        let host = u.host_str()?;
        return Some(match u.port() {
            Some(p) => format!("{scheme}://{host}:{p}"),
            None => format!("{scheme}://{host}"),
        });
    }
    None
}

/// Ontology item reference → canonical absolute URL on the slug host.
pub fn canonicalize_item(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }

    // External scope: `-/host/path` is the universal alias for `https://host/path`.
    if let Some(rest) = s.strip_prefix("-/") {
        let (host, tail) = rest
            .split_once('/')
            .map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        if host.is_empty() {
            return String::new();
        }
        let preserve_case = host_preserves_dash_path_case(&host);
        return if tail.is_empty() {
            finalize_external_identity_url(format!("https://{}", host))
        } else {
            let path = tail
                .trim_start_matches('/')
                .trim_end_matches('/')
                .split('/')
                .filter_map(|seg| {
                    let t = seg.trim();
                    if t.is_empty() {
                        None
                    } else if preserve_case {
                        Some(t.to_string())
                    } else {
                        Some(t.to_lowercase())
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            finalize_external_identity_url(format!("https://{}/{}", host, path))
        };
    }

    if let Some(rest) = s.strip_prefix("https://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        return finalize_external_identity_url(if tail.is_empty() {
            format!("https://{}", host)
        } else {
            format!("https://{}/{}", host, tail)
        });
    }
    if let Some(rest) = s.strip_prefix("http://") {
        let (host, tail) = rest.split_once('/').map_or((rest, ""), |(h, t)| (h, t));
        let host = host.trim().to_lowercase();
        return finalize_external_identity_url(if tail.is_empty() {
            format!("http://{}", host)
        } else {
            format!("http://{}/{}", host, tail)
        });
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
        if tail.is_empty() {
            return SLUG_TILDE_ONTOLOGY_ROOT.to_string();
        }
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

fn external_display_dash_prefix(host_and_path: &str) -> String {
    let (host, path) = host_and_path
        .split_once('/')
        .map_or((host_and_path, ""), |(h, p)| (h, p));
    let host = host.trim().to_lowercase();
    let path = path
        .trim_end_matches('/')
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
    if path.is_empty() {
        format!("-/{}", host)
    } else {
        format!("-/{}", format!("{}/{}", host, path))
    }
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
            Some(Self(normalize_slug_ontology_storage_url(&c)))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Collapses legacy slug ontology root spellings so [`HashMap`] keys match the reducer graph.
    pub fn normalized_storage(self) -> Self {
        Self(normalize_slug_ontology_storage_url(self.as_str()))
    }

    pub fn tilde_tail(&self) -> Option<&str> {
        if let Some(tail) = self.0.strip_prefix("https://slug.social/~/") {
            return Some(tail);
        }
        if self.0 == SLUG_TILDE_ONTOLOGY_ROOT || self.0 == "https://slug.social/~/" {
            return Some("");
        }
        None
    }

    pub fn last_segment(&self) -> &str {
        self.0
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(self.0.as_str())
    }

    pub fn ontology_root() -> Self {
        Self(SLUG_TILDE_ONTOLOGY_ROOT.to_string())
    }

    pub fn parent(&self) -> Option<Self> {
        if self.tilde_tail().is_some() {
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
        } else if let Some(rest) = self.0.strip_prefix("https://") {
            Self::parent_http_url("https://", rest)
        } else if let Some(rest) = self.0.strip_prefix("http://") {
            Self::parent_http_url("http://", rest)
        } else {
            None
        }
    }

    fn parent_http_url(scheme: &'static str, rest: &str) -> Option<Self> {
        let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
        let host = host.trim();
        let path = path.trim_end_matches('/');
        if path.is_empty() {
            return None;
        }
        let parent_path = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
        if parent_path.is_empty() {
            Some(Self(format!("{scheme}{}", host)))
        } else {
            Some(Self(format!("{scheme}{}/{}", host, parent_path)))
        }
    }

    /// `-/` representation for external `https://…` items, `~/…` for slug ontology, else unchanged.
    pub fn display_path(&self) -> String {
        if let Some(tail) = self.tilde_tail() {
            if tail.is_empty() {
                return "~/".to_string();
            }
            return format!("~/{}", tail);
        }
        if let Some(tail) = self.0.strip_prefix("https://") {
            if tail.starts_with("slug.social") {
                self.0.clone()
            } else {
                external_display_dash_prefix(tail)
            }
        } else if let Some(tail) = self.0.strip_prefix("http://") {
            if tail.starts_with("slug.social") {
                self.0.clone()
            } else {
                external_display_dash_prefix(tail)
            }
        } else {
            self.0.clone()
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

/// HTTP route capture: path segment after `~/` in `GET /~/*path` or `…/r/{short}{slug}/~/…` (empty = ontology root).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TildeHttpPathTail(pub String);

impl TildeHttpPathTail {
    pub fn new(path_segment: &str) -> Self {
        Self(path_segment.trim_start_matches('/').to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_canonical(&self) -> CanonicalItemUrl {
        tilde_http_path_to_canonical(self.as_str())
    }
}

/// Map the router's tilde tail (e.g. `topic/a`, or empty for root) to a [`CanonicalItemUrl`].
pub fn tilde_http_path_to_canonical(path_segment: &str) -> CanonicalItemUrl {
    let p = path_segment.trim_start_matches('/');
    let raw = if p.starts_with("http://") || p.starts_with("https://") {
        p.to_string()
    } else if p.is_empty() {
        "~/".to_string()
    } else {
        format!("~/{}", p)
    };
    CanonicalItemUrl::parse(&raw).unwrap_or_else(|| CanonicalItemUrl::ontology_root())
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
    let Some(room_seg) = room_route_segment(room) else {
        return api_path_or_url(item);
    };
    let Some(c) = CanonicalItemUrl::parse(item) else {
        return api_path_or_url(item);
    };
    let root = CanonicalItemUrl::ontology_root();
    let item_norm = c.as_str().trim_end_matches('/');
    let root_norm = root.as_str().trim_end_matches('/');
    if let Some(tail) = c.tilde_tail() {
        return if tail.is_empty() {
            format!("https://slug.social/r/{room_seg}/~")
        } else {
            format!("https://slug.social/r/{room_seg}/~/{}", tail)
        };
    }
    if item_norm == root_norm {
        return format!("https://slug.social/r/{room_seg}/~");
    }
    // External http(s) items use the same `/-/…` namespace under the room garden.
    if c.as_str().starts_with("https://") || c.as_str().starts_with("http://") {
        let tail = c.display_path();
        let tail = tail.strip_prefix("-/").unwrap_or(tail.as_str());
        return format!("https://slug.social/r/{room_seg}/-/{tail}");
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
        } else if let Some(room_seg) = room_route_segment(room) {
            format!("https://slug.social/r/{room_seg}/t/{tag}")
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
        Self(c.display_path())
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
        assert_eq!(root.as_str(), SLUG_TILDE_ONTOLOGY_ROOT);
        assert_eq!(root, CanonicalItemUrl::ontology_root());
    }

    #[test]
    fn tilde_ontology_root_unifies_forms() {
        assert_eq!(canonicalize_item("~/"), SLUG_TILDE_ONTOLOGY_ROOT);
        assert_eq!(
            normalize_slug_ontology_storage_url("https://slug.social/~/"),
            SLUG_TILDE_ONTOLOGY_ROOT.to_string()
        );
        assert_eq!(
            CanonicalItemUrl::parse("https://slug.social/~/")
                .unwrap()
                .as_str(),
            SLUG_TILDE_ONTOLOGY_ROOT
        );
        let legacy = CanonicalItemUrl("https://slug.social/~/".to_string());
        assert_eq!(legacy.normalized_storage().as_str(), SLUG_TILDE_ONTOLOGY_ROOT);
    }

    #[test]
    fn tilde_http_path_tail_maps_router_segment() {
        assert_eq!(
            TildeHttpPathTail::new("").to_canonical(),
            CanonicalItemUrl::ontology_root()
        );
        assert_eq!(
            tilde_http_path_to_canonical("topic/x").as_str(),
            "https://slug.social/~/topic/x"
        );
    }

    #[test]
    fn display_path_slug_ontology_root() {
        let r = CanonicalItemUrl::ontology_root();
        assert_eq!(r.display_path(), "~/");
        assert_eq!(r.tilde_tail(), Some(""));
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
            "https://slug.social/r/9ab12cdmy-room/~/topic/x"
        );
    }

    #[test]
    fn garden_private_room_ontology_root() {
        assert_eq!(
            GardenItemUrl::from_storage_str("https://slug.social/~", "9ab12cd/my-room").as_str(),
            "https://slug.social/r/9ab12cdmy-room/~"
        );
        assert_eq!(
            GardenItemUrl::from_storage_str("https://slug.social/~/", "9ab12cd/my-room").as_str(),
            "https://slug.social/r/9ab12cdmy-room/~"
        );
    }

    #[test]
    fn garden_external_url_room_scoped_under_dash_namespace() {
        let u = "https://example.com/z";
        assert_eq!(
            GardenItemUrl::from_storage_str(u, "9ab12cd/my-room").as_str(),
            "https://slug.social/r/9ab12cdmy-room/-/example.com/z"
        );
    }

    #[test]
    fn forum_web_public_vs_room() {
        assert_eq!(
            ForumThreadUrl::from_room_tag("public", "debate").as_str(),
            "https://slug.social/t/debate"
        );
        assert_eq!(
            ForumThreadUrl::from_room_tag("9ab12cd/my-room", "#debate").as_str(),
            "https://slug.social/r/9ab12cdmy-room/t/debate"
        );
    }

    #[test]
    fn canonicalize_youtube_short_links() {
        assert_eq!(
            canonicalize_item("https://youtu.be/dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
        assert_eq!(
            canonicalize_item("-/youtu.be/dQw4w9WgXcQ"),
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        );
    }

    #[test]
    fn canonicalize_item_dash_namespace_https() {
        assert_eq!(
            canonicalize_item("-/github.com/A/B"),
            "https://github.com/a/b"
        );
        assert_eq!(canonicalize_item("-/Example.COM"), "https://example.com");
        assert_eq!(
            canonicalize_item("-/example.com/foo/bar/"),
            "https://example.com/foo/bar"
        );
    }

    #[test]
    fn canonical_item_url_parent_external_strips_last_segment() {
        let c = CanonicalItemUrl::parse("https://spotify.com/track/1").unwrap();
        assert_eq!(
            c.parent().unwrap().as_str(),
            "https://spotify.com/track"
        );
        assert_eq!(
            CanonicalItemUrl::parse("https://github.com/iss/1")
                .unwrap()
                .parent()
                .unwrap()
                .as_str(),
            "https://github.com/iss"
        );
        assert!(CanonicalItemUrl::parse("https://github.com").unwrap().parent().is_none());
    }

    #[test]
    fn display_path_roundtrips_dash_and_tilde() {
        let ext = CanonicalItemUrl::parse("https://GitHub.com/org/Issue").unwrap();
        assert_eq!(ext.display_path(), "-/github.com/org/issue");
        let tilde = CanonicalItemUrl::parse("~/Rust/Doc").unwrap();
        assert_eq!(tilde.display_path(), "~/rust/doc");
    }

    #[test]
    fn item_parent_path_external() {
        assert_eq!(
            item_parent_path("-/github.com/org/repo/issues/1").as_deref(),
            Some("https://github.com/org/repo/issues")
        );
    }
}
