//! Canonical paths, storage ids, and JSON href newtypes. All normalization and
//! room-aware URL rules for items live here.
//!
//! ## String kinds (parse in this module only)
//!
//! - **[`canonicalize_item`] / [`crate::ItemId`]** — graph storage key and DSL form; tilde
//!   ontology root is always [`SLUG_TILDE_ONTOLOGY_ROOT`] (no `…/~/` trailing slash only).
//! - **[`TildeHttpPathTail`]** — capture from `GET /~/*path` or `…/r/{short}{slug}/~/…` (the `*path` segment).
//! - **`-/…` wire form** — external items; see [`canonicalize_item`] dash branch.
//! - **[`GardenItemUrl`], [`ForumThreadUrl`]** — JSON / browser href surfaces.
//! - **[`ROOM_SHORT_ID_LEN`] / [`room_route_segment`]** — `/r/{short}{slug}` vs wire `short/slug`.

use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

use crate::item_id::ItemId;
pub use crate::item_wire::{
    canonicalize_item, item_parent_path, item_path_segments, normalize_slug_ontology_storage_url,
    SLUG_TILDE_ONTOLOGY_ROOT,
};

// ---------------------------------------------------------------------------
// Private room HTTP path (`/r/{short}{slug}`; wire id remains `short/slug`)
// ---------------------------------------------------------------------------

/// Byte length of the random `short` segment in `short/slug` room ids (matches server `gen_short_id`).
pub const ROOM_SHORT_ID_LEN: usize = 7;

/// `ab12cde/my-room` → `ab12cdemy-room` for a single `/r/…` path segment.
pub fn room_route_segment(room_id: &str) -> Option<String> {
    let (short, slug) = room_id.split_once('/')?;
    if short.len() != ROOM_SHORT_ID_LEN || short.is_empty() || slug.is_empty() {
        return None;
    }
    if !short
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'z'))
    {
        return None;
    }
    Some(format!("{short}{slug}"))
}

/// `/r/{short}{slug}` path segment → `short/slug` wire id (inverse of [`room_route_segment`]).
pub fn room_id_from_route_segment(seg: &str) -> Option<String> {
    if seg.len() <= ROOM_SHORT_ID_LEN {
        return None;
    }
    let (short, slug) = seg.split_at(ROOM_SHORT_ID_LEN);
    if short.is_empty() || slug.is_empty() {
        return None;
    }
    if !short
        .bytes()
        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'z'))
    {
        return None;
    }
    Some(format!("{short}/{slug}"))
}

// ---------------------------------------------------------------------------
// Normalization (moved from server `canonical_path`)
// ---------------------------------------------------------------------------

/// Thread / public tag: stored without leading `#`, lowercase.
pub fn canonicalize_tag(input: &str) -> String {
    input.trim().trim_start_matches('#').to_lowercase()
}

// ---------------------------------------------------------------------------
// Storage + input path newtypes
// ---------------------------------------------------------------------------

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

    pub fn to_item_id(&self) -> ItemId {
        tilde_http_path_to_item_id(self.as_str())
    }
}

