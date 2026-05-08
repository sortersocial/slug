//! Pluggable per-host resolvers for external (`https://…`) garden items.

use std::sync::Arc;

use async_trait::async_trait;

use crate::github_resolver::GitHubResolver;
use crate::path_types::ItemId;

/// One child discovered under a parent external URL.
#[derive(Debug, Clone)]
pub struct ResolvedChild {
    pub url: String,
    pub title: String,
    pub body: Option<String>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ResolverError {
    #[error("{0}")]
    Msg(String),
}

#[async_trait]
pub trait DomainResolver: Send + Sync {
    fn matches_host(&self, host: &str) -> bool;

    async fn list_children(&self, parent: &ItemId) -> Result<Vec<ResolvedChild>, ResolverError>;

    async fn fetch_body(&self, item: &ItemId) -> Result<String, ResolverError>;
}

/// Stub resolver: no automatic children or bodies.
pub struct DefaultDomainResolver;

#[async_trait]
impl DomainResolver for DefaultDomainResolver {
    fn matches_host(&self, _host: &str) -> bool {
        false
    }

    async fn list_children(&self, _parent: &ItemId) -> Result<Vec<ResolvedChild>, ResolverError> {
        Ok(vec![])
    }

    async fn fetch_body(&self, _item: &ItemId) -> Result<String, ResolverError> {
        Err(ResolverError::Msg("no domain resolver for this URL".into()))
    }
}

/// Resolver chain: GitHub first, then implicit default (empty lists).
pub struct ResolverRegistry {
    pub github: Arc<GitHubResolver>,
    default_res: Arc<DefaultDomainResolver>,
}

impl ResolverRegistry {
    pub fn from_env() -> Self {
        let token = std::env::var("SLUG_GITHUB_TOKEN").ok().filter(|s| !s.trim().is_empty());
        Self {
            github: Arc::new(GitHubResolver::new(token)),
            default_res: Arc::new(DefaultDomainResolver),
        }
    }

    pub fn for_host(&self, host: &str) -> &dyn DomainResolver {
        if self.github.matches_host(host) {
            self.github.as_ref()
        } else {
            self.default_res.as_ref()
        }
    }

    /// Hosts where [`Self::for_host`] may perform network I/O beyond a fast no-op.
    pub fn has_automatic_listing(&self, host: &str) -> bool {
        self.github.matches_host(host)
    }
}
