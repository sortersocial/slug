use crate::canonical_path::canonicalize_item;
use crate::reducer::ScopeId;
use slug_types::room_route_segment;

/// Posts per page on `/t/:tag` (and room thread) list views.
pub(crate) const THREAD_PAGE_SIZE: usize = 10;

/// DOM `id` / URL fragment for a chronological post on a thread page (`#post-N`).
pub(crate) fn post_fragment_id(post_idx: usize) -> String {
    format!("post-{post_idx}")
}

/// URL helpers for public `/t/…` and private room threads `/r/{short}{slug}/t/…`.
#[derive(Clone)]
pub struct ThreadNav {
    pub room_wire: String,
    scope: ScopeId,
    room_path: String,
    thread_path_prefix: String,
    garden_path_prefix: String,
}

impl ThreadNav {
    pub(crate) fn public() -> Self {
        Self {
            room_wire: "public".into(),
            scope: ScopeId::Public,
            room_path: "/".into(),
            thread_path_prefix: "/t".into(),
            garden_path_prefix: "/~".into(),
        }
    }

    /// `room_id` wire form `shortid/slug` (HTTP uses [`slug_types::room_route_segment`]).
    pub(crate) fn from_room_id(room_id: &str) -> Option<Self> {
        let room_seg = room_route_segment(room_id)?;
        Some(Self {
            room_wire: room_id.to_string(),
            scope: ScopeId::Room(room_id.to_string()),
            room_path: format!("/r/{room_seg}"),
            thread_path_prefix: format!("/r/{room_seg}/t"),
            garden_path_prefix: format!("/r/{room_seg}/~"),
        })
    }

    pub(crate) fn scope(&self) -> ScopeId {
        self.scope.clone()
    }

    pub(crate) fn room_url(&self) -> &str {
        &self.room_path
    }

    pub(crate) fn thread_url(&self, tag: &str) -> String {
        format!("{}/{}", self.thread_path_prefix, tag)
    }

    pub(crate) fn garden_root_url(&self) -> &str {
        &self.garden_path_prefix
    }

    pub(crate) fn garden_item_url(&self, item: &str) -> String {
        let Some(c) = crate::path_types::ItemId::parse(item) else {
            return format!("{}/{}", self.garden_path_prefix, canonicalize_item(item));
        };
        self.garden_item_href(&c)
    }

    /// Relative href for a structured [`crate::path_types::ItemId`] in this scope’s garden.
    /// Tilde items use the leaf (`~/x/luke` → `/~/luke`).
    pub(crate) fn garden_item_href(&self, c: &crate::path_types::ItemId) -> String {
        let c = c.ontology_leaf();
        if let Some(tail) = c.tilde_tail().map(str::to_owned) {
            format!("{}/{}", self.garden_path_prefix, tail)
        } else if c.as_str().starts_with("http://") || c.as_str().starts_with("https://") {
            let disp = c.display_path();
            let rest = disp.strip_prefix("-/").unwrap_or(disp.as_str());
            let ext_prefix = format!("{}-", self.garden_path_prefix.trim_end_matches('~'));
            format!("{ext_prefix}/{rest}")
        } else {
            format!(
                "{}/{}",
                self.garden_path_prefix,
                canonicalize_item(c.as_str())
            )
        }
    }

    pub(crate) fn thread_page_url(&self, tag: &str, offset: usize) -> String {
        let base = self.thread_url(tag);
        if offset == 0 {
            base
        } else {
            format!("{base}?offset={offset}")
        }
    }

    /// Thread list URL for the page that contains `post_idx`, with `#post-N` to scroll to it.
    pub(crate) fn thread_url_for_post(&self, tag: &str, post_idx: usize) -> String {
        let offset = (post_idx / THREAD_PAGE_SIZE) * THREAD_PAGE_SIZE;
        format!(
            "{}#{}",
            self.thread_page_url(tag, offset),
            post_fragment_id(post_idx)
        )
    }

    pub(crate) fn post_url(&self, tag: &str, idx: usize) -> String {
        format!("{}/{}/{}", self.thread_path_prefix, tag, idx)
    }

    /// Empty for public; `/r/:seg` for room — prefix for routes like `/vote`.
    pub(crate) fn room_path_prefix_for_vote_compare(&self) -> String {
        match &self.scope {
            ScopeId::Public => String::new(),
            ScopeId::Room(room_id) => {
                let seg = room_route_segment(room_id).expect("room seg");
            format!("/r/{seg}")
        }
    }
}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_url_for_post_uses_page_offset_and_fragment() {
        let nav = ThreadNav::public();
        assert_eq!(
            nav.thread_url_for_post("hist-test", 0),
            "/t/hist-test#post-0"
        );
        assert_eq!(
            nav.thread_url_for_post("hist-test", 9),
            "/t/hist-test#post-9"
        );
        assert_eq!(
            nav.thread_url_for_post("hist-test", 10),
            "/t/hist-test?offset=10#post-10"
        );
        assert_eq!(
            nav.thread_url_for_post("hist-test", 25),
            "/t/hist-test?offset=20#post-25"
        );
    }

    #[test]
    fn room_thread_url_for_post_keeps_room_prefix() {
        let nav = ThreadNav::from_room_id("abcd123/demo").unwrap();
        assert_eq!(
            nav.thread_url_for_post("topic", 12),
            "/r/abcd123demo/t/topic?offset=10#post-12"
        );
        assert_eq!(nav.post_url("topic", 12), "/r/abcd123demo/t/topic/12");
    }

}