/// Map the router's tilde tail (e.g. `topic/a`, or empty for root) to an [`ItemId`].
pub fn tilde_http_path_to_item_id(path_segment: &str) -> ItemId {
    let p = path_segment.trim_start_matches('/');
    let raw = if p.starts_with("http://") || p.starts_with("https://") {
        p.to_string()
    } else if p.is_empty() {
        "~/".to_string()
    } else {
        format!("~/{}", p)
    };
    ItemId::parse(&raw).unwrap_or_else(ItemId::ontology_root)
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

    pub fn canonicalize(&self) -> Option<ItemId> {
        ItemId::parse(&self.0)
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

    pub fn join_under_ontology_root(&self, root: &ItemId) -> Option<ItemId> {
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
        ItemId::parse(&joined)
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

    /// Stored item id + RPC `room` field (`"public"` or `"short/slug"`).
    pub fn from_stored(stored: &ItemId, room_wire: &str) -> Self {
        Self(garden_href_string(stored, room_wire))
    }

    /// Like [`Self::from_stored`] but accepts a string that may already be canonical.
    pub fn from_storage_str(stored: &str, room_wire: &str) -> Self {
        let Some(id) = ItemId::parse(stored) else {
            return Self(api_path_or_url(stored));
        };
        Self(garden_href_string(&id, room_wire))
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

fn garden_href_string(c: &ItemId, room_wire: &str) -> String {
    let room = room_wire.trim();
    if room.is_empty() || room == "public" {
        return api_path_or_url(c.as_str());
    }
    let Some(room_seg) = room_route_segment(room) else {
        return api_path_or_url(c.as_str());
    };
    let root = ItemId::ontology_root();
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
    api_path_or_url(c.as_str())
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
    pub fn from_stored(c: &ItemId) -> Self {
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
    fn item_parent_deep() {
        let c = ItemId::parse("~/a/b/c").unwrap();
        assert_eq!(c.parent().unwrap().as_str(), "https://slug.social/~/a/b");
    }

    #[test]
    fn item_parent_one_level() {
        let c = ItemId::parse("~/a").unwrap();
        assert_eq!(c.parent().unwrap().as_str(), "https://slug.social/~");
    }

    #[test]
    fn item_parent_root_is_none() {
        let root = ItemId::parse("~/").unwrap();
        assert!(root.parent().is_none());
        assert_eq!(root.as_str(), SLUG_TILDE_ONTOLOGY_ROOT);
        assert_eq!(root, ItemId::ontology_root());
    }

    #[test]
    fn tilde_ontology_root_unifies_forms() {
        assert_eq!(canonicalize_item("~/"), SLUG_TILDE_ONTOLOGY_ROOT);
        assert_eq!(
            normalize_slug_ontology_storage_url("https://slug.social/~/"),
            SLUG_TILDE_ONTOLOGY_ROOT.to_string()
        );
        assert_eq!(
            ItemId::parse("https://slug.social/~/")
                .unwrap()
                .as_str(),
            SLUG_TILDE_ONTOLOGY_ROOT
        );
        let legacy = ItemId::opaque("https://slug.social/~/".to_string());
        assert_eq!(legacy.normalized_storage().as_str(), SLUG_TILDE_ONTOLOGY_ROOT);
    }

    #[test]
    fn tilde_http_path_tail_maps_router_segment() {
        assert_eq!(
            TildeHttpPathTail::new("").to_item_id(),
            ItemId::ontology_root()
        );
        assert_eq!(
            tilde_http_path_to_item_id("topic/x").as_str(),
            "https://slug.social/~/topic/x"
        );
    }

    #[test]
    fn display_path_slug_ontology_root() {
        let r = ItemId::ontology_root();
        assert_eq!(r.display_path(), "~/");
        assert_eq!(r.tilde_tail(), Some(""));
    }

    #[test]
    fn tilde_segments_deep() {
        let c = ItemId::parse("~/a/b").unwrap();
        assert_eq!(c.tilde_segments(), vec!["~", "a", "b"]);
    }

    #[test]
    fn tilde_segments_root() {
        let c = ItemId::parse("~/").unwrap();
        assert_eq!(c.tilde_segments(), vec!["~"]);
    }

    #[test]
    fn tilde_segments_non_ontology_is_empty() {
        let c = ItemId::parse("https://example.com/foo").unwrap();
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
    fn canonicalize_external_url_strips_fragment_and_tracking_params() {
        assert_eq!(
            canonicalize_item("https://example.com/path?utm_source=newsletter&b=2#section"),
            "https://example.com/path?b=2"
        );
        assert_eq!(
            canonicalize_item("-/example.com/path?FbClId=abc&a=1"),
            "https://example.com/path?a=1"
        );
    }

    #[test]
    fn canonicalize_github_external_url_normalizes_repo_identity() {
        assert_eq!(
            canonicalize_item("https://github.com/ORG/REPO.git?tab=issues&q=is%3Aopen"),
            "https://github.com/org/repo"
        );
    }

    #[test]
    fn item_id_parent_external_strips_last_segment() {
        let c = ItemId::parse("https://spotify.com/track/1").unwrap();
        assert_eq!(
            c.parent().unwrap().as_str(),
            "https://spotify.com/track"
        );
        assert_eq!(
            ItemId::parse("https://github.com/iss/1")
                .unwrap()
                .parent()
                .unwrap()
                .as_str(),
            "https://github.com/iss"
        );
        assert!(ItemId::parse("https://github.com").unwrap().parent().is_none());
    }

    #[test]
    fn display_path_roundtrips_dash_and_tilde() {
        let ext = ItemId::parse("https://GitHub.com/org/Issue").unwrap();
        assert_eq!(ext.display_path(), "-/github.com/org/issue");
        let tilde = ItemId::parse("~/Rust/Doc").unwrap();
        assert_eq!(tilde.display_path(), "~/rust/doc");
    }

    #[test]
    fn item_parent_path_external() {
        assert_eq!(
            item_parent_path("-/github.com/org/repo/issues/1").as_deref(),
            Some("https://github.com/org/repo/issues")
        );
    }

    #[test]
    fn round_trip_room_segment() {
        let id = "9ab12cd/my-room";
        let seg = room_route_segment(id).unwrap();
        assert_eq!(seg, "9ab12cdmy-room");
        assert_eq!(room_id_from_route_segment(&seg).as_deref(), Some(id));
    }

    #[test]
    fn too_short_room_route_segment_rejected() {
        assert!(room_id_from_route_segment("9ab12cd").is_none());
    }
}
