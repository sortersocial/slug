use async_trait::async_trait;

use super::github::ExternalResolver;
use crate::path_types::ItemId;

/// Placeholder until other domain-specific resolvers exist.
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
