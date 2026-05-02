use async_trait::async_trait;

use crate::path_types::ItemId;

#[async_trait]
pub trait ExternalResolver: Send + Sync {
    /// e.g. `"github.com"`
    fn domain_match(&self) -> &'static str;

    /// Normalizes URLs (e.g. stripping fragments); extend per-domain later.
    fn normalize(&self, path: &str) -> String;

    /// Fetches body when missing; GitHub hook lands here in a follow-up.
    async fn fetch_body(&self, item: &ItemId) -> Result<String, String>;
}

/// Placeholder until domain-specific resolvers exist.
pub struct DefaultExternalResolver;

#[async_trait]
impl ExternalResolver for DefaultExternalResolver {
    fn domain_match(&self) -> &'static str {
        ""
    }

    fn normalize(&self, path: &str) -> String {
        path.to_string()
    }

    async fn fetch_body(&self, _item: &ItemId) -> Result<String, String> {
        Err("external fetch not implemented".to_string())
    }
}
