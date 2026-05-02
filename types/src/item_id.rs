//! Structural item identity for the reducer graph and ranking (vs presentation-only strings).

use std::cmp::Ordering;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::item_wire::{
    canonicalize_item, external_display_dash_prefix, normalize_slug_ontology_storage_url,
    SLUG_TILDE_ONTOLOGY_ROOT,
};

/// Structural key for items in [`slug_types`] and the server reducer.
///
/// Wire / JSON uses the same single string as the former canonical item URL (via serde).
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum ItemId {
    /// Tilde ontology root (`~/`); storage [`SLUG_TILDE_ONTOLOGY_ROOT`].
    Root,
    /// `https://slug.social/~/…` (non-root; normalized trailing path).
    Local(String),
    /// Normalized `http(s)://…` storage form, including non-`~/` paths on `slug.social`.
    Web(String),
    /// Raw key material that did not round-trip through [`Self::parse`] (historical edge case).
    Opaque(String),
}

impl ItemId {
    pub fn parse(input: &str) -> Option<Self> {
        let c = normalize_slug_ontology_storage_url(&canonicalize_item(input));
        if c.is_empty() {
            return None;
        }
        if c == SLUG_TILDE_ONTOLOGY_ROOT {
            return Some(Self::Root);
        }
        if c.starts_with("https://slug.social/~/") && c != SLUG_TILDE_ONTOLOGY_ROOT {
            return Some(Self::Local(c));
        }
        Some(Self::Web(c))
    }

    /// Same as the old `ensure_item` fallback: use `s` verbatim as the map key.
    pub fn opaque(raw: String) -> Self {
        Self::Opaque(raw)
    }

    pub fn ontology_root() -> Self {
        Self::Root
    }

    /// Collapses legacy slug ontology root spellings so [`std::collections::HashMap`] keys match the graph.
    pub fn normalized_storage(self) -> Self {
        Self::parse(self.as_str()).unwrap_or(self)
    }

    pub fn as_str(&self) -> &str {
        match self {
            ItemId::Root => SLUG_TILDE_ONTOLOGY_ROOT,
            ItemId::Local(s) | ItemId::Web(s) | ItemId::Opaque(s) => s,
        }
    }

    pub fn to_storage_string(&self) -> String {
        self.as_str().to_string()
    }

    pub fn tilde_tail(&self) -> Option<&str> {
        match self {
            ItemId::Root => Some(""),
            ItemId::Local(s) => s.strip_prefix("https://slug.social/~/"),
            ItemId::Web(s) | ItemId::Opaque(s) => {
                if let Some(tail) = s.strip_prefix("https://slug.social/~/") {
                    return Some(tail);
                }
                if s == SLUG_TILDE_ONTOLOGY_ROOT || s == "https://slug.social/~/" {
                    return Some("");
                }
                None
            }
        }
    }

    /// HTTP garden tail after `~/` (empty at ontology root), or `None` if not under tilde ontology.
    pub fn tilde_http_tail(&self) -> Option<String> {
        self.tilde_tail().map(str::to_owned)
    }

    pub fn last_segment(&self) -> &str {
        let s = self.as_str();
        s.rsplit('/').find(|x| !x.is_empty()).unwrap_or(s)
    }

    pub fn parent(&self) -> Option<Self> {
        match self {
            ItemId::Root => None,
            ItemId::Local(s) => {
                if s == SLUG_TILDE_ONTOLOGY_ROOT || s == "https://slug.social/~/" {
                    return None;
                }
                let last_slash = s.rfind('/')?;
                let parent_str = &s[..last_slash];
                if parent_str.is_empty() {
                    None
                } else {
                    Self::parse(parent_str)
                }
            }
            ItemId::Web(s) | ItemId::Opaque(s) => {
                if s == SLUG_TILDE_ONTOLOGY_ROOT || s == "https://slug.social/~/" {
                    return None;
                }
                if let Some(rest) = s.strip_prefix("https://slug.social/~/") {
                    if rest.is_empty() {
                        return None;
                    }
                    let last_slash = s.rfind('/')?;
                    let parent_str = &s[..last_slash];
                    Self::parse(parent_str)
                } else if s.starts_with("https://") {
                    let rest = s.strip_prefix("https://").unwrap();
                    Self::parent_http_url("https://", rest)
                } else if s.starts_with("http://") {
                    let rest = s.strip_prefix("http://").unwrap();
                    Self::parent_http_url("http://", rest)
                } else {
                    None
                }
            }
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
            Self::parse(&format!("{scheme}{}", host))
        } else {
            Self::parse(&format!("{scheme}{}/{}", host, parent_path))
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
        let s = self.as_str();
        if let Some(tail) = s.strip_prefix("https://") {
            if tail.starts_with("slug.social") {
                return s.to_string();
            }
            return external_display_dash_prefix(tail);
        }
        if let Some(tail) = s.strip_prefix("http://") {
            if tail.starts_with("slug.social") {
                return s.to_string();
            }
            return external_display_dash_prefix(tail);
        }
        s.to_string()
    }

    pub fn tilde_segments(&self) -> Vec<&str> {
        match self.tilde_tail() {
            Some(tail) if !tail.is_empty() => std::iter::once("~")
                .chain(tail.split('/').filter(|s| !s.is_empty()))
                .collect(),
            Some(_) => vec!["~"],
            None => vec![],
        }
    }

    /// Normalized URL string for HTTP fetch boundaries (external identities).
    pub fn to_wire_url(&self) -> String {
        self.to_storage_string()
    }
}

impl PartialOrd for ItemId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ItemId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

impl Serialize for ItemId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.as_str().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ItemId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        ItemId::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid item id: {s:?}"))
        })
    }
}
