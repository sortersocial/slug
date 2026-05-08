//! GitHub REST resolver: org repos, repo facets, issues/PR listings and bodies.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use crate::domain_resolver::{DomainResolver, ResolvedChild, ResolverError};
use crate::path_types::ItemId;
use slug_types::normalize_http_identity_url;

const LIST_CACHE_TTL: Duration = Duration::from_secs(90);

pub struct GitHubResolver {
    client: reqwest::Client,
    token: Option<String>,
    list_cache: Mutex<HashMap<String, (Instant, Vec<ResolvedChild>)>>,
}

impl GitHubResolver {
    pub fn new(token: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("slugsocial-server/resolver (https://github.com/sortersocial/slug)")
                .build()
                .expect("reqwest client"),
            token,
            list_cache: Mutex::new(HashMap::new()),
        }
    }

    fn github_api_headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::ACCEPT,
            "application/vnd.github+json".parse().unwrap(),
        );
        if let Some(ref t) = self.token {
            let v = format!("Bearer {t}").parse().unwrap();
            h.insert(reqwest::header::AUTHORIZATION, v);
        }
        h
    }

    async fn get_json(&self, url: &str) -> Result<reqwest::Response, ResolverError> {
        let req = self
            .client
            .get(url)
            .headers(self.github_api_headers());
        let resp = req
            .send()
            .await
            .map_err(|e| ResolverError::Msg(e.to_string()))?;
        Ok(resp)
    }

    fn normalize_github_item_url(s: &str) -> String {
        let Ok(mut u) = Url::parse(s) else {
            return s.to_string();
        };
        if !u
            .host_str()
            .is_some_and(|h| h.eq_ignore_ascii_case("github.com"))
        {
            return normalize_http_identity_url(s).unwrap_or_else(|| s.to_string());
        }
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .into_owned()
            .filter(|(k, _)| {
                let kl = k.to_ascii_lowercase();
                kl != "tab" && kl != "q"
            })
            .collect();
        u.set_query(None);
        if !pairs.is_empty() {
            let mut qm = u.query_pairs_mut();
            for (k, v) in pairs {
                qm.append_pair(&k, &v);
            }
        }
        let segs: Vec<String> = u
            .path_segments()
            .map(|p| p.map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap_or_default();
        if !segs.is_empty() {
            let mut segs = segs;
            if let Some(last) = segs.last_mut() {
                if let Some(stripped) = last.strip_suffix(".git") {
                    *last = stripped.to_string();
                }
            }
            let mut path = format!("/{}", segs.join("/"));
            while path.len() > 1 && path.ends_with('/') {
                path.pop();
            }
            u.set_path(&path);
        }
        let s = u.to_string();
        normalize_http_identity_url(&s).unwrap_or(s)
    }

    fn parse_github_path(item: &ItemId) -> Option<Vec<String>> {
        let s = Self::normalize_github_item_url(item.as_str());
        let u = Url::parse(&s).ok()?;
        if !u.scheme().eq_ignore_ascii_case("https") {
            return None;
        }
        if !u.host_str()?.eq_ignore_ascii_case("github.com") {
            return None;
        }
        let segs: Vec<String> = u
            .path_segments()
            .map(|p| {
                p.map(|seg| {
                    seg.strip_suffix(".git")
                        .unwrap_or(seg)
                        .to_ascii_lowercase()
                })
                .collect()
            })
            .unwrap_or_default();
        if segs.is_empty() {
            None
        } else {
            Some(segs)
        }
    }

    async fn list_org_repos(&self, org: &str) -> Result<Vec<ResolvedChild>, ResolverError> {
        let url = format!("https://api.github.com/orgs/{org}/repos?per_page=100&type=all");
        let resp = self.get_json(&url).await?;
        if resp.status() == 404 {
            return self.list_user_repos(org).await;
        }
        if !resp.status().is_success() {
            return Err(ResolverError::Msg(format!(
                "GitHub org repos: HTTP {}",
                resp.status()
            )));
        }
        let v: Value = resp.json().await.map_err(|e| ResolverError::Msg(e.to_string()))?;
        let arr = v.as_array().ok_or_else(|| ResolverError::Msg("repos: not array".into()))?;
        let mut out = Vec::new();
        for ent in arr {
            let name = ent
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("?");
            let html_url = ent
                .get("html_url")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let desc = ent
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let title = format!("{name}");
            let url = if !html_url.is_empty() {
                Self::normalize_github_item_url(html_url)
            } else {
                Self::normalize_github_item_url(&format!("https://github.com/{org}/{name}"))
            };
            let body = if desc.trim().is_empty() {
                None
            } else {
                Some(desc.to_string())
            };
            out.push(ResolvedChild {
                url,
                title,
                body,
            });
        }
        Ok(out)
    }

    async fn list_user_repos(&self, user: &str) -> Result<Vec<ResolvedChild>, ResolverError> {
        let url = format!("https://api.github.com/users/{user}/repos?per_page=100&type=all");
        let resp = self.get_json(&url).await?;
        if !resp.status().is_success() {
            return Err(ResolverError::Msg(format!(
                "GitHub user repos: HTTP {}",
                resp.status()
            )));
        }
        let v: Value = resp.json().await.map_err(|e| ResolverError::Msg(e.to_string()))?;
        let arr = v.as_array().ok_or_else(|| ResolverError::Msg("repos: not array".into()))?;
        let mut out = Vec::new();
        for ent in arr {
            let name = ent
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("?");
            let html_url = ent
                .get("html_url")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let desc = ent
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let title = format!("{name}");
            let url = if !html_url.is_empty() {
                Self::normalize_github_item_url(html_url)
            } else {
                Self::normalize_github_item_url(&format!("https://github.com/{user}/{name}"))
            };
            let body = if desc.trim().is_empty() {
                None
            } else {
                Some(desc.to_string())
            };
            out.push(ResolvedChild {
                url,
                title,
                body,
            });
        }
        Ok(out)
    }

    fn structural_repo_children(&self, org: &str, repo: &str) -> Vec<ResolvedChild> {
        [
            ("issues", "Issues"),
            ("pulls", "Pull requests"),
            ("commits", "Commits"),
            ("releases", "Releases"),
            ("wiki", "Wiki"),
        ]
        .into_iter()
        .map(|(tail, title)| ResolvedChild {
            url: Self::normalize_github_item_url(&format!(
                "https://github.com/{org}/{repo}/{tail}"
            )),
            title: title.to_string(),
            body: None,
        })
        .collect()
    }

    async fn list_issues(&self, org: &str, repo: &str) -> Result<Vec<ResolvedChild>, ResolverError> {
        let url = format!(
            "https://api.github.com/repos/{org}/{repo}/issues?state=open&per_page=50"
        );
        let resp = self.get_json(&url).await?;
        if !resp.status().is_success() {
            return Err(ResolverError::Msg(format!(
                "GitHub issues: HTTP {}",
                resp.status()
            )));
        }
        let v: Value = resp.json().await.map_err(|e| ResolverError::Msg(e.to_string()))?;
        let arr = v.as_array().ok_or_else(|| ResolverError::Msg("issues: not array".into()))?;
        let mut out = Vec::new();
        for ent in arr {
            if ent.get("pull_request").is_some() {
                continue;
            }
            let num = ent.get("number").and_then(|x| x.as_u64()).unwrap_or(0);
            let title = ent
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("issue")
                .to_string();
            let html_url = ent
                .get("html_url")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let url = if !html_url.is_empty() {
                Self::normalize_github_item_url(html_url)
            } else {
                Self::normalize_github_item_url(&format!(
                    "https://github.com/{org}/{repo}/issues/{num}"
                ))
            };
            out.push(ResolvedChild {
                url,
                title: format!("#{num} {title}"),
                body: None,
            });
        }
        Ok(out)
    }

    async fn list_pulls(&self, org: &str, repo: &str) -> Result<Vec<ResolvedChild>, ResolverError> {
        let url = format!(
            "https://api.github.com/repos/{org}/{repo}/pulls?state=open&per_page=50"
        );
        let resp = self.get_json(&url).await?;
        if !resp.status().is_success() {
            return Err(ResolverError::Msg(format!(
                "GitHub pulls: HTTP {}",
                resp.status()
            )));
        }
        let v: Value = resp.json().await.map_err(|e| ResolverError::Msg(e.to_string()))?;
        let arr = v.as_array().ok_or_else(|| ResolverError::Msg("pulls: not array".into()))?;
        let mut out = Vec::new();
        for ent in arr {
            let num = ent.get("number").and_then(|x| x.as_u64()).unwrap_or(0);
            let title = ent
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("PR")
                .to_string();
            let html_url = ent
                .get("html_url")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let url = if !html_url.is_empty() {
                Self::normalize_github_item_url(html_url)
            } else {
                Self::normalize_github_item_url(&format!(
                    "https://github.com/{org}/{repo}/pulls/{num}"
                ))
            };
            out.push(ResolvedChild {
                url,
                title: format!("PR #{num} {title}"),
                body: None,
            });
        }
        Ok(out)
    }

    async fn fetch_issue(&self, org: &str, repo: &str, num: u32) -> Result<String, ResolverError> {
        let url = format!("https://api.github.com/repos/{org}/{repo}/issues/{num}");
        let resp = self.get_json(&url).await?;
        if !resp.status().is_success() {
            return Err(ResolverError::Msg(format!(
                "GitHub issue: HTTP {}",
                resp.status()
            )));
        }
        let v: Value = resp.json().await.map_err(|e| ResolverError::Msg(e.to_string()))?;
        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
        Ok(format!("#{num} {title}\n\n{body}"))
    }

    async fn fetch_pull(&self, org: &str, repo: &str, num: u32) -> Result<String, ResolverError> {
        let url = format!("https://api.github.com/repos/{org}/{repo}/pulls/{num}");
        let resp = self.get_json(&url).await?;
        if !resp.status().is_success() {
            return Err(ResolverError::Msg(format!(
                "GitHub pull: HTTP {}",
                resp.status()
            )));
        }
        let v: Value = resp.json().await.map_err(|e| ResolverError::Msg(e.to_string()))?;
        let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
        Ok(format!("PR #{num} {title}\n\n{body}"))
    }
}

