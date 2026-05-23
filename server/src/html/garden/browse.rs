use maud::html;

use crate::{
    html::{
        breadcrumb_path::{ExternalOntologyPath, OntologyPath},
        forum::ThreadNav,
        bc_path, bc_path_external, bc_segment,
    },
    reducer::ScopeId,
};

pub(super) fn garden_layout_meta(nav: &ThreadNav) -> (String, String) {
    (nav.room_wire.clone(), nav.garden_root_url().to_string())
}

pub(super) fn scoped_bc_path_external(path: &ExternalOntologyPath, nav: &ThreadNav) -> maud::Markup {
    match nav.scope() {
        ScopeId::Public => bc_path_external(path),
        ScopeId::Room(rid) => {
            let slug = rid.split_once('/').map(|(_, s)| s).unwrap_or(rid.as_str());
            let ext_root = format!("{}-", nav.garden_root_url().trim_end_matches('~'));
            html! {
                a href="/" { "slug.social" }
                (bc_segment(&format!("room:{slug}"), nav.room_url(), false))
                (bc_segment("-", &ext_root, path.is_root()))
                @for (i, seg) in path.segments().iter().enumerate() {
                    @let href = format!("{}/{}", ext_root, path.segments()[..=i].join("/"));
                    @let is_last = i == path.segments().len() - 1;
                    (bc_segment(seg, &href, is_last))
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

pub(super) fn scoped_bc_path_for(path: &GardenBrowsePath, nav: &ThreadNav) -> maud::Markup {
    match path {
        GardenBrowsePath::Tilde(p) => scoped_bc_path(p, nav),
        GardenBrowsePath::External(p) => scoped_bc_path_external(p, nav),
    }
}

pub(super) fn scoped_bc_path(path: &OntologyPath, nav: &ThreadNav) -> maud::Markup {
    match nav.scope() {
        ScopeId::Public => bc_path(path),
        ScopeId::Room(rid) => {
            let slug = rid.split_once('/').map(|(_, s)| s).unwrap_or(rid.as_str());
            html! {
                a href="/" { "slug.social" }
                (bc_segment(&format!("room:{slug}"), nav.room_url(), false))
                (bc_segment("~", nav.garden_root_url(), path.is_root()))
                @for (i, seg) in path.segments().iter().enumerate() {
                    @let href = format!("{}/{}", nav.garden_root_url(), path.segments()[..=i].join("/"));
                    @let is_last = i == path.segments().len() - 1;
                    (bc_segment(seg, &href, is_last))
                }
            }
        }
    }
}
