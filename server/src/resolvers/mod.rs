//! Domain resolvers (GitHub, …) and matching HTML renderers for imported item bodies.
//!
//! Resolver output is ingested as DSL; bodies may embed a `slug-github-card` fenced JSON
//! envelope that [`crate::html::render_item_body_in_scope`] renders instead of a raw `<pre>`.

pub mod github;
pub mod default_external;

pub use default_external::DefaultExternalResolver;
pub use github::{
    resolve_github_children, try_render_github_import_markup, ExternalResolver, GitHubResolver,
    GithubImportCard, GithubImportKind, ResolvedChild,
};

/// Extension point: add more `try_render_*` calls here as new resolvers ship.
pub fn try_render_resolver_item_body(raw: &str) -> Option<maud::Markup> {
    github::try_render_github_import_markup(raw)
}
