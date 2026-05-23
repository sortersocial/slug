use async_trait::async_trait;
use maud::html;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use crate::{path_types::ItemId, state::AppState, write_cmd::WriteCmd};

pub const SLUG_GITHUB_SCHEMA: &str = "slug_github_import";

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GithubImportKind {
    Repo,
    RepoSection,
    Issue,
    Pull,
    Commit,
    Release,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubImportCard {
    pub v: u32,
    #[serde(default)]
    pub schema: String,
    pub kind: GithubImportKind,
    pub url: String,
    pub headline: String,
    #[serde(default)]
    pub sublines: Vec<String>,
    #[serde(default)]
    pub excerpt: Option<String>,
}

impl GithubImportCard {
    fn new(kind: GithubImportKind, url: String, headline: String) -> Self {
        Self {
            v: 1,
            schema: SLUG_GITHUB_SCHEMA.to_string(),
            kind,
            url,
            headline,
            sublines: Vec::new(),
            excerpt: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedChild {
    pub url: String,
    pub title: String,
    pub card: GithubImportCard,
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
            let url = format!("https://github.com/{full_name}");
            let mut card = card_for_repo(repo, &url);
            card.headline = full_name.clone();
            out.push(ResolvedChild {
                url,
                title: full_name,
                card,
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
            let url = format!("https://github.com/{owner}/{repo}/issues/{number}");
            let card = card_for_issue(issue, &url, GithubImportKind::Issue);
            out.push(ResolvedChild {
                url: url.clone(),
                title: format!("#{number} {title}"),
                card,
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
            let url = format!("https://github.com/{owner}/{repo}/pulls/{number}");
            let card = card_for_issue(pull, &url, GithubImportKind::Pull);
            out.push(ResolvedChild {
                url: url.clone(),
                title: format!("#{number} {title}"),
                card,
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
            let card = card_for_commit(commit, &url, &short, title);
            out.push(ResolvedChild {
                url: url.clone(),
                title: format!("{short} {title}"),
                card,
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
            let card = card_for_release(release, &url, title);
            out.push(ResolvedChild {
                url: url.clone(),
                title: title.to_string(),
                card,
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

fn title_case_segment(seg: &str) -> String {
    let mut c = seg.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().chain(c).collect(),
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
    .map(|(section, blurb)| {
        let url = format!("https://github.com/{owner}/{repo}/{section}");
        let mut card = GithubImportCard::new(
            GithubImportKind::RepoSection,
            url.clone(),
            format!("{owner}/{repo} — {}", title_case_segment(section)),
        );
        card.excerpt = Some(blurb.to_string());
        ResolvedChild {
            url,
            title: section.to_string(),
            card,
        }
    })
    .collect()
}

fn resolver_thread_tag(item: &ItemId) -> String {
    let tail = item
        .display_path()
        .trim_start_matches("-/")
        .replace(['/', '?'], ":");
    format!("import:{tail}")
}

fn children_to_dsl(children: &[ResolvedChild]) -> String {
    let mut out = String::new();
    for child in children {
        let json = serde_json::to_string(&child.card).unwrap_or_else(|_| "{}".to_string());
        let inner = format!("```slug-github-card\n{json}\n```");
        out.push_str(&format!("{} {{\n{}\n}}\n\n", child.url, inner));
    }
    out
}

fn card_for_repo(repo: &Value, fallback_url: &str) -> GithubImportCard {
    let url = github_string(repo, "html_url")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_url.to_string());
    let full_name = github_string(repo, "full_name")
        .or_else(|| github_string(repo, "name"))
        .unwrap_or("repository");
    let mut card = GithubImportCard::new(GithubImportKind::Repo, url, full_name.to_string());
    if let Some(lang) = github_string(repo, "language") {
        card.sublines.push(format!("Language: {lang}"));
    }
    if let Some(desc) = github_string(repo, "description") {
        card.excerpt = Some(desc.to_string());
    }
    card
}

fn excerpt_from_github_body(body: Option<&str>) -> Option<String> {
    let b = body?.trim();
    if b.is_empty() {
        return None;
    }
    let max = 1200usize;
    if b.len() <= max {
        Some(b.to_string())
    } else {
        Some(format!("{}…", b.chars().take(max).collect::<String>()))
    }
}

fn card_for_issue(v: &Value, url: &str, kind: GithubImportKind) -> GithubImportCard {
    let number = v.get("number").and_then(|n| n.as_i64());
    let title = github_string(v, "title").unwrap_or("Untitled");
    let state = github_string(v, "state").unwrap_or("unknown");
    let headline = match number {
        Some(n) => format!("#{n} {title}"),
        None => title.to_string(),
    };
    let mut card = GithubImportCard::new(kind, url.to_string(), headline);
    card.sublines.push(format!("State: {state}"));
    if let Some(a) = github_user_login(v) {
        card.sublines.push(format!("Author: @{a}"));
    }
    let labels = github_labels(v);
    if !labels.is_empty() {
        card.sublines
            .push(format!("Labels: {}", labels.join(", ")));
    }
    card.excerpt = excerpt_from_github_body(github_string(v, "body"));
    card
}

fn card_for_commit(v: &Value, url: &str, short_sha: &str, subject: &str) -> GithubImportCard {
    let headline = format!("{short_sha} {subject}");
    let mut card = GithubImportCard::new(GithubImportKind::Commit, url.to_string(), headline);
    if let Some(name) = v
        .get("commit")
        .and_then(|c| c.get("author"))
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        card.sublines.push(format!("Author: {name}"));
    }
    if let Some(login) = github_user_login(v) {
        card.sublines.push(format!("GitHub: @{login}"));
    }
    if let Some(date) = v
        .get("commit")
        .and_then(|c| c.get("author"))
        .and_then(|a| a.get("date"))
        .and_then(|d| d.as_str())
    {
        card.sublines.push(format!("Date: {date}"));
    }
    if let Some(msg) = v
        .get("commit")
        .and_then(|c| c.get("message"))
        .and_then(|m| m.as_str())
    {
        card.excerpt = excerpt_from_github_body(Some(msg));
    }
    card
}

fn card_for_release(v: &Value, url: &str, title: &str) -> GithubImportCard {
    let tag = github_string(v, "tag_name").unwrap_or("untagged");
    let mut card = GithubImportCard::new(
        GithubImportKind::Release,
        url.to_string(),
        format!("Release — {title}"),
    );
    card.sublines.push(format!("Tag: {tag}"));
    if v.get("draft").and_then(|b| b.as_bool()).unwrap_or(false) {
        card.sublines.push("Draft: yes".to_string());
    }
    if v.get("prerelease")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
    {
        card.sublines.push("Prerelease: yes".to_string());
    }
    if let Some(a) = github_user_login(v) {
        card.sublines.push(format!("Author: @{a}"));
    }
    if let Some(pub_at) = github_string(v, "published_at") {
        card.sublines.push(format!("Published: {pub_at}"));
    }
    card.excerpt = excerpt_from_github_body(github_string(v, "body"));
    card
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

fn extract_fence<'a>(body: &'a str, lang: &str) -> Option<&'a str> {
    let b = body.trim();
    let prefix = format!("```{lang}");
    let rest = b.strip_prefix(prefix.as_str())?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix('\r'))
        .unwrap_or(rest);
    let end = rest.find("\n```")?;
    Some(rest[..end].trim())
}

fn parse_github_import_from_body(body: &str) -> Option<GithubImportCard> {
    let trimmed = body.trim();
    if let Some(json) = extract_fence(trimmed, "slug-github-card") {
        let c: GithubImportCard = serde_json::from_str(json).ok()?;
        return (c.v == 1 && (c.schema.is_empty() || c.schema == SLUG_GITHUB_SCHEMA)).then_some(c);
    }
    if let Some(json) = extract_fence(trimmed, "json") {
        if let Ok(c) = serde_json::from_str::<GithubImportCard>(json) {
            if c.v == 1
                && (c.schema == SLUG_GITHUB_SCHEMA
                    || (c.schema.is_empty() && c.url.contains("github.com")))
            {
                return Some(c);
            }
        }
    }
    if trimmed.starts_with('{') {
        let c: GithubImportCard = serde_json::from_str(trimmed).ok()?;
        return (c.v == 1
            && (c.schema == SLUG_GITHUB_SCHEMA
                || (c.schema.is_empty() && c.url.contains("github.com"))))
        .then_some(c);
    }
    None
}

fn kind_badge(kind: &GithubImportKind) -> &'static str {
    match kind {
        GithubImportKind::Repo => "GitHub · repository",
        GithubImportKind::RepoSection => "GitHub · tree",
        GithubImportKind::Issue => "GitHub · issue",
        GithubImportKind::Pull => "GitHub · pull request",
        GithubImportKind::Commit => "GitHub · commit",
        GithubImportKind::Release => "GitHub · release",
    }
}

fn render_github_card(card: &GithubImportCard) -> maud::Markup {
    html! {
        article.github-import-card {
            header.github-import-card__hdr {
                span class="github-import-card__badge" { (kind_badge(&card.kind)) }
                h3.github-import-card__title { (card.headline.as_str()) }
            }
            @if !card.sublines.is_empty() {
                ul.github-import-card__meta {
                    @for line in &card.sublines {
                        li { (line.as_str()) }
                    }
                }
            }
            @if let Some(ex) = &card.excerpt {
                div.github-import-card__excerpt {
                    @for block in ex.split("\n\n") {
                        @if !block.trim().is_empty() {
                            p { (block) }
                        }
                    }
                }
            }
            p.github-import-card__link {
                a href=(card.url.as_str()) rel="noopener noreferrer" target="_blank" {
                    "Open on GitHub"
                }
            }
        }
    }
}

/// Rich HTML for bodies that contain a [`GithubImportCard`] fence (or equivalent JSON).
pub fn try_render_github_import_markup(raw: &str) -> Option<maud::Markup> {
    let card = parse_github_import_from_body(raw)?;
    Some(render_github_card(&card))
}

#[async_trait]
impl ExternalResolver for GitHubResolver {
    fn domain_match(&self) -> &'static str {
        "github.com"
    }

    fn normalize(&self, path: &str) -> String {
        path.to_string()
    }

    async fn fetch_body(&self, _item: &ItemId) -> Result<String, String> {
        Err("GitHub fetch_body not implemented".to_string())
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
    fn children_to_dsl_wraps_slug_github_card() {
        let dsl = children_to_dsl(&[ResolvedChild {
            url: "https://github.com/o/r/issues/1".into(),
            title: "#1 title".into(),
            card: GithubImportCard::new(
                GithubImportKind::Issue,
                "https://github.com/o/r/issues/1".into(),
                "#1 title".into(),
            ),
        }]);
        assert!(dsl.contains("https://github.com/o/r/issues/1"));
        assert!(dsl.contains("```slug-github-card"));
        assert!(dsl.contains("\"schema\":\"slug_github_import\""));
    }

    #[test]
    fn parse_accepts_slug_github_fence() {
        let card = GithubImportCard::new(
            GithubImportKind::Repo,
            "https://github.com/o/r".into(),
            "o/r".into(),
        );
        let body = format!("```slug-github-card\n{}\n```\n", serde_json::to_string(&card).unwrap());
        let parsed = parse_github_import_from_body(&body).expect("parses");
        assert_eq!(parsed, card);
    }

    #[test]
    fn parse_accepts_schema_json_fence() {
        let card = GithubImportCard::new(
            GithubImportKind::Issue,
            "https://github.com/o/r/issues/2".into(),
            "#2 hi".into(),
        );
        let json = serde_json::to_string(&card).unwrap();
        let body = format!("```json\n{json}\n```");
        let parsed = parse_github_import_from_body(&body).expect("parses json fence");
        assert_eq!(parsed.headline, "#2 hi");
    }

    #[test]
    fn issue_card_includes_author_and_excerpt() {
        let issue = serde_json::json!({
            "number": 12,
            "title": "Render children",
            "state": "open",
            "html_url": "https://github.com/o/r/issues/12",
            "user": {"login": "octo"},
            "labels": [{"name": "bug"}],
            "body": "The issue body."
        });
        let card = card_for_issue(
            &issue,
            "https://github.com/o/r/issues/12",
            GithubImportKind::Issue,
        );
        assert!(card.sublines.iter().any(|l| l.contains("@octo")));
        assert_eq!(card.excerpt.as_deref(), Some("The issue body.").as_deref());
    }
}
