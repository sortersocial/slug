use async_trait::async_trait;
use maud::html;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    child_urls_declared_in_ingest, extract_fence, normalized_child_url, now_ms,
    resolver_cooldown_ms, system_ingest, system_redact, ResolveStats,
};
use crate::{path_types::ItemId, state::AppState};

pub const SLUG_GITHUB_SCHEMA: &str = "slug_github_import";

const GITHUB_SYSTEM_PRINCIPAL: &str = "system:github-resolver";
const GITHUB_RESOLVER_COOLDOWN_MS_DEFAULT: i64 = 15_000;
/// Default page cap for non-issues list endpoints (repos/PRs/commits/releases).
const GITHUB_MAX_PAGES: usize = 3;
/// Safety valve while paging open issues (`per_page=100` → up to 100k issues).
const GITHUB_MAX_ISSUE_PAGES: usize = 1000;

fn github_cooldown_ms() -> i64 {
    resolver_cooldown_ms(
        "SLUG_GITHUB_RESOLVER_COOLDOWN_MS",
        GITHUB_RESOLVER_COOLDOWN_MS_DEFAULT,
    )
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
        self.get_json_array_pages_limited(path, GITHUB_MAX_PAGES)
            .await
    }

    async fn get_json_array_pages_limited(
        &self,
        path: &str,
        max_pages: usize,
    ) -> Result<Vec<Value>, String> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let mut out = Vec::new();
        for page in 1..=max_pages {
            let value = self.get_json(&format!("{path}{sep}page={page}")).await?;
            let arr = value
                .as_array()
                .ok_or_else(|| "GitHub paged response was not an array".to_string())?;
            let n = arr.len();
            out.extend(arr.iter().cloned());
            if n < 100 {
                break;
            }
            if page == max_pages && n >= 100 {
                return Err(format!(
                    "GitHub list truncated after {max_pages} pages ({} items); refine scope or raise page cap",
                    out.len()
                ));
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
            .get_json_array_pages_limited(
                &format!("/repos/{owner}/{repo}/issues?state=open&per_page=100"),
                GITHUB_MAX_ISSUE_PAGES,
            )
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

/// Stable import thread for a GitHub URL: one thread per repo (`owner/repo`),
/// or per owner when resolving a user/org page. Section suffixes (`/issues`, etc.)
/// must not create additional threads.
fn resolver_thread_tag(item: &ItemId) -> String {
    let path = match github_segments(item).as_deref() {
        Some([owner, repo, ..]) => format!("https://github.com/{owner}/{repo}"),
        Some([owner]) => format!("https://github.com/{owner}"),
        _ => item.display_path().trim_start_matches("-/").to_string(),
    };
    let tail = path.replace(['/', '?'], ":");
    format!("import:{tail}")
}

/// Encode a card for a ```slug-github-card``` fence.
///
/// DSL code fences are **toggle** markers (see `BlockMasker`): a ``` inside the
/// fence payload closes it. The UUID/token substitution protects fence *contents*
/// from brace matching and allows *sibling* fences inside `{ … }`, but it cannot
/// nest fences. Issue bodies often contain markdown fences, so the JSON card is
/// stored as base64 — opaque to the masker, decoded back to real markdown for
/// rendering (and a future markdown renderer on `excerpt`).
fn card_payload_for_dsl_fence(card: &GithubImportCard) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let json = serde_json::to_string(card).unwrap_or_else(|_| "{}".to_string());
    STANDARD.encode(json.as_bytes())
}

fn decode_github_card_payload(payload: &str) -> Option<GithubImportCard> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let bytes = STANDARD.decode(payload.trim()).ok()?;
    let s = std::str::from_utf8(&bytes).ok()?;
    serde_json::from_str(s).ok()
}

fn child_to_dsl(child: &ResolvedChild) -> String {
    let payload = card_payload_for_dsl_fence(&child.card);
    let inner = format!("```slug-github-card\n{payload}\n```");
    format!("{} {{\n{}\n}}\n", child.url, inner)
}

