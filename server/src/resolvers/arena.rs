//! Are.na resolver: imports channel contents (blocks + nested channels) and user
//! profiles (their channels) as garden items.
//!
//! Unlike GitHub issues, are.na block URLs (`/block/:id`) are not path-children of the
//! channel URL, so membership is emitted as an explicit containment claim
//! (`block <: channel`) alongside the card body. Blocks keep cross-channel identity:
//! the same block connected in two imported channels is one item with two scopes.
//! Legacy `/:user/:channel` URLs resolve onto the canonical `/channel/:slug` item.

use async_trait::async_trait;
use maud::html;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    extract_fence, now_ms, resolver_cooldown_ms, system_ingest, system_redact, ExternalResolver,
    ResolveStats,
};
use crate::{path_types::ItemId, state::AppState};

pub const SLUG_ARENA_SCHEMA: &str = "slug_arena_import";

const ARENA_SYSTEM_PRINCIPAL: &str = "system:arena-resolver";
const ARENA_RESOLVER_COOLDOWN_MS_DEFAULT: i64 = 15_000;
/// Safety valve while paging channel contents (`per=100` → up to 2500 entries).
const ARENA_MAX_PAGES: usize = 25;

fn arena_cooldown_ms() -> i64 {
    resolver_cooldown_ms(
        "SLUG_ARENA_RESOLVER_COOLDOWN_MS",
        ARENA_RESOLVER_COOLDOWN_MS_DEFAULT,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArenaImportKind {
    Channel,
    User,
    Text,
    Image,
    Link,
    Media,
    Attachment,
    Embed,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArenaImportCard {
    pub v: u32,
    #[serde(default)]
    pub schema: String,
    pub kind: ArenaImportKind,
    pub url: String,
    pub headline: String,
    #[serde(default)]
    pub sublines: Vec<String>,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
}

impl ArenaImportCard {
    fn new(kind: ArenaImportKind, url: String, headline: String) -> Self {
        Self {
            v: 1,
            schema: SLUG_ARENA_SCHEMA.to_string(),
            kind,
            url,
            headline,
            sublines: Vec::new(),
            excerpt: None,
            image_url: None,
            source_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArenaChild {
    pub url: String,
    pub title: String,
    pub card: ArenaImportCard,
}

#[derive(Clone)]
pub struct ArenaResolver {
    client: reqwest::Client,
    api_base_url: String,
    token: Option<String>,
}

impl ArenaResolver {
    pub fn from_env() -> Self {
        let api_base_url = std::env::var("SLUG_ARENA_API_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "https://api.are.na".to_string());
        let token = std::env::var("SLUG_ARENA_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Self {
            client: reqwest::Client::new(),
            api_base_url: api_base_url.trim_end_matches('/').to_string(),
            token,
        }
    }

    pub fn can_resolve_children(&self, item: &ItemId) -> bool {
        arena_target(item).is_some()
    }

    pub async fn list_children(&self, item: &ItemId) -> Result<Vec<ArenaChild>, String> {
        match arena_target(item).ok_or_else(|| "not an are.na channel or user URL".to_string())? {
            ArenaTarget::Channel(slug) => self.list_channel_contents(&slug).await,
            ArenaTarget::User(user) => self.list_user_channels(&user).await,
        }
    }

    async fn get_json(&self, path: &str) -> Result<Value, String> {
        let url = format!("{}/{}", self.api_base_url, path.trim_start_matches('/'));
        let mut req = self
            .client
            .get(url)
            .header(reqwest::header::USER_AGENT, "slugsocial-arena-resolver");
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("are.na request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("are.na request returned {status}"));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| format!("are.na response JSON failed: {e}"))
    }

    async fn list_channel_contents(&self, slug: &str) -> Result<Vec<ArenaChild>, String> {
        self.collect_paged(&format!("/v3/channels/{slug}/contents"), false)
            .await
    }

    /// A user's channels via `/v3/users/:id/contents` (non-channel entries skipped).
    async fn list_user_channels(&self, user: &str) -> Result<Vec<ArenaChild>, String> {
        self.collect_paged(&format!("/v3/users/{user}/contents"), true)
            .await
    }

    async fn collect_paged(
        &self,
        path: &str,
        channels_only: bool,
    ) -> Result<Vec<ArenaChild>, String> {
        let mut out = Vec::new();
        for page in 1..=ARENA_MAX_PAGES {
            let value = self
                .get_json(&format!("{path}?per=100&page={page}"))
                .await?;
            let arr = value
                .get("data")
                .and_then(|d| d.as_array())
                .ok_or_else(|| "are.na response missing data array".to_string())?;
            for entry in arr {
                if channels_only && entry.get("type").and_then(|v| v.as_str()) != Some("Channel") {
                    continue;
                }
                if let Some(child) = arena_child_from_entry(entry) {
                    out.push(child);
                }
            }
            let has_more = value
                .get("meta")
                .and_then(|m| m.get("has_more_pages"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !has_more {
                break;
            }
            if page == ARENA_MAX_PAGES {
                return Err(format!(
                    "are.na list truncated after {ARENA_MAX_PAGES} pages ({} items); refine scope or raise page cap",
                    out.len()
                ));
            }
        }
        out.sort_by(|a, b| a.url.cmp(&b.url));
        Ok(out)
    }
}

fn arena_segments(item: &ItemId) -> Option<Vec<String>> {
    let url = url::Url::parse(item.as_str()).ok()?;
    let host = url.host_str()?;
    if host.eq_ignore_ascii_case("www.are.na") || host.eq_ignore_ascii_case("are.na") {
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

/// Are.na site sections that must not be mistaken for a user slug in
/// `/:user/...` URLs.
const ARENA_RESERVED_SEGMENTS: &[&str] = &[
    "about",
    "api",
    "blog",
    "block",
    "channel",
    "developers",
    "explore",
    "gift",
    "getting-started",
    "log-in",
    "login",
    "premium",
    "search",
    "settings",
    "sign-up",
    "signup",
    "tools",
];

/// What a pasted are.na URL points at. Channel slugs are globally unique, so
/// the legacy `/:user/:channel` form names the same channel as
/// `/channel/:slug` (are.na itself redirects the legacy form).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ArenaTarget {
    Channel(String),
    User(String),
}

fn arena_target(item: &ItemId) -> Option<ArenaTarget> {
    let segs = arena_segments(item)?;
    match segs.as_slice() {
        [first, slug] if first == "channel" && !slug.is_empty() => {
            Some(ArenaTarget::Channel(slug.clone()))
        }
        [user, slug] if !slug.is_empty() && !ARENA_RESERVED_SEGMENTS.contains(&user.as_str()) => {
            Some(ArenaTarget::Channel(slug.clone()))
        }
        [user] if !ARENA_RESERVED_SEGMENTS.contains(&user.as_str()) => {
            Some(ArenaTarget::User(user.clone()))
        }
        _ => None,
    }
}

fn canonical_channel_item(slug: &str) -> ItemId {
    ItemId::parse(&format!("https://www.are.na/channel/{slug}"))
        .expect("channel slug came from a parsed URL")
        .normalized_storage()
}

/// Item the resolver attaches children to: collapses legacy `/:user/:channel`
/// URLs onto the canonical `/channel/:slug` item so one channel is one item
/// no matter which URL form was pasted.
pub fn canonical_arena_item(item: &ItemId) -> Option<ItemId> {
    match arena_target(item)? {
        ArenaTarget::Channel(slug) => Some(canonical_channel_item(&slug)),
        ArenaTarget::User(_) => Some(item.clone().normalized_storage()),
    }
}

/// Stable import thread for an are.na URL: one thread per channel or per user.
fn resolver_thread_tag(item: &ItemId) -> String {
    let path = match arena_target(item) {
        Some(ArenaTarget::Channel(slug)) => format!("https://www.are.na/channel/{slug}"),
        _ => item.display_path().trim_start_matches("-/").to_string(),
    };
    let tail = path.replace(['/', '?'], ":");
    format!("import:{tail}")
}

fn arena_kind_from_class(class: &str) -> ArenaImportKind {
    match class {
        "Text" => ArenaImportKind::Text,
        "Image" => ArenaImportKind::Image,
        "Link" => ArenaImportKind::Link,
        "Media" => ArenaImportKind::Media,
        "Attachment" => ArenaImportKind::Attachment,
        "Embed" => ArenaImportKind::Embed,
        _ => ArenaImportKind::Block,
    }
}

fn arena_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn arena_rich_text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(|d| d.get("markdown"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn excerpt_from_arena_text(text: Option<&str>) -> Option<String> {
    let t = text?.trim();
    if t.is_empty() {
        return None;
    }
    let max = 1200usize;
    if t.len() <= max {
        Some(t.to_string())
    } else {
        Some(format!("{}…", t.chars().take(max).collect::<String>()))
    }
}

fn arena_child_from_entry(entry: &Value) -> Option<ArenaChild> {
    let class = entry.get("type").and_then(|v| v.as_str())?;
    if class == "Channel" {
        let slug = arena_str(entry, "slug")?;
        let url = format!("https://www.are.na/channel/{slug}");
        let title = arena_str(entry, "title").unwrap_or(slug);
        let mut card =
            ArenaImportCard::new(ArenaImportKind::Channel, url.clone(), title.to_string());
        if let Some(n) = entry
            .get("counts")
            .and_then(|c| c.get("contents"))
            .and_then(|v| v.as_i64())
        {
            card.sublines.push(format!("Contents: {n}"));
        }
        card.excerpt = excerpt_from_arena_text(arena_rich_text(entry, "description"));
        return Some(ArenaChild {
            url,
            title: title.to_string(),
            card,
        });
    }

    let id = entry.get("id").and_then(|v| v.as_i64())?;
    let url = format!("https://www.are.na/block/{id}");
    let content_md = arena_rich_text(entry, "content");
    let title = arena_str(entry, "title").map(|s| s.to_string());
    let headline = title
        .clone()
        .or_else(|| {
            content_md.and_then(|m| {
                let first = m.lines().next()?.trim();
                if first.is_empty() {
                    None
                } else {
                    let max = 80usize;
                    Some(if first.len() <= max {
                        first.to_string()
                    } else {
                        format!("{}…", first.chars().take(max).collect::<String>())
                    })
                }
            })
        })
        .unwrap_or_else(|| format!("{class} block {id}"));

    let mut card =
        ArenaImportCard::new(arena_kind_from_class(class), url.clone(), headline.clone());
    card.sublines.push(format!("Class: {class}"));
    if let Some(name) = entry
        .get("user")
        .and_then(|u| u.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        card.sublines.push(format!("By: {name}"));
    }
    if let Some(name) = entry
        .get("connection")
        .and_then(|c| c.get("connected_by"))
        .and_then(|u| u.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        card.sublines.push(format!("Connected by: {name}"));
    }
    if let Some(connected_at) = entry
        .get("connection")
        .and_then(|c| c.get("connected_at"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        card.sublines.push(format!("Connected: {connected_at}"));
    }
    if let Some(source) = entry
        .get("source")
        .and_then(|s| s.get("url"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        card.source_url = Some(source.to_string());
        if let Some(host) = url::Url::parse(source)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
        {
            card.sublines.push(format!("Source: {host}"));
        }
    }
    card.image_url = entry
        .get("image")
        .and_then(|i| {
            i.get("medium")
                .and_then(|m| m.get("src"))
                .or_else(|| i.get("src"))
        })
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());
    // Text blocks carry their payload in `content`; other classes in `description`.
    card.excerpt =
        excerpt_from_arena_text(content_md.or_else(|| arena_rich_text(entry, "description")));

    Some(ArenaChild {
        url,
        title: headline,
        card,
    })
}

/// Encode a card for a ```slug-arena-card``` fence (base64 for the same toggle-fence
/// reason as GitHub cards: are.na text blocks are markdown and may contain ```).
fn card_payload_for_dsl_fence(card: &ArenaImportCard) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let json = serde_json::to_string(card).unwrap_or_else(|_| "{}".to_string());
    STANDARD.encode(json.as_bytes())
}

fn decode_arena_card_payload(payload: &str) -> Option<ArenaImportCard> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let bytes = STANDARD.decode(payload.trim()).ok()?;
    let s = std::str::from_utf8(&bytes).ok()?;
    serde_json::from_str(s).ok()
}

/// Braces/quotes/newlines would break the leading `{ … }` explanation block.
fn sanitize_explanation_text(raw: &str) -> String {
    raw.replace(['{', '}'], "(")
        .replace('"', "'")
        .replace(['\n', '\r'], " ")
}

fn child_to_dsl(parent: &ItemId, child: &ArenaChild) -> String {
    let payload = card_payload_for_dsl_fence(&child.card);
    let inner = format!("```slug-arena-card\n{payload}\n```");
    let parent_url = parent.clone().normalized_storage();
    let label = sanitize_explanation_text(parent_url.as_str());
    // Item body first: validation requires containment sides to be defined
    // (here or previously) before the claim. The parent side is guaranteed by
    // `ensure_scope_body`, not by re-declaring it here — re-declaring would
    // clobber a user-written parent body on every child import.
    format!(
        "{child_url} {{\n{inner}\n}}\n\n{{ Connected in are.na {label}. }}\n{child_url} <: {parent_url}\n",
        child_url = child.url,
        parent_url = parent_url.as_str(),
    )
}

/// Parent items (channel or user profile) are usually created (with a body)
/// when the user pastes the URL. When the parent only exists as a ghost,
/// define it once so the containment claims validate — and so the parent page
/// renders a rich card.
async fn ensure_scope_body(
    state: &AppState,
    room: &str,
    thread_tag: &str,
    parent: &ItemId,
    kind: ArenaImportKind,
    headline: String,
) -> Result<(), String> {
    use crate::reducer::scope_from_room_wire;

    let needs_body = {
        let reduced = state.reduced.read().await;
        let scope = scope_from_room_wire(room);
        let in_room = reduced
            .content_for_scope(&scope)
            .map(|c| c.item_bodies.contains_key(parent))
            .unwrap_or(false);
        let in_public = reduced.public().item_bodies.contains_key(parent);
        !in_room && !in_public
    };
    if !needs_body {
        return Ok(());
    }
    let card = ArenaImportCard::new(kind, parent.as_str().to_string(), headline);
    let payload = card_payload_for_dsl_fence(&card);
    let text = format!(
        "{url} {{\n```slug-arena-card\n{payload}\n```\n}}\n",
        url = parent.as_str()
    );
    system_ingest(state, room, thread_tag, text, ARENA_SYSTEM_PRINCIPAL).await
}

/// Child URLs declared via containment claims into `parent` within one ingest body.
fn arena_urls_declared_in_ingest(raw: &str, parent: &ItemId) -> Vec<String> {
    let Ok(doc) = crate::dsl::parse_full(raw) else {
        return Vec::new();
    };
    let parent = parent.clone().normalized_storage();
    let mut out = Vec::new();
    for stmt in doc.statements {
        let crate::dsl::Stmt::Containment {
            child,
            parent: p,
            border,
            ..
        } = stmt
        else {
            continue;
        };
        if border {
            continue;
        }
        let Some(child_id) = ItemId::parse(&child).map(|i| i.normalized_storage()) else {
            continue;
        };
        let Some(p_id) = ItemId::parse(&p).map(|i| i.normalized_storage()) else {
            continue;
        };
        if p_id == parent {
            out.push(child_id.as_str().to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// One post per entry; redact posts for entries removed from the parent scope
/// (and legacy multi-entry bulk posts).
async fn resolve_arena_scope_contents(
    state: &AppState,
    room: &str,
    parent: &ItemId,
    children: Vec<ArenaChild>,
) -> Result<ResolveStats, String> {
    use crate::canonical_path::canonicalize_tag;
    use crate::reducer::scope_from_room_wire;
    use std::collections::HashSet;

    let live_urls: HashSet<String> = children
        .iter()
        .filter_map(|c| super::normalized_child_url(&c.url))
        .collect();

    let thread_tag = resolver_thread_tag(parent);
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
            if ing.principal != ARENA_SYSTEM_PRINCIPAL {
                continue;
            }
            let declared = arena_urls_declared_in_ingest(&ing.raw, parent);
            if declared.is_empty() {
                continue;
            }
            let stale = declared.iter().any(|u| !live_urls.contains(u));
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
        system_redact(state, post_id, ARENA_SYSTEM_PRINCIPAL).await?;
        deleted += 1;
    }

    let mut imported = 0usize;
    let mut kept = 0usize;
    for child in &children {
        let Some(url) = super::normalized_child_url(&child.url) else {
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
            child_to_dsl(parent, child),
            ARENA_SYSTEM_PRINCIPAL,
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

pub async fn resolve_arena_children(
    state: &AppState,
    room: &str,
    item: &ItemId,
) -> Result<ResolveStats, String> {
    let Some(target) = arena_target(item) else {
        return Err(
            "are.na resolver handles channel URLs (https://www.are.na/channel/…) and user profiles (https://www.are.na/:user) only"
                .to_string(),
        );
    };

    let key = format!("arena:{}:{}", room.trim(), item.as_str());
    let now = now_ms();
    {
        let mut runs = state.resolver_runs.write().await;
        if let Some(last) = runs.get(&key) {
            let remaining = arena_cooldown_ms() - (now - *last);
            if remaining > 0 {
                return Err(format!(
                    "are.na resolver cooldown: try again in {}s",
                    (remaining + 999) / 1000
                ));
            }
        }
        runs.insert(key, now);
    }

    let parent = canonical_arena_item(item).expect("arena_target matched");
    let children = state.arena_resolver.list_children(item).await?;
    let thread_tag = resolver_thread_tag(&parent);
    let (kind, headline) = match &target {
        ArenaTarget::Channel(slug) => (ArenaImportKind::Channel, slug.clone()),
        ArenaTarget::User(user) => (ArenaImportKind::User, user.clone()),
    };
    ensure_scope_body(state, room, &thread_tag, &parent, kind, headline).await?;
    resolve_arena_scope_contents(state, room, &parent, children).await
}

fn parse_arena_import_from_body(body: &str) -> Option<ArenaImportCard> {
    let payload = extract_fence(body.trim(), "slug-arena-card")?;
    let c = decode_arena_card_payload(payload)?;
    (c.v == 1 && (c.schema.is_empty() || c.schema == SLUG_ARENA_SCHEMA)).then_some(c)
}

fn kind_badge(kind: &ArenaImportKind) -> &'static str {
    match kind {
        ArenaImportKind::Channel => "Are.na · channel",
        ArenaImportKind::User => "Are.na · user",
        ArenaImportKind::Text => "Are.na · text",
        ArenaImportKind::Image => "Are.na · image",
        ArenaImportKind::Link => "Are.na · link",
        ArenaImportKind::Media => "Are.na · media",
        ArenaImportKind::Attachment => "Are.na · attachment",
        ArenaImportKind::Embed => "Are.na · embed",
        ArenaImportKind::Block => "Are.na · block",
    }
}

fn render_arena_card(card: &ArenaImportCard) -> maud::Markup {
    html! {
        article.import-card.arena-import-card {
            header.import-card__hdr {
                span class="import-card__badge" { (kind_badge(&card.kind)) }
                h3.import-card__title { (card.headline.as_str()) }
            }
            @if let Some(img) = &card.image_url {
                p.import-card__image {
                    a href=(card.url.as_str()) rel="noopener noreferrer" target="_blank" {
                        img src=(img.as_str()) alt=(card.headline.as_str()) loading="lazy";
                    }
                }
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
                    "Open on Are.na"
                }
                @if let Some(src) = &card.source_url {
                    " · "
                    a href=(src.as_str()) rel="noopener noreferrer" target="_blank" {
                        "Source"
                    }
                }
            }
        }
    }
}

/// Rich HTML for bodies that contain an [`ArenaImportCard`] fence.
pub fn try_render_arena_import_markup(raw: &str) -> Option<maud::Markup> {
    let card = parse_arena_import_from_body(raw)?;
    Some(render_arena_card(&card))
}

#[async_trait]
impl ExternalResolver for ArenaResolver {
    fn domain_match(&self) -> &'static str {
        "are.na"
    }

    fn normalize(&self, path: &str) -> String {
        path.to_string()
    }

    async fn fetch_body(&self, _item: &ItemId) -> Result<String, String> {
        Err("are.na fetch_body not implemented".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_item() -> ItemId {
        ItemId::parse("https://www.are.na/channel/arena-influences").unwrap()
    }

    fn block_child(id: i64, class: &str) -> ArenaChild {
        let entry = serde_json::json!({
            "id": id,
            "type": class,
            "title": null,
            "content": (class == "Text").then(|| serde_json::json!({"markdown": "quote body"})),
            "description": null,
            "user": {"name": "Ada"},
            "connection": {"connected_by": {"name": "Charles"}, "connected_at": "2024-01-01T00:00:00Z"},
        });
        arena_child_from_entry(&entry).expect("block entry maps to child")
    }

    #[test]
    fn arena_target_parses_channel_urls() {
        let item = ItemId::parse("https://www.are.na/channel/arena-influences").unwrap();
        assert_eq!(
            arena_segments(&item),
            Some(vec!["channel".to_string(), "arena-influences".to_string()])
        );
        assert_eq!(
            arena_target(&item),
            Some(ArenaTarget::Channel("arena-influences".to_string()))
        );
        let bare = ItemId::parse("https://are.na/channel/arena-influences").unwrap();
        assert_eq!(
            arena_target(&bare),
            Some(ArenaTarget::Channel("arena-influences".to_string()))
        );
        let block = ItemId::parse("https://www.are.na/block/123").unwrap();
        assert_eq!(arena_target(&block), None);
    }

    #[test]
    fn arena_target_parses_legacy_user_channel_urls() {
        let legacy = ItemId::parse("https://www.are.na/jake-chvatal/item-industrial").unwrap();
        assert_eq!(
            arena_target(&legacy),
            Some(ArenaTarget::Channel("item-industrial".to_string()))
        );
        // Legacy URLs collapse onto the canonical channel item.
        let canonical = canonical_arena_item(&legacy).expect("legacy channel URL");
        assert_eq!(
            canonical.as_str(),
            "https://www.are.na/channel/item-industrial"
        );
        // Same thread as the canonical form.
        assert_eq!(
            resolver_thread_tag(&legacy),
            "import:https:::www.are.na:channel:item-industrial"
        );
        // Reserved site sections are not user slugs.
        let blog = ItemId::parse("https://www.are.na/blog/some-post").unwrap();
        assert_eq!(arena_target(&blog), None);
    }

    #[test]
    fn arena_target_parses_user_profile_urls() {
        let user = ItemId::parse("https://www.are.na/jake-chvatal").unwrap();
        assert_eq!(
            arena_target(&user),
            Some(ArenaTarget::User("jake-chvatal".to_string()))
        );
        assert_eq!(
            resolver_thread_tag(&user),
            "import:https:::www.are.na:jake-chvatal"
        );
        let reserved = ItemId::parse("https://www.are.na/explore").unwrap();
        assert_eq!(arena_target(&reserved), None);
    }

    #[test]
    fn resolver_thread_tag_is_one_per_channel() {
        let channel = channel_item();
        assert_eq!(
            resolver_thread_tag(&channel),
            "import:https:::www.are.na:channel:arena-influences"
        );
    }

    #[test]
    fn channel_entry_maps_to_channel_card() {
        let entry = serde_json::json!({
            "id": 9530,
            "type": "Channel",
            "title": "Adam Curtis",
            "slug": "adam-curtis",
            "counts": {"contents": 33},
            "description": {"markdown": "docs"},
        });
        let child = arena_child_from_entry(&entry).expect("channel entry");
        assert_eq!(child.url, "https://www.are.na/channel/adam-curtis");
        assert_eq!(child.card.kind, ArenaImportKind::Channel);
        assert!(child.card.sublines.iter().any(|l| l.contains("33")));
    }

    #[test]
    fn text_block_headline_falls_back_to_content_first_line() {
        let child = block_child(4929062, "Text");
        assert_eq!(child.url, "https://www.are.na/block/4929062");
        assert_eq!(child.card.kind, ArenaImportKind::Text);
        assert_eq!(child.card.headline, "quote body");
        assert_eq!(child.card.excerpt.as_deref(), Some("quote body"));
        assert!(child.card.sublines.iter().any(|l| l.contains("By: Ada")));
        assert!(child
            .card
            .sublines
            .iter()
            .any(|l| l.contains("Connected by: Charles")));
    }

    #[test]
    fn image_block_carries_image_and_source() {
        let entry = serde_json::json!({
            "id": 9613792,
            "type": "Image",
            "title": "Sitterwerk",
            "image": {"medium": {"src": "https://images.are.na/medium.jpg"}},
            "source": {"url": "https://example.com/page"},
        });
        let child = arena_child_from_entry(&entry).expect("image entry");
        assert_eq!(child.card.kind, ArenaImportKind::Image);
        assert_eq!(
            child.card.image_url.as_deref(),
            Some("https://images.are.na/medium.jpg")
        );
        assert_eq!(
            child.card.source_url.as_deref(),
            Some("https://example.com/page")
        );
        assert!(child
            .card
            .sublines
            .iter()
            .any(|l| l == "Source: example.com"));
    }

    #[test]
    fn child_to_dsl_declares_containment_and_card() {
        let channel = channel_item();
        let dsl = child_to_dsl(&channel, &block_child(7, "Link"));
        assert!(dsl
            .contains("https://www.are.na/block/7 <: https://www.are.na/channel/arena-influences"));
        assert!(dsl.contains("```slug-arena-card"));
        // Payload is base64 so nested ``` in excerpts cannot break DSL fences.
        assert!(!dsl.contains("\"schema\":\"slug_arena_import\""));
        assert_eq!(
            arena_urls_declared_in_ingest(&dsl, &channel),
            vec!["https://www.are.na/block/7".to_string()]
        );
    }

    #[test]
    fn parse_accepts_slug_arena_fence() {
        let card = ArenaImportCard::new(
            ArenaImportKind::Image,
            "https://www.are.na/block/1".into(),
            "img".into(),
        );
        let body = format!(
            "```slug-arena-card\n{}\n```\n",
            card_payload_for_dsl_fence(&card)
        );
        let parsed = parse_arena_import_from_body(&body).expect("parses");
        assert_eq!(parsed, card);
    }

    #[test]
    fn parse_rejects_raw_json_slug_arena_fence() {
        let card = ArenaImportCard::new(
            ArenaImportKind::Text,
            "https://www.are.na/block/2".into(),
            "txt".into(),
        );
        let body = format!(
            "```slug-arena-card\n{}\n```",
            serde_json::to_string(&card).unwrap()
        );
        assert!(
            parse_arena_import_from_body(&body).is_none(),
            "raw JSON inside slug-arena-card is not accepted"
        );
    }

    #[test]
    fn child_to_dsl_validates_as_ingest_with_markdown_fences_in_content() {
        let channel = channel_item();
        let entry = serde_json::json!({
            "id": 42,
            "type": "Text",
            "title": null,
            "content": {"markdown": "Some code:\n\n```json\n{\"a\":1}\n```\n\ntrailing"},
        });
        let child = arena_child_from_entry(&entry).expect("text entry");
        // Containment sides must be defined with bodies: in production the channel
        // is defined by `ensure_scope_body` (or the user's original paste); here
        // we prepend an equivalent channel definition to the same document.
        let text = format!(
            "{channel} {{are.na channel}}\n\n{child_dsl}",
            channel = channel.as_str(),
            child_dsl = child_to_dsl(&channel, &child)
        );
        let reduced = crate::reducer::ReducerState::default();
        let validated = crate::api::validate_ingest_document(
            &reduced,
            &text,
            &crate::reducer::ScopeId::Public,
        )
        .unwrap_or_else(|(code, msg, hint)| {
            panic!("arena import DSL should validate; got {code} {msg} hint={hint:?}\nDSL:\n{text}")
        });
        let item_body = validated
            .doc
            .statements
            .iter()
            .find_map(|s| match s {
                crate::dsl::Stmt::Item { body: Some(b), .. } if b.contains("slug-arena-card") => {
                    Some(b.as_str())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected item body\nDSL:\n{text}"));
        let parsed = parse_arena_import_from_body(item_body).unwrap_or_else(|| {
            panic!("body should round-trip as arena card; body was:\n{item_body}\nDSL:\n{text}")
        });
        assert_eq!(parsed.excerpt.as_deref(), child.card.excerpt.as_deref());
    }

    #[tokio::test]
    async fn list_channel_contents_pages_until_exhausted() {
        use axum::{extract::Query, routing::get, Json, Router};
        use tokio::sync::oneshot;

        let (tx, rx) = oneshot::channel::<()>();
        let app = Router::new().route(
            "/v3/channels/foo/contents",
            get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
                let page: u32 = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
                let (data, has_more) = match page {
                    1 => (
                        serde_json::json!([
                            {"id": 1, "type": "Link", "title": "one"},
                            {"id": 2, "type": "Channel", "title": "nested", "slug": "nested-chan"}
                        ]),
                        true,
                    ),
                    _ => (serde_json::json!([{"id": 3, "type": "Text", "content": {"markdown": "hi"}}]), false),
                };
                Json(serde_json::json!({
                    "meta": {"current_page": page, "has_more_pages": has_more},
                    "data": data,
                }))
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
        let resolver = ArenaResolver {
            client: reqwest::Client::new(),
            api_base_url: format!("http://{addr}"),
            token: None,
        };
        let item = ItemId::parse("https://www.are.na/channel/foo").unwrap();
        let kids = resolver.list_children(&item).await.unwrap();
        assert_eq!(kids.len(), 3);
        assert!(kids.iter().any(|k| k.url == "https://www.are.na/block/1"));
        assert!(kids
            .iter()
            .any(|k| k.url == "https://www.are.na/channel/nested-chan"));
        assert!(kids.iter().any(|k| k.url == "https://www.are.na/block/3"));
        let _ = tx.send(());
        let _ = server.await;
    }

    #[tokio::test]
    async fn list_user_channels_keeps_channels_skips_blocks() {
        use axum::{routing::get, Json, Router};
        use tokio::sync::oneshot;

        let (tx, rx) = oneshot::channel::<()>();
        let app = Router::new().route(
            "/v3/users/jake-chvatal/contents",
            get(|| async move {
                Json(serde_json::json!({
                    "meta": {"current_page": 1, "has_more_pages": false},
                    "data": [
                        {"id": 11, "type": "Channel", "title": "item/industrial", "slug": "item-industrial", "counts": {"contents": 1900}},
                        {"id": 12, "type": "Text", "content": {"markdown": "loose block"}},
                        {"id": 13, "type": "Channel", "title": "notes", "slug": "notes-abc"}
                    ],
                }))
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
        let resolver = ArenaResolver {
            client: reqwest::Client::new(),
            api_base_url: format!("http://{addr}"),
            token: None,
        };
        let user = ItemId::parse("https://www.are.na/jake-chvatal").unwrap();
        assert!(resolver.can_resolve_children(&user));
        let kids = resolver.list_children(&user).await.unwrap();
        assert_eq!(kids.len(), 2);
        assert!(kids
            .iter()
            .any(|k| k.url == "https://www.are.na/channel/item-industrial"));
        assert!(kids
            .iter()
            .any(|k| k.url == "https://www.are.na/channel/notes-abc"));
        assert!(!kids.iter().any(|k| k.url.contains("/block/")));
        let _ = tx.send(());
        let _ = server.await;
    }
}
