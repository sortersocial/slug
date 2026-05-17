use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::{path_types::ItemId, state::AppState, write_cmd::WriteCmd};

const GITHUB_SYSTEM_PRINCIPAL: &str = "system:github-resolver";
const GITHUB_RESOLVER_COOLDOWN_MS: i64 = 15_000;
const GITHUB_MAX_PAGES: usize = 3;

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChild {
    pub url: String,
    pub title: String,
    pub body: Option<String>,
}

#[async_trait]
pub trait ExternalResolver: Send + Sync {
    /// e.g. `"github.com"`
    fn domain_match(&self) -> &'static str;

    /// Normalizes URLs (e.g. stripping fragments); extend per-domain later.
    fn normalize(&self, path: &str) -> String;

    /// Fetches body when missing; GitHub hook lands here in a follow-up.
    async fn fetch_body(&self, item: &ItemId) -> Result<String, String>;
}

#[derive(Clone)]
pub struct GitHubResolver {
    client: reqwest::Client,
    api_base_url: String,
    token: Option<String>,
}

impl GitHubResolver {
    pub fn from_env() -> Self {
        let api_base_url = std::env::var("SLUG_GITHUB_API_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "https://api.github.com".to_string());
        let token = std::env::var("SLUG_GITHUB_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self {
            client: reqwest::Client::new(),
            api_base_url: api_base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    pub fn can_resolve_children(&self, item: &ItemId) -> bool {
        github_segments(item).is_some()
    }

    pub async fn list_children(&self, item: &ItemId) -> Result<Vec<ResolvedChild>, String> {
        let segments = github_segments(item).ok_or_else(|| "not a GitHub URL".to_string())?;
        match segments.as_slice() {
            [] => Ok(vec![]),
            [owner] => self.list_repos(owner).await,
            [owner, repo] => Ok(github_repo_sections(owner, repo)),
            [owner, repo, section] if section == "issues" => self.list_issues(owner, repo).await,
            [owner, repo, section] if section == "pulls" => self.list_pulls(owner, repo).await,
            [owner, repo, section] if section == "commits" => self.list_commits(owner, repo).await,
            [owner, repo, section] if section == "releases" => {
                self.list_releases(owner, repo).await
            }
            _ => Ok(vec![]),
        }
    }

    async fn get_json(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}/{}", self.api_base_url, path.trim_start_matches('/'));
        let mut req = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, "slugsocial-github-resolver");
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("GitHub request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("GitHub request returned {status}"));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| format!("GitHub response JSON failed: {e}"))
    }

    async fn get_json_array_pages(&self, path: &str) -> Result<Vec<Value>, String> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut out = Vec::new();
        for page in 1..=GITHUB_MAX_PAGES {
            let value = self.get_json(&format!("{path}{sep}page={page}")).await?;
            let arr = value
                .as_array()
                .ok_or_else(|| "GitHub paged response was not an array".to_string())?;
            let n = arr.len();
            out.extend(arr.iter().cloned());
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    async fn list_repos(&self, owner: &str) -> Result<Vec<ResolvedChild>, String> {
        let arr = self
            .get_json_array_pages(&format!(
                "/users/{owner}/repos?per_page=100&sort=updated&type=owner"
            ))
            .await?;
        let mut out = Vec::new();
        for repo in &arr {
            let name = repo
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let full_name = repo
                .get("full_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_else(|| format!("{owner}/{name}").to_ascii_lowercase());
            out.push(ResolvedChild {
                url: format!("https://github.com/{full_name}"),
                title: full_name.clone(),
                body: Some(github_repo_body(repo)),
            });
        }
        out.sort_by(|a, b| a.url.cmp(&b.url));
        Ok(out)
    }

    async fn list_issues(&self, owner: &str, repo: &str) -> Result<Vec<ResolvedChild>, String> {
        let arr = self
            .get_json_array_pages(&format!(
                "/repos/{owner}/{repo}/issues?state=open&per_page=100"
            ))
            .await?;
        let mut out = Vec::new();
        for issue in &arr {
            if issue.get("pull_request").is_some() {
                continue;
            }
            let Some(number) = issue.get("number").and_then(|v| v.as_i64()) else {
                continue;
            };
            let title = issue
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled issue");
            out.push(ResolvedChild {
                url: format!("https://github.com/{owner}/{repo}/issues/{number}"),
                title: format!("#{number} {title}"),
                body: Some(github_issue_body(issue, "issue")),
            });
        }
        out.sort_by(|a, b| a.url.cmp(&b.url));
        Ok(out)
    }

    async fn list_pulls(&self, owner: &str, repo: &str) -> Result<Vec<ResolvedChild>, String> {
        let arr = self
            .get_json_array_pages(&format!(
                "/repos/{owner}/{repo}/pulls?state=open&per_page=100"
            ))
            .await?;
        let mut out = Vec::new();
        for pull in &arr {
            let Some(number) = pull.get("number").and_then(|v| v.as_i64()) else {
                continue;
            };
            let title = pull
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled pull request");
            out.push(ResolvedChild {
                url: format!("https://github.com/{owner}/{repo}/pulls/{number}"),
                title: format!("#{number} {title}"),
                body: Some(github_issue_body(pull, "pull request")),
            });
        }
        out.sort_by(|a, b| a.url.cmp(&b.url));
        Ok(out)
    }

    async fn list_commits(&self, owner: &str, repo: &str) -> Result<Vec<ResolvedChild>, String> {
        let arr = self
            .get_json_array_pages(&format!("/repos/{owner}/{repo}/commits?per_page=100"))
            .await?;
        let mut out = Vec::new();
        for commit in &arr {
            let Some(sha) = github_string(commit, "sha") else {
                continue;
            };
            let short = sha.chars().take(7).collect::<String>();
            let title = commit
                .get("commit")
                .and_then(|c| c.get("message"))
                .and_then(|v| v.as_str())
                .and_then(|m| m.lines().next())
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("commit");
            let url = github_string(commit, "html_url")
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("https://github.com/{owner}/{repo}/commit/{sha}"));
            out.push(ResolvedChild {
                url,
                title: format!("{short} {title}"),
                body: Some(github_commit_body(commit)),
            });
        }
        out.sort_by(|a, b| a.url.cmp(&b.url));
        Ok(out)
    }

    async fn list_releases(&self, owner: &str, repo: &str) -> Result<Vec<ResolvedChild>, String> {
        let arr = self
            .get_json_array_pages(&format!("/repos/{owner}/{repo}/releases?per_page=100"))
            .await?;
        let mut out = Vec::new();
        for release in &arr {
            let Some(tag) = github_string(release, "tag_name") else {
                continue;
            };
            let title = github_string(release, "name").unwrap_or(tag);
            let url = github_string(release, "html_url")
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("https://github.com/{owner}/{repo}/releases/tag/{tag}"));
            out.push(ResolvedChild {
                url,
                title: title.to_string(),
                body: Some(github_release_body(release)),
            });
        }
        out.sort_by(|a, b| a.url.cmp(&b.url));
        Ok(out)
    }
}

fn github_segments(item: &ItemId) -> Option<Vec<String>> {
    let url = url::Url::parse(item.as_str()).ok()?;
    if url.host_str()?.eq_ignore_ascii_case("github.com") {
        Some(
            url.path_segments()
                .map(|segments| {
                    segments
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_ascii_lowercase())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )
    } else {
        None
    }
}

fn github_repo_sections(owner: &str, repo: &str) -> Vec<ResolvedChild> {
    [
        ("issues", "GitHub issues for this repository."),
        ("pulls", "GitHub pull requests for this repository."),
        ("commits", "GitHub commits for this repository."),
        ("releases", "GitHub releases for this repository."),
    ]
    .into_iter()
    .map(|(section, body)| ResolvedChild {
        url: format!("https://github.com/{owner}/{repo}/{section}"),
        title: section.to_string(),
        body: Some(body.to_string()),
    })
    .collect()
}

fn resolver_thread_tag(item: &ItemId) -> String {
    let tail = item
        .display_path()
        .trim_start_matches("-/")
        .replace('/', ":")
        .replace('?', ":");
    format!("import:{tail}")
}

fn sanitize_body(s: &str) -> String {
    s.replace('{', "(")
        .replace('}', ")")
        .replace("```", "` ` `")
        .chars()
        .take(4_000)
        .collect()
}

fn github_string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn github_user_login(value: &Value) -> Option<&str> {
    value
        .get("user")
        .and_then(|u| u.get("login"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn github_labels(value: &Value) -> Vec<String> {
    value
        .get("labels")
        .and_then(|v| v.as_array())
        .into_iter()
        .flat_map(|labels| labels.iter())
        .filter_map(|label| label.get("name").and_then(|v| v.as_str()))
        .filter(|name| !name.trim().is_empty())
        .map(|name| name.to_string())
        .collect()
}

fn github_repo_body(repo: &Value) -> String {
    let full_name = github_string(repo, "full_name")
        .or_else(|| github_string(repo, "name"))
        .unwrap_or("GitHub repository");
    let mut lines = vec![full_name.to_string()];
    if let Some(desc) = github_string(repo, "description") {
        lines.push(String::new());
        lines.push(desc.to_string());
    }
    if let Some(url) = github_string(repo, "html_url") {
        lines.push(String::new());
        lines.push(format!("Source: {url}"));
    }
    if let Some(lang) = github_string(repo, "language") {
        lines.push(format!("Language: {lang}"));
    }
    lines.join("\n")
}

fn github_issue_body(issue: &Value, kind: &str) -> String {
    let number = issue
        .get("number")
        .and_then(|v| v.as_i64())
        .map(|n| format!("#{n} "))
        .unwrap_or_default();
    let title = github_string(issue, "title").unwrap_or("Untitled");
    let state = github_string(issue, "state").unwrap_or("unknown");
    let mut lines = vec![format!("{kind} {number}{title}")];
    lines.push(format!("State: {state}"));
    if let Some(author) = github_user_login(issue) {
        lines.push(format!("Author: @{author}"));
    }
    let labels = github_labels(issue);
    if !labels.is_empty() {
        lines.push(format!("Labels: {}", labels.join(", ")));
    }
    if let Some(url) = github_string(issue, "html_url") {
        lines.push(format!("Source: {url}"));
    }
    if let Some(body) = github_string(issue, "body") {
        lines.push(String::new());
        lines.push(body.to_string());
    }
    lines.join("\n")
}

fn github_commit_body(commit: &Value) -> String {
    let sha = github_string(commit, "sha").unwrap_or("unknown");
    let short = sha.chars().take(7).collect::<String>();
    let commit_obj = commit.get("commit");
    let message = commit_obj
        .and_then(|c| c.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("commit");
    let mut lines = vec![format!("commit {short}")];
    if let Some(author) = commit_obj
        .and_then(|c| c.get("author"))
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("Author: {author}"));
    }
    if let Some(login) = github_user_login(commit) {
        lines.push(format!("GitHub user: @{login}"));
    }
    if let Some(date) = commit_obj
        .and_then(|c| c.get("author"))
        .and_then(|a| a.get("date"))
        .and_then(|v| v.as_str())
    {
        lines.push(format!("Date: {date}"));
    }
    if let Some(url) = github_string(commit, "html_url") {
        lines.push(format!("Source: {url}"));
    }
    lines.push(String::new());
    lines.push(message.to_string());
    lines.join("\n")
}

fn github_release_body(release: &Value) -> String {
    let tag = github_string(release, "tag_name").unwrap_or("untagged");
    let title = github_string(release, "name").unwrap_or(tag);
    let mut lines = vec![format!("release {title}")];
    lines.push(format!("Tag: {tag}"));
    if release
        .get("draft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        lines.push("Draft: yes".to_string());
    }
    if release
        .get("prerelease")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        lines.push("Prerelease: yes".to_string());
    }
    if let Some(author) = github_user_login(release) {
        lines.push(format!("Author: @{author}"));
    }
    if let Some(published) = github_string(release, "published_at") {
        lines.push(format!("Published: {published}"));
    }
    if let Some(url) = github_string(release, "html_url") {
        lines.push(format!("Source: {url}"));
    }
    if let Some(body) = github_string(release, "body") {
        lines.push(String::new());
        lines.push(body.to_string());
    }
    lines.join("\n")
}

fn children_to_dsl(children: &[ResolvedChild]) -> String {
    let mut out = String::new();
    for child in children {
        let body = child
            .body
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(child.title.as_str());
        if body.trim_start().starts_with("```") {
            out.push_str(&format!("{} {{\n{}\n}}\n\n", child.url, body.trim()));
        } else {
            out.push_str(&format!(
                "{} {{\n{}\n}}\n\n",
                child.url,
                sanitize_body(body)
            ));
        }
    }
    out
}

pub async fn resolve_github_children(
    state: &AppState,
    room: &str,
    item: &ItemId,
) -> Result<usize, String> {
    if !state.github_resolver.can_resolve_children(item) {
        return Err("no GitHub resolver for this item".to_string());
    }

    let key = format!("github:{}:{}", room.trim(), item.as_str());
    let now = now_ms();
    {
        let mut runs = state.resolver_runs.write().await;
        if let Some(last) = runs.get(&key) {
            let remaining = GITHUB_RESOLVER_COOLDOWN_MS - (now - *last);
            if remaining > 0 {
                return Err(format!(
                    "GitHub resolver cooldown: try again in {}s",
                    (remaining + 999) / 1000
                ));
            }
        }
        runs.insert(key, now);
    }

    let children = state.github_resolver.list_children(item).await?;
    if children.is_empty() {
        return Ok(0);
    }
    let text = children_to_dsl(&children);
    let thread_tag = resolver_thread_tag(item);
    let (tx, rx) = oneshot::channel();
    state
        .write_tx
        .send(WriteCmd::SystemIngest {
            room: room.to_string(),
            thread_tag,
            text,
            principal: GITHUB_SYSTEM_PRINCIPAL.to_string(),
            reply: tx,
        })
        .await
        .map_err(|_| "writer unavailable".to_string())?;
    rx.await
        .map_err(|_| "writer dropped".to_string())?
        .map_err(|(msg, hint)| hint.map_or(msg.clone(), |h| format!("{msg}: {h}")))?;
    Ok(children.len())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_segments_parse_normalized_url() {
        let item = ItemId::parse("https://github.com/Sortersocial/Slug/issues").unwrap();
        assert_eq!(
            github_segments(&item),
            Some(vec![
                "sortersocial".to_string(),
                "slug".to_string(),
                "issues".to_string()
            ])
        );
    }

    #[test]
    fn repo_sections_are_direct_children() {
        let sections = github_repo_sections("sortersocial", "slug");
        let urls: Vec<String> = sections.into_iter().map(|c| c.url).collect();
        assert!(urls.contains(&"https://github.com/sortersocial/slug/issues".to_string()));
        assert!(urls.contains(&"https://github.com/sortersocial/slug/pulls".to_string()));
    }

    #[test]
    fn children_to_dsl_contains_item_bodies() {
        let dsl = children_to_dsl(&[ResolvedChild {
            url: "https://github.com/o/r/issues/1".into(),
            title: "#1 title".into(),
            body: Some("body with {braces}".into()),
        }]);
        assert!(dsl.contains("https://github.com/o/r/issues/1"));
        assert!(dsl.contains("body with (braces)"));
    }

    #[test]
    fn children_to_dsl_preserves_fenced_json_bodies() {
        let dsl = children_to_dsl(&[ResolvedChild {
            url: "https://github.com/o/r/issues/1".into(),
            title: "#1 title".into(),
            body: Some("```json\n{\"test\": true}\n```".into()),
        }]);
        assert!(dsl.contains("https://github.com/o/r/issues/1 {\n```json"));
        assert!(dsl.contains("{\"test\": true}"));
        assert!(dsl.contains("```\n}\n"));
    }

    #[test]
    fn github_issue_body_is_readable_text_not_json_dump() {
        let issue = serde_json::json!({
            "number": 12,
            "title": "Render children",
            "state": "open",
            "html_url": "https://github.com/o/r/issues/12",
            "user": {"login": "octo"},
            "labels": [{"name": "bug"}],
            "body": "The issue body."
        });
        let body = github_issue_body(&issue, "issue");
        assert!(body.contains("issue #12 Render children"));
        assert!(body.contains("Author: @octo"));
        assert!(body.contains("The issue body."));
        assert!(!body.trim_start().starts_with("```json"));
    }

    #[test]
    fn github_commit_and_release_bodies_are_readable() {
        let commit = serde_json::json!({
            "sha": "abcdef123456",
            "html_url": "https://github.com/o/r/commit/abcdef123456",
            "author": {"login": "octo"},
            "commit": {
                "message": "Fix vote page\n\nDetails here.",
                "author": {"name": "Octo Dev", "date": "2026-05-17T00:00:00Z"}
            }
        });
        let release = serde_json::json!({
            "tag_name": "v1.2.3",
            "name": "Release 1.2.3",
            "html_url": "https://github.com/o/r/releases/tag/v1.2.3",
            "author": {"login": "octo"},
            "prerelease": true,
            "body": "Release notes."
        });
        assert!(github_commit_body(&commit).contains("commit abcdef1"));
        assert!(github_commit_body(&commit).contains("Fix vote page"));
        assert!(github_release_body(&release).contains("release Release 1.2.3"));
        assert!(github_release_body(&release).contains("Prerelease: yes"));
    }
}
