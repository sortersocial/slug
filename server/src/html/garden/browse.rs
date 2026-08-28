use maud::html;

use crate::{
    html::{
        bc_path_external, bc_segment,
        breadcrumb_path::{ExternalOntologyPath, OntologyPath},
        forum::ThreadNav,
    },
    path_types::ItemId,
    reducer::ScopeId,
};

pub(super) fn garden_layout_meta(nav: &ThreadNav) -> (String, String) {
    (nav.room_wire.clone(), nav.garden_root_url().to_string())
}

pub(super) fn scoped_bc_path_external(
    path: &ExternalOntologyPath,
    nav: &ThreadNav,
) -> maud::Markup {
    match nav.scope() {
        ScopeId::Public => bc_path_external(path),
        ScopeId::Room(rid) => {
            let slug = rid.split_once('/').map(|(_, s)| s).unwrap_or(rid.as_str());
            let ext_root = format!("{}-", nav.garden_root_url().trim_end_matches('~'));
            html! {
                a href="/" { "slug.social" }
                (bc_segment(&format!("room:{slug}"), nav.room_url(), false))
                (bc_segment("-", &ext_root, path.is_root()))
                @for (i, id) in path.breadcrumb_chain().iter().enumerate() {
                    @let disp = id.display_path();
                    @let tail = disp.strip_prefix("-/").unwrap_or(disp.as_str());
                    @let href = format!("{}/{}", ext_root, tail);
                    @let is_last = i + 1 == path.breadcrumb_chain().len();
                    (bc_segment(id.last_segment(), &href, is_last))
                }
            }
        }
    }
}

pub(super) enum GardenBrowsePath {
    Tilde(OntologyPath),
    External(ExternalOntologyPath),
}

impl GardenBrowsePath {
    pub(super) fn item(&self) -> &str {
        match self {
            GardenBrowsePath::Tilde(p) => p.as_str(),
            GardenBrowsePath::External(p) => p.as_str(),
        }
    }

    pub(super) fn is_external(&self) -> bool {
        matches!(self, GardenBrowsePath::External(_))
    }
}

pub(super) fn scoped_bc_path_for(
    path: &GardenBrowsePath,
    nav: &ThreadNav,
    crumbs: &[ItemId],
) -> maud::Markup {
    match path {
        GardenBrowsePath::Tilde(p) => scoped_bc_containment(p, crumbs, nav),
        GardenBrowsePath::External(p) => scoped_bc_path_external(p, nav),
    }
}

/// Primary crumb trail from containment (strongest-parent walk), leaf hrefs.
pub(super) fn scoped_bc_containment(
    path: &OntologyPath,
    crumbs: &[ItemId],
    nav: &ThreadNav,
) -> maud::Markup {
    let garden = nav.garden_root_url();
    let segments = html! {
        @for (i, id) in crumbs.iter().enumerate() {
            @let leaf = id.ontology_leaf();
            @let href = match leaf.tilde_tail() {
                Some("") => garden.to_string(),
                Some(tail) => format!("{garden}/{tail}"),
                None => format!("{garden}/{}", leaf.last_segment()),
            };
            @let is_last = i + 1 == crumbs.len();
            (bc_segment(leaf.last_segment(), &href, is_last))
        }
    };
    match nav.scope() {
        ScopeId::Public => html! {
            a href=(path.slug_root_href()) { "slug.social" }
            (bc_segment("~", "/~", path.is_root()))
            (segments)
        },
        ScopeId::Room(rid) => {
            let slug = rid.split_once('/').map(|(_, s)| s).unwrap_or(rid.as_str());
            html! {
                a href="/" { "slug.social" }
                (bc_segment(&format!("room:{slug}"), nav.room_url(), false))
                (bc_segment("~", nav.garden_root_url(), path.is_root()))
                (segments)
            }
        }
    }
}
