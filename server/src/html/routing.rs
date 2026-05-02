//! Scoped browser paths for HTML. **RouteContext** is the intended single place to build `href`s
//! given public vs room scope (the original blueprint name); today it wraps [`ThreadNav`].
//!
//! Prefer `RouteContext::item_href` / [`RouteContext::thread_url`] in new Maud over stitching
//! `/r/…` vs `/~` manually. Call sites can migrate incrementally from passing `&ThreadNav`.

use crate::path_types::ItemId;

use super::forum::ThreadNav;

#[derive(Clone)]
pub struct RouteContext(ThreadNav);

impl RouteContext {
    #[inline]
    pub fn public() -> Self {
        Self(ThreadNav::public())
    }

    #[inline]
    pub fn from_room_id(room_id: &str) -> Option<Self> {
        ThreadNav::from_room_id(room_id).map(Self)
    }

    #[inline]
    pub fn thread_nav(&self) -> &ThreadNav {
        &self.0
    }

    #[inline]
    pub fn into_thread_nav(self) -> ThreadNav {
        self.0
    }

    /// Relative path for a stored item in this scope’s garden.
    pub fn item_href(&self, item: &ItemId) -> String {
        self.0.garden_item_href(item)
    }

    /// Same as [`Self::item_href`] but parses `item` first (raw DSL / user paste).
    pub fn item_href_raw(&self, item: &str) -> String {
        self.0.garden_item_url(item)
    }

    #[inline]
    pub fn thread_url(&self, tag: &str) -> String {
        self.0.thread_url(tag)
    }

    #[inline]
    pub fn garden_root_url(&self) -> &str {
        self.0.garden_root_url()
    }

    #[inline]
    pub fn room_url(&self) -> &str {
        self.0.room_url()
    }
}

impl From<ThreadNav> for RouteContext {
    fn from(nav: ThreadNav) -> Self {
        Self(nav)
    }
}

impl From<RouteContext> for ThreadNav {
    fn from(ctx: RouteContext) -> Self {
        ctx.0
    }
}