#[async_trait]
impl DomainResolver for GitHubResolver {
    fn matches_host(&self, host: &str) -> bool {
        host.trim().eq_ignore_ascii_case("github.com")
    }

    async fn list_children(&self, parent: &ItemId) -> Result<Vec<ResolvedChild>, ResolverError> {
        let Some(segs) = Self::parse_github_path(parent) else {
            return Ok(vec![]);
        };
        let cache_key = parent.as_str().to_string();
        {
            let guard = self.list_cache.lock().map_err(|e| ResolverError::Msg(e.to_string()))?;
            if let Some((t, v)) = guard.get(&cache_key) {
                if t.elapsed() < LIST_CACHE_TTL {
                    return Ok(v.clone());
                }
            }
        }

        let out: Result<Vec<ResolvedChild>, ResolverError> = match segs.len() {
            1 => {
                let org = &segs[0];
                self.list_org_repos(org).await
            }
            2 => {
                let org = &segs[0];
                let repo = &segs[1];
                Ok(self.structural_repo_children(org, repo))
            }
            3 => {
                let org = &segs[0];
                let repo = &segs[1];
                match segs[2].as_str() {
                    "issues" => self.list_issues(org, repo).await,
                    "pulls" => self.list_pulls(org, repo).await,
                    _ => Ok(vec![]),
                }
            }
            _ => Ok(vec![]),
        };

        let resolved = out?;
        if let Ok(mut guard) = self.list_cache.lock() {
            guard.insert(
                cache_key,
                (Instant::now(), resolved.clone()),
            );
        }
        Ok(resolved)
    }

    async fn fetch_body(&self, item: &ItemId) -> Result<String, ResolverError> {
        let Some(segs) = Self::parse_github_path(item) else {
            return Err(ResolverError::Msg("not a github.com item".into()));
        };
        if segs.len() != 4 {
            return Err(ResolverError::Msg("no body fetch for this github path".into()));
        }
        let org = &segs[0];
        let repo = &segs[1];
        let n: u32 = segs[3].parse().map_err(|_| ResolverError::Msg("bad issue/pr number".into()))?;
        match segs[2].as_str() {
            "issues" => self.fetch_issue(org, repo, n).await,
            "pulls" => self.fetch_pull(org, repo, n).await,
            _ => Err(ResolverError::Msg("unsupported github leaf".into())),
        }
    }
}