fn children_to_dsl(children: &[ResolvedChild]) -> String {
    let mut out = String::new();
    for child in children {
        out.push_str(&child_to_dsl(child));
        out.push('\n');
    }
    out
}

/// One post per open issue; redact posts for closed/missing issues (and bulk posts).
async fn resolve_github_issues(
    state: &AppState,
    room: &str,
    issues_parent: &ItemId,
    open_children: Vec<ResolvedChild>,
) -> Result<ResolveStats, String> {
    use crate::canonical_path::canonicalize_tag;
    use crate::reducer::scope_from_room_wire;
    use std::collections::HashSet;

    let open_urls: HashSet<String> = open_children
        .iter()
        .filter_map(|c| normalized_child_url(&c.url))
        .collect();

    let thread_tag = resolver_thread_tag(issues_parent);
    let scope = scope_from_room_wire(room);
    let tag = canonicalize_tag(&thread_tag);

    let mut to_redact: Vec<String> = Vec::new();
    let mut covered: HashSet<String> = HashSet::new();
    {
        let reduced = state.reduced.read().await;
        let ids = reduced
            .ingests_by_scope_thread
            .get(&(scope, tag))
            .cloned()
            .unwrap_or_default();
        for id in ids {
            if reduced.redacted_posts.contains(&id) {
                continue;
            }
            let Some(ing) = reduced.ingests_by_id.get(&id) else {
                continue;
            };
            if ing.principal != GITHUB_SYSTEM_PRINCIPAL {
                continue;
            }
            let declared = child_urls_declared_in_ingest(&ing.raw, issues_parent);
            if declared.is_empty() {
                continue;
            }
            let stale = declared.iter().any(|u| !open_urls.contains(u));
            let bulk = declared.len() > 1;
            if stale || bulk {
                to_redact.push(id);
            } else {
                covered.insert(declared[0].clone());
            }
        }
    }

    let mut deleted = 0usize;
    for post_id in &to_redact {
        system_redact(state, post_id, GITHUB_SYSTEM_PRINCIPAL).await?;
        deleted += 1;
    }

    let mut imported = 0usize;
    let mut kept = 0usize;
    for child in &open_children {
        let Some(url) = normalized_child_url(&child.url) else {
            continue;
        };
        if covered.contains(&url) {
            kept += 1;
            continue;
        }
        system_ingest(
            state,
            room,
            &thread_tag,
            child_to_dsl(child),
            GITHUB_SYSTEM_PRINCIPAL,
        )
        .await?;
        imported += 1;
    }

    Ok(ResolveStats {
        imported,
        deleted,
        kept,
    })
}

