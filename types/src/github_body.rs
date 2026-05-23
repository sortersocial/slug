//! Compact text summaries for GitHub import bodies (CLI and other non-HTML surfaces).

use serde::Deserialize;

const SLUG_GITHUB_SCHEMA: &str = "slug_github_import";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GithubImportKind {
    Repo,
    RepoSection,
    Issue,
    Pull,
    Commit,
    Release,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubImportCard {
    v: u32,
    #[serde(default)]
    schema: String,
    kind: GithubImportKind,
    url: String,
    headline: String,
    #[serde(default)]
    sublines: Vec<String>,
    #[serde(default)]
    excerpt: Option<String>,
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

fn parse_github_card(body: &str) -> Option<GithubImportCard> {
    let trimmed = body.trim();
    if let Some(json) = extract_fence(trimmed, "slug-github-card") {
        let c: GithubImportCard = serde_json::from_str(json).ok()?;
        return (c.v == 1 && (c.schema.is_empty() || c.schema == SLUG_GITHUB_SCHEMA)).then_some(c);
    }
    if let Some(json) = extract_fence(trimmed, "json") {
        let c: GithubImportCard = serde_json::from_str(json).ok()?;
        if c.v == 1
            && (c.schema == SLUG_GITHUB_SCHEMA
                || (c.schema.is_empty() && c.url.contains("github.com")))
        {
            return Some(c);
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

fn format_github_card(card: &GithubImportCard) -> String {
    let mut out = card.headline.clone();
    if !card.sublines.is_empty() {
        out.push('\n');
        for line in &card.sublines {
            out.push_str(line);
            out.push('\n');
        }
    }
    if let Some(ex) = &card.excerpt {
        if !ex.trim().is_empty() {
            out.push('\n');
            out.push_str(ex.trim());
        }
    }
    out.trim().to_string()
}

fn summarize_raw_github_api_json(value: &serde_json::Value) -> Option<String> {
    let title = value.get("title").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let state = value.get("state").and_then(|v| v.as_str());
    let body = value.get("body").and_then(|v| v.as_str());
    let number = value.get("number").and_then(|v| v.as_i64());
    if title.is_none() && body.is_none() {
        return None;
    }
    let headline = match (number, title) {
        (Some(n), Some(t)) => format!("#{n} {t}"),
        (_, Some(t)) => t.to_string(),
        _ => return None,
    };
    let mut out = headline;
    if let Some(st) = state {
        out.push_str(&format!("\nState: {st}"));
    }
    if let Some(user) = value.get("user").and_then(|u| u.get("login")).and_then(|v| v.as_str()) {
        out.push_str(&format!("\nAuthor: @{user}"));
    }
    if let Some(b) = body.filter(|s| !s.trim().is_empty()) {
        out.push_str("\n\n");
        out.push_str(b.trim());
    }
    Some(out)
}

/// If `raw` looks like a GitHub import card or API JSON blob, return a compact human-readable summary.
pub fn compact_github_item_body(raw: &str) -> Option<String> {
    if let Some(card) = parse_github_card(raw) {
        return Some(format_github_card(&card));
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.trim()) {
        if let Some(summary) = summarize_raw_github_api_json(&value) {
            return Some(summary);
        }
    }
    if let Some(json) = extract_fence(raw.trim(), "json") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json) {
            return summarize_raw_github_api_json(&value);
        }
    }
    None
}

/// Prefer a compact GitHub summary when the stored body is import/API JSON; otherwise return `raw`.
pub fn compact_item_body_for_display(raw: &str) -> String {
    compact_github_item_body(raw).unwrap_or_else(|| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_slug_github_card_fence() {
        let body = "```slug-github-card\n\
{\"v\":1,\"schema\":\"slug_github_import\",\"kind\":\"issue\",\
\"url\":\"https://github.com/o/r/issues/1\",\"headline\":\"#1 Title\",\
\"sublines\":[\"State: open\"],\"excerpt\":\"Issue text.\"}\n\
```";
        let out = compact_github_item_body(body).expect("parses");
        assert!(out.contains("#1 Title"));
        assert!(out.contains("State: open"));
        assert!(out.contains("Issue text."));
    }

    #[test]
    fn compact_raw_github_api_issue_json() {
        let body = r#"{"number":42,"title":"Ranking history","state":"open","body":"The real description.","user":{"login":"octo"}}"#;
        let out = compact_github_item_body(body).expect("parses");
        assert!(out.contains("#42 Ranking history"));
        assert!(out.contains("The real description."));
    }
}