pub async fn resolve_github_children(
    state: &AppState,
    room: &str,
    item: &ItemId,
) -> Result<ResolveStats, String> {
    if !state.github_resolver.can_resolve_children(item) {
        return Err("no GitHub resolver for this item".to_string());
    }

    let key = format!("github:{}:{}", room.trim(), item.as_str());
    let now = now_ms();
    {
        let mut runs = state.resolver_runs.write().await;
        if let Some(last) = runs.get(&key) {
            let remaining = github_cooldown_ms() - (now - *last);
            if remaining > 0 {
                return Err(format!(
                    "GitHub resolver cooldown: try again in {}s",
                    (remaining + 999) / 1000
                ));
            }
        }
        runs.insert(key, now);
    }

    let segments = github_segments(item).ok_or_else(|| "not a GitHub URL".to_string())?;
    let is_issues = matches!(segments.as_slice(), [_, _, section] if section == "issues");

    if is_issues {
        let children = state.github_resolver.list_children(item).await?;
        return resolve_github_issues(state, room, item, children).await;
    }

    let children = state.github_resolver.list_children(item).await?;
    if children.is_empty() {
        return Ok(ResolveStats::default());
    }
    let text = children_to_dsl(&children);
    let thread_tag = resolver_thread_tag(item);
    system_ingest(state, room, &thread_tag, text, GITHUB_SYSTEM_PRINCIPAL).await?;
    Ok(ResolveStats {
        imported: children.len(),
        deleted: 0,
        kept: 0,
    })
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
        card.sublines.push(format!("Labels: {}", labels.join(", ")));
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

fn parse_github_import_from_body(body: &str) -> Option<GithubImportCard> {
    let payload = extract_fence(body.trim(), "slug-github-card")?;
    let c = decode_github_card_payload(payload)?;
    (c.v == 1 && (c.schema.is_empty() || c.schema == SLUG_GITHUB_SCHEMA)).then_some(c)
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
        article.import-card {
            header.import-card__hdr {
                span class="import-card__badge" { (kind_badge(&card.kind)) }
                h3.import-card__title { (card.headline.as_str()) }
            }
            @if !card.sublines.is_empty() {
                ul.import-card__meta {
                    @for line in &card.sublines {
                        li { (line.as_str()) }
                    }
                }
            }
            @if let Some(ex) = &card.excerpt {
                div.import-card__excerpt {
                    @for block in ex.split("\n\n") {
                        @if !block.trim().is_empty() {
                            p { (block) }
                        }
                    }
                }
            }
            p.import-card__link {
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
    fn resolver_thread_tag_is_one_per_repo() {
        let repo = ItemId::parse("https://github.com/berriai/litellm").unwrap();
        let issues = ItemId::parse("https://github.com/berriai/litellm/issues").unwrap();
        let issue = ItemId::parse("https://github.com/berriai/litellm/issues/1").unwrap();
        let pulls = ItemId::parse("https://github.com/berriai/litellm/pulls").unwrap();
        let expected = "import:https:::github.com:berriai:litellm";
        assert_eq!(resolver_thread_tag(&repo), expected);
        assert_eq!(resolver_thread_tag(&issues), expected);
        assert_eq!(resolver_thread_tag(&issue), expected);
        assert_eq!(resolver_thread_tag(&pulls), expected);
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
        // Payload is base64 so nested ``` in excerpts cannot break DSL fences.
        assert!(!dsl.contains("\"schema\":\"slug_github_import\""));
        let body = dsl
            .split_once('{')
            .and_then(|(_, rest)| rest.rsplit_once('}'))
            .map(|(inner, _)| inner.trim())
            .expect("braced body");
        let parsed = parse_github_import_from_body(body).expect("decodes base64 card");
        assert_eq!(parsed.schema, SLUG_GITHUB_SCHEMA);
    }

    #[test]
    fn child_to_dsl_is_single_item_post() {
        let dsl = child_to_dsl(&ResolvedChild {
            url: "https://github.com/o/r/issues/7".into(),
            title: "#7 alone".into(),
            card: GithubImportCard::new(
                GithubImportKind::Issue,
                "https://github.com/o/r/issues/7".into(),
                "#7 alone".into(),
            ),
        });
        assert!(dsl.starts_with("https://github.com/o/r/issues/7 {"));
        assert!(!dsl.contains("issues/8"));
        assert_eq!(dsl.matches("```slug-github-card").count(), 1);
    }

    #[test]
    fn child_urls_declared_reads_single_and_bulk_posts() {
        let parent = ItemId::parse("https://github.com/o/r/issues").unwrap();
        let single = child_to_dsl(&ResolvedChild {
            url: "https://github.com/o/r/issues/1".into(),
            title: "#1".into(),
            card: GithubImportCard::new(
                GithubImportKind::Issue,
                "https://github.com/o/r/issues/1".into(),
                "#1".into(),
            ),
        });
        assert_eq!(
            child_urls_declared_in_ingest(&single, &parent),
            vec!["https://github.com/o/r/issues/1".to_string()]
        );

        let bulk = "https://github.com/o/r/issues/1 {a}\nhttps://github.com/o/r/issues/2 {b}\n";
        assert_eq!(
            child_urls_declared_in_ingest(bulk, &parent),
            vec![
                "https://github.com/o/r/issues/1".to_string(),
                "https://github.com/o/r/issues/2".to_string()
            ]
        );
    }

    #[test]
    fn parse_accepts_slug_github_fence() {
        let card = GithubImportCard::new(
            GithubImportKind::Repo,
            "https://github.com/o/r".into(),
            "o/r".into(),
        );
        let body = format!(
            "```slug-github-card\n{}\n```\n",
            card_payload_for_dsl_fence(&card)
        );
        let parsed = parse_github_import_from_body(&body).expect("parses");
        assert_eq!(parsed, card);
    }

    #[test]
    fn parse_rejects_raw_json_slug_github_fence() {
        let card = GithubImportCard::new(
            GithubImportKind::Issue,
            "https://github.com/o/r/issues/2".into(),
            "#2 hi".into(),
        );
        let body = format!(
            "```slug-github-card\n{}\n```",
            serde_json::to_string(&card).unwrap()
        );
        assert!(
            parse_github_import_from_body(&body).is_none(),
            "raw JSON inside slug-github-card is not accepted"
        );
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
        assert_eq!(card.excerpt.as_deref(), Some("The issue body."));
    }

    #[test]
    fn child_to_dsl_validates_as_ingest() {
        let child = ResolvedChild {
            url: "https://github.com/octo/hello/issues/43".into(),
            title: "#43 Imported sibling".into(),
            card: card_for_issue(
                &serde_json::json!({
                    "number": 43,
                    "title": "Imported sibling",
                    "state": "open",
                    "user": {"login": "octo"},
                    "body": "Loaded from mock GitHub",
                    "labels": []
                }),
                "https://github.com/octo/hello/issues/43",
                GithubImportKind::Issue,
            ),
        };
        let text = child_to_dsl(&child);
        let reduced = crate::reducer::ReducerState::default();
        crate::api::validate_ingest_document(&reduced, &text, &crate::reducer::ScopeId::Public)
            .expect("single issue card should validate");
    }

    fn assert_child_dsl_ingests_cleanly(issue_body: &str) {
        let child = ResolvedChild {
            url: "https://github.com/berriai/litellm/issues/1".into(),
            title: "#1 repro".into(),
            card: card_for_issue(
                &serde_json::json!({
                    "number": 1,
                    "title": "repro",
                    "state": "open",
                    "user": {"login": "octo"},
                    "labels": [],
                    "body": issue_body
                }),
                "https://github.com/berriai/litellm/issues/1",
                GithubImportKind::Issue,
            ),
        };
        let text = child_to_dsl(&child);
        let reduced = crate::reducer::ReducerState::default();
        let validated =
            crate::api::validate_ingest_document(&reduced, &text, &crate::reducer::ScopeId::Public)
                .unwrap_or_else(|(code, msg, hint)| {
                    panic!(
                "github import DSL should validate; got {code} {msg} hint={hint:?}\nDSL:\n{text}"
            )
                });
        let item_body = validated
            .doc
            .statements
            .iter()
            .find_map(|s| match s {
                crate::dsl::Stmt::Item { body: Some(b), .. } => Some(b.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected item body\nDSL:\n{text}"));
        let parsed = parse_github_import_from_body(item_body).unwrap_or_else(|| {
            panic!("body should round-trip as github card; body was:\n{item_body}\nDSL:\n{text}")
        });
        assert!(
            child
                .card
                .excerpt
                .as_ref()
                .is_some_and(|e| e.contains("```")),
            "precondition: card excerpt contains markdown fences"
        );
        assert_eq!(
            parsed.excerpt.as_deref(),
            child.card.excerpt.as_deref(),
            "excerpt should survive DSL fence masking/unmasking"
        );
    }

    #[test]
    fn child_to_dsl_validates_when_issue_body_contains_balanced_markdown_fences() {
        // Existing happy-path tests only used plain excerpts, so fence-bearing
        // GitHub markdown was never exercised end-to-end through ingest validation.
        assert_child_dsl_ingests_cleanly(concat!(
            "Prefer A > B when ranking.\n\n",
            "```python\n",
            "print('hi')\n",
            "```\n\n",
            "Closing thoughts."
        ));
    }

    #[test]
    fn child_to_dsl_validates_when_issue_body_has_truncated_markdown_fence() {
        // Common in the wild: issue opens a ```json/py fence and either never
        // closes it, or our 1200-char excerpt cuts off before the closer.
        // Nested ``` inside a toggle fence would break masking if the card JSON
        // were stored raw; base64 payload keeps the outer fence intact.
        assert_child_dsl_ingests_cleanly(concat!(
            "## Describe the bug\n\n",
            "Using a skill with:\n\n",
            "```json\n",
            "\"container\": {\n",
            "  \"skills\": [{\"type\": \"custom\", \"skill_id\": \"x\"}]\n",
        ));
    }

    #[test]
    fn children_to_dsl_validates_when_earlier_issue_body_has_markdown_fence() {
        // Bulk ingest path (also a good stress test for fence leakage across
        // concatenated items). A ``` inside issue 1's card must not make issue 2
        // parse as a bare comparison / vote.
        let kids = [
            ResolvedChild {
                url: "https://github.com/berriai/litellm/issues/1".into(),
                title: "#1".into(),
                card: card_for_issue(
                    &serde_json::json!({
                        "number": 1,
                        "title": "one",
                        "state": "open",
                        "user": {"login": "octo"},
                        "labels": [],
                        "body": "## bug\n\n```json\n{\"a\":1}\n"
                    }),
                    "https://github.com/berriai/litellm/issues/1",
                    GithubImportKind::Issue,
                ),
            },
            ResolvedChild {
                url: "https://github.com/berriai/litellm/issues/2".into(),
                title: "#2".into(),
                card: card_for_issue(
                    &serde_json::json!({
                        "number": 2,
                        "title": "two",
                        "state": "open",
                        "user": {"login": "octo"},
                        "labels": [],
                        "body": "plain body"
                    }),
                    "https://github.com/berriai/litellm/issues/2",
                    GithubImportKind::Issue,
                ),
            },
        ];
        let text = children_to_dsl(&kids);
        let reduced = crate::reducer::ReducerState::default();
        crate::api::validate_ingest_document(&reduced, &text, &crate::reducer::ScopeId::Public)
            .unwrap_or_else(|(code, msg, hint)| {
                panic!(
                    "bulk github import DSL should validate; got {code} {msg} hint={hint:?}\nDSL:\n{text}"
                )
            });
    }

    #[tokio::test]
    async fn list_issues_pages_until_exhausted() {
        use axum::{routing::get, Json, Router};
        use tokio::sync::oneshot;

        let (tx, rx) = oneshot::channel::<()>();
        let app = Router::new().route(
            "/repos/o/r/issues",
            get(|| async {
                Json(serde_json::json!([
                    {"number": 1, "title": "One", "state": "open", "user": {"login": "a"}, "body": "b"},
                    {"number": 2, "title": "Two", "state": "open", "user": {"login": "a"}, "body": "b"},
                    {"number": 3, "title": "PR", "pull_request": {}, "state": "open"}
                ]))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        let resolver = GitHubResolver {
            client: reqwest::Client::new(),
            api_base_url: format!("http://{addr}"),
            token: None,
        };
        let kids = resolver.list_issues("o", "r").await.unwrap();
        assert_eq!(kids.len(), 2);
        assert!(kids.iter().any(|k| k.url.ends_with("/issues/1")));
        assert!(kids.iter().any(|k| k.url.ends_with("/issues/2")));
        let _ = tx.send(());
        let _ = server.await;
    }
}
