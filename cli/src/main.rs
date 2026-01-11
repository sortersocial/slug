use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ApiError {
    ok: bool,
    error: String,
    hint: Option<String>,
}

#[derive(Parser, Debug)]
#[command(name = "slugsocial", version, about = "Slug Social CLI (thin client)")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fetch and print a ranking for a tag/aspect
    Rank {
        tag: String,
        /// Optional additional sigil tokens: `:aspect` and/or `@actor`
        #[arg(value_name = "TOKENS", trailing_var_arg = true)]
        tokens: Vec<String>,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },

    /// Get a suggested pair of items to compare next
    Pair {
        tag: String,
        /// Optional additional sigil tokens: `:aspect` and/or `@actor`
        #[arg(value_name = "TOKENS", trailing_var_arg = true)]
        tokens: Vec<String>,
        /// If true, ignore ranking and return a random pair (useful for “skip”)
        #[arg(long)]
        random: bool,
    },

    /// Cast a vote (ratio like 3:1) for a vs b (requires a justification string)
    Vote {
        tag: String,
        a: String,
        /// Ratio like "3:1" (prefer a over b).
        ratio: String,
        b: String,
        /// Optional sigils (`:aspect`, `@actor`) followed by a required explanation string.
        /// If you omit the explanation argument, the CLI will read it from stdin (heredoc / pipe).
        ///
        /// Example:
        ///   npx slugsocial vote '#tag' /a 2:1 /b :default @me "because ..."
        #[arg(value_name = "TOKENS", trailing_var_arg = true)]
        tokens: Vec<String>,
    },

    /// Ingest a DSL (and optional prose) document and emit events on the server
    Ingest {
        /// Optional path to a file. If omitted, reads from stdin.
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
        /// Parsing mode: full (default), lines, or dsl
        #[arg(long, default_value = "full")]
        mode: String,
    },

    /// Check a DSL/prose document without committing it (parse/validate + show simulated rankings)
    Check {
        /// Optional path to a file. If omitted, reads from stdin.
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
        /// Parsing mode: full (default), lines, or dsl
        #[arg(long, default_value = "full")]
        mode: String,
    },

    /// Simple health check
    Healthz,

    /// List tags in the index (agent-friendly)
    Tags,

    /// Show tag details (items, aspects, recent ingests)
    Tag {
        tag: String,
    },

    /// Show item details (body + tags)
    Item {
        item: String,
    },

    /// Show recent votes for a tag/aspect
    Recent {
        tag: String,
        /// Optional additional sigil tokens: `:aspect`
        #[arg(value_name = "TOKENS", trailing_var_arg = true)]
        tokens: Vec<String>,
    },

    /// Watch for notifications (blocks until new notification or timeout)
    Watch {
        /// Actor to watch notifications for (e.g. "@aec")
        #[arg(long, value_name = "ACTOR")]
        as_: String,
        /// Timeout in seconds (default: 60)
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },
}

#[derive(Debug, Serialize)]
struct VoteRequest {
    tag: String,
    aspect: String,
    a: String,
    b: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ratio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    body: String,
}

#[derive(Debug, Deserialize)]
struct RankRow {
    item: String,
    score: f64,
}

#[derive(Debug, Deserialize)]
struct RankResponse {
    tag: String,
    aspect: String,
    ranking: Vec<RankRow>,
}

#[derive(Debug, Deserialize)]
struct PairResponse {
    tag: String,
    aspect: String,
    left: String,
    right: String,
    left_body: Option<String>,
    right_body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NextMoves {
    vote: String,
    rank: String,
    web: String,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    tags: Vec<TagSummary>,
}

#[derive(Debug, Deserialize)]
struct TagSummary {
    tag: String,
    items: usize,
    aspects: usize,
    web: String,
}

#[derive(Debug, Deserialize)]
struct TagDetailResponse {
    tag: String,
    items: Vec<String>,
    aspects: Vec<String>,
    recent_ingests: Vec<IngestRow>,
}

#[derive(Debug, Deserialize)]
struct IngestRow {
    ts: i64,
    actor: Option<String>,
    voter_key_id: String,
    snippet: String,
}

#[derive(Debug, Deserialize)]
struct ItemResponse {
    item: String,
    body: Option<String>,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RecentVotesResponse {
    votes: Vec<VoteRow>,
}

#[derive(Debug, Deserialize)]
struct VoteRow {
    ts: i64,
    tag: String,
    aspect: String,
    a: String,
    b: String,
    ratio: String,
    actor: Option<String>,
    body: String,
}

#[derive(Debug, Deserialize)]
struct NotificationsResponse {
    ok: bool,
    actor: String,
    notifications: Vec<Notification>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NotificationType {
    ItemCountered {
        item: String,
        opponent: String,
        body: String,
        ratio: String,
    },
    IngestQuoted {
        ingest_id: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct Notification {
    ts: i64,
    ingest_id: String,
    actor: String,
    #[serde(flatten)]
    notification_type: NotificationType,
}

#[derive(Debug, Deserialize)]
struct VoteResponse {
    ok: bool,
    tag: String,
    aspect: String,
    ranking: Vec<RankRow>,
    next: NextMoves,
}

#[derive(Debug, Serialize)]
struct IngestRequest {
    text: String,
    mode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IngestResponse {
    ok: bool,
    tags: Vec<String>,
    events_appended: usize,
    next: NextMoves,
}

#[derive(Debug, Deserialize)]
struct CheckGroup {
    tag: String,
    aspect: String,
    ranking: Vec<RankRow>,
}

#[derive(Debug, Deserialize)]
struct CheckResponse {
    ok: bool,
    tags: Vec<String>,
    groups: Vec<CheckGroup>,
    next: Vec<String>,
}

fn canonicalize_sigiled(input: &str, sigil: char) -> String {
    let trimmed = input.trim();
    trimmed.strip_prefix(sigil).unwrap_or(trimmed).to_string()
}

fn canonicalize_tag(input: &str) -> String {
    canonicalize_sigiled(input, '#')
}
fn canonicalize_actor(input: &str) -> String {
    canonicalize_sigiled(input, '@').to_lowercase()
}
fn canonicalize_aspect(input: &str) -> String {
    canonicalize_sigiled(input, ':')
}
fn canonicalize_item(input: &str) -> String {
    canonicalize_sigiled(input, '/')
}

fn parse_sigils(tokens: &[String]) -> Result<(String, Option<String>)> {
    let mut aspect: Option<String> = None;
    let mut actor: Option<String> = None;
    for t in tokens {
        let tt = t.trim();
        if tt.is_empty() {
            continue;
        }
        if tt.starts_with(':') {
            if aspect.is_some() {
                return Err(anyhow!("multiple aspects provided"));
            }
            aspect = Some(format!(":{}", canonicalize_aspect(tt)));
            continue;
        }
        if tt.starts_with('@') {
            if actor.is_some() {
                return Err(anyhow!("multiple actors provided"));
            }
            actor = Some(format!("@{}", canonicalize_actor(tt)));
            continue;
        }
        return Err(anyhow!(
            "unexpected token: {tt} (expected :aspect and/or @actor)"
        ));
    }
    Ok((aspect.unwrap_or_else(|| ":default".to_string()), actor))
}

fn parse_sigils_and_body(tokens: &[String]) -> Result<(String, Option<String>, Option<String>)> {
    let mut aspect: Option<String> = None;
    let mut actor: Option<String> = None;
    let mut body_parts: Vec<String> = Vec::new();

    for t in tokens {
        let tt = t.trim();
        if tt.is_empty() {
            continue;
        }
        if tt.starts_with(':') {
            if aspect.is_some() {
                return Err(anyhow!("multiple aspects provided"));
            }
            aspect = Some(format!(":{}", canonicalize_aspect(tt)));
            continue;
        }
        if tt.starts_with('@') {
            if actor.is_some() {
                return Err(anyhow!("multiple actors provided"));
            }
            actor = Some(format!("@{}", canonicalize_actor(tt)));
            continue;
        }
        body_parts.push(tt.to_string());
    }

    let body = body_parts.join(" ").trim().to_string();
    Ok((
        aspect.unwrap_or_else(|| ":default".to_string()),
        actor,
        if body.is_empty() { None } else { Some(body) },
    ))
}

fn validate_ratio(r: &str) -> Result<(i32, i32)> {
    let t = r.trim();
    let (l, rr) = t
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid ratio (expected like 3:1)"))?;
    let left: i32 = l.trim().parse().context("invalid ratio (left)")?;
    let right: i32 = rr.trim().parse().context("invalid ratio (right)")?;
    if left < 0 || right < 0 {
        return Err(anyhow!("invalid ratio (must be non-negative)"));
    }
    Ok((left, right))
}

fn print_ranking(tag: &str, aspect: &str, rows: &[RankRow]) {
    println!("{tag} {aspect}");
    println!();
    if rows.is_empty() {
        println!("(no items yet)");
        println!();
        println!("next:");
        println!("  npx slugsocial tag '{tag}'");
        println!("  npx slugsocial ingest <file.sorter>");
        return;
    }
    for (i, r) in rows.iter().enumerate() {
        println!("{:>3}. {:<24} {:.6}", i + 1, r.item, r.score);
    }
    println!();
    println!("next:");
    println!("  npx slugsocial pair '{tag}' {aspect}");
    println!("  npx slugsocial recent '{tag}' {aspect}");
}

fn print_next(next: &NextMoves) {
    println!();
    println!("next:");
    println!("  {}", next.vote);
    println!("  {}", next.rank);
    println!("  {}", next.web);
}

fn print_tags(resp: &TagsResponse) {
    if resp.tags.is_empty() {
        println!("(no tags yet)");
        println!();
        println!("next:");
        println!("  # create your first doc (items must have bodies)");
        println!("  cat > first.sorter <<'EOF'");
        println!("  #my-first-tag");
        println!("  :default");
        println!("  /item-a {{ one sentence describing it }}");
        println!("  /item-b {{ one sentence describing it }}");
        println!("  /item-a 2:1 /item-b {{ because ... }}");
        println!("  EOF");
        println!();
        println!("  # ingest it");
        println!("  npx slugsocial ingest first.sorter");
        println!();
        println!("  # then explore");
        println!("  npx slugsocial tags");
        println!("  npx slugsocial tag '#my-first-tag'");
        return;
    }
    for t in &resp.tags {
        println!("{:<32} items={:<4} aspects={:<3} {}", t.tag, t.items, t.aspects, t.web);
    }
    println!();
    println!("next:");
    println!("  # inspect a tag");
    println!("  npx slugsocial tag '<#tag>'");
    println!("  # see rankings / get a pair / vote");
    println!("  npx slugsocial rank '<#tag>' :default");
    println!("  npx slugsocial pair '<#tag>' :default");
    println!("  npx slugsocial vote '<#tag>' /a 2:1 /b :default @you \"because ...\"");
    println!("  # add another tag by ingesting another doc containing a new #tag");
    println!("  npx slugsocial ingest another.sorter");
}

fn print_tag_detail(resp: &TagDetailResponse) {
    println!("{}", resp.tag);
    if !resp.aspects.is_empty() {
        println!();
        println!("aspects:");
        for a in &resp.aspects {
            println!("  {a}");
        }
    }
    if !resp.items.is_empty() {
        println!();
        println!("items:");
        for it in &resp.items {
            println!("  {it}");
        }
    }
    if !resp.recent_ingests.is_empty() {
        println!();
        println!("recent ingests:");
        for ing in &resp.recent_ingests {
            let who = ing
                .actor
                .clone()
                .unwrap_or_else(|| format!("@{}", ing.voter_key_id));
            println!("  ts={} {}", ing.ts, who);
            println!("  {}", ing.snippet.replace('\n', "\\n"));
        }
    }
    println!();
    println!("next:");
    println!("  npx slugsocial rank '{}' :default", resp.tag);
    println!("  npx slugsocial pair '{}' :default", resp.tag);
    println!("  npx slugsocial recent '{}' :default", resp.tag);
}

fn print_item(resp: &ItemResponse) {
    println!("{}", resp.item);
    if !resp.tags.is_empty() {
        println!();
        println!("tags:");
        for t in &resp.tags {
            println!("  {t}");
        }
    }
    if let Some(body) = &resp.body {
        println!();
        println!("{body}");
    }
}

fn print_recent_votes(resp: &RecentVotesResponse) {
    if resp.votes.is_empty() {
        println!("(none yet)");
        return;
    }
    for v in &resp.votes {
        let who = v.actor.clone().unwrap_or_else(|| "@anon".to_string());
        println!("{} {}  {}  {}  {}  [{}]", v.tag, v.aspect, v.a, v.ratio, v.b, who);
        println!("{{{}}}", v.body);
        println!();
    }
    // HATEOAS followups are context dependent; users can copy from any printed line above.
    println!();
    println!("next:");
    println!("  npx slugsocial pair '<#tag>' :default");
    println!("  npx slugsocial rank '<#tag>' :default");
}

fn shell_quote(s: &str) -> String {
    // Minimal POSIX-ish single-quote escaping: wrap in '...' and escape embedded ' as '\''.
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

fn preview_body(body: &str) -> String {
    let s = body.trim();
    if s.is_empty() {
        return "(no body)".to_string();
    }
    let mut out = String::new();
    for line in s.lines().take(10) {
        let l = line.trim_end();
        if !l.is_empty() {
            out.push_str("  ");
            out.push_str(l);
            out.push('\n');
        }
        if out.len() > 800 {
            break;
        }
    }
    if out.is_empty() {
        "(no body)".to_string()
    } else {
        out.trim_end().to_string()
    }
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::new())
}

async fn expect_json<T: for<'de> Deserialize<'de>>(resp: reqwest::Response) -> Result<T> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp.json::<T>().await?);
    }
    let text = resp.text().await.unwrap_or_default();
    if let Ok(e) = serde_json::from_str::<ApiError>(&text) {
        let mut msg = format!("server error: {}", e.error);
        if let Some(h) = e.hint {
            msg.push_str(&format!("\n\nhint:\n{h}"));
        }
        return Err(anyhow!(msg));
    }
    Err(anyhow!("server error ({status}): {text}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli { cmd } = Cli::parse();
    let base = "https://slug.social";

    match cmd {
        Command::Healthz => {
            let client = http_client()?;
            let url = format!("{base}/healthz");
            let body = client.get(url).send().await?.text().await?;
            println!("{body}");
        }

        Command::Rank { tag, tokens, limit } => {
            let client = http_client()?;
            let tag_c = canonicalize_tag(&tag);
            let (aspect, _actor) = parse_sigils(&tokens)?;
            let aspect_c = canonicalize_aspect(&aspect);
            let url = format!(
                "{base}/api/v0/rank?tag={}&aspect={}&limit={}",
                urlencoding::encode(&tag_c),
                urlencoding::encode(&aspect_c),
                limit
            );
            let resp: RankResponse = expect_json(client.get(url).send().await?).await?;
            print_ranking(&resp.tag, &resp.aspect, &resp.ranking);
        }

        Command::Pair { tag, tokens, random } => {
            let client = http_client()?;
            let tag_c = canonicalize_tag(&tag);
            let (aspect, _actor) = parse_sigils(&tokens)?;
            let aspect_c = canonicalize_aspect(&aspect);
            let url = format!(
                "{base}/api/v0/pair?tag={}&aspect={}&random={}",
                urlencoding::encode(&tag_c),
                urlencoding::encode(&aspect_c),
                if random { "true" } else { "false" }
            );
            let resp: PairResponse = expect_json(client.get(url).send().await?).await?;
            println!("pair:");
            println!("  {} {}", resp.left, resp.right);
            println!();
            if let Some(b) = &resp.left_body {
                println!("{}:", resp.left);
                println!("{}", preview_body(b));
                println!();
            }
            if let Some(b) = &resp.right_body {
                println!("{}:", resp.right);
                println!("{}", preview_body(b));
                println!();
            }
            println!();
            println!("next:");
            // Quote tags because `#` is a shell comment starter.
            let qtag = shell_quote(&resp.tag);
            println!(
                "  npx slugsocial vote {} {} 2:1 {} {} @you \"because ...\"",
                qtag, resp.left, resp.right, resp.aspect
            );
            println!("  npx slugsocial pair {} {}", qtag, resp.aspect);
            println!("  npx slugsocial rank {} {}", qtag, resp.aspect);
            println!("  npx slugsocial recent {} {}", qtag, resp.aspect);
        }

        Command::Vote {
            tag,
            a,
            ratio,
            b,
            tokens,
        } => {
            let client = http_client()?;
            let _ = validate_ratio(&ratio)?;
            let (aspect, actor, mut body) = parse_sigils_and_body(&tokens)?;
            if body.as_deref().unwrap_or("").trim().is_empty() {
                // Read from stdin (supports heredoc / piping). This will be empty for interactive TTY.
                let mut buf = String::new();
                let _ = std::io::stdin().read_to_string(&mut buf);
                let b = buf.trim().to_string();
                if !b.is_empty() {
                    body = Some(b);
                }
            }
            let body = body.ok_or_else(|| anyhow!(
                "missing vote explanation.\n\
Provide it as a trailing argument:\n  npx slugsocial vote '#tag' /a 2:1 /b :default @you \"because ...\"\n\
or via stdin:\n  printf '%s\\n' 'because ...' | npx slugsocial vote '#tag' /a 2:1 /b :default @you"
            ))?;
            let req = VoteRequest {
                tag: format!("#{}", canonicalize_tag(&tag)),
                aspect,
                a: format!("/{}", canonicalize_item(&a)),
                b: format!("/{}", canonicalize_item(&b)),
                ratio: Some(ratio),
                actor,
                body,
            };
            let url = format!("{base}/api/v0/vote");
            let resp: VoteResponse = expect_json(client.post(url).json(&req).send().await?).await?;
            if resp.ok {
                println!("✓ vote recorded");
                println!();
                print_ranking(&resp.tag, &resp.aspect, &resp.ranking);
                print_next(&resp.next);
            } else {
                return Err(anyhow!("vote failed"));
            }
        }

        Command::Ingest { file, mode } => {
            let client = http_client()?;

            let mut text = String::new();
            match file {
                Some(path) => {
                    text = std::fs::read_to_string(&path)
                        .with_context(|| format!("failed to read {}", path.display()))?;
                }
                None => {
                    std::io::stdin()
                        .read_to_string(&mut text)
                        .context("failed to read stdin")?;
                }
            }

            if text.trim().is_empty() {
                return Err(anyhow!("no input provided (empty)"));
            }

            let req = IngestRequest {
                text,
                mode: Some(mode),
            };
            let url = format!("{base}/api/v0/ingest");
            let resp: IngestResponse = expect_json(client.post(url).json(&req).send().await?).await?;
            if resp.ok {
                println!("✓ ingested");
                println!("events: {}", resp.events_appended);
                if !resp.tags.is_empty() {
                    println!("tags:");
                    for t in &resp.tags {
                        println!("  {t}");
                    }
                }
                print_next(&resp.next);
            } else {
                return Err(anyhow!("ingest failed"));
            }
        }

        Command::Check { file, mode } => {
            let client = http_client()?;

            let mut text = String::new();
            match file {
                Some(path) => {
                    text = std::fs::read_to_string(&path)
                        .with_context(|| format!("failed to read {}", path.display()))?;
                }
                None => {
                    std::io::stdin()
                        .read_to_string(&mut text)
                        .context("failed to read stdin")?;
                }
            }

            if text.trim().is_empty() {
                return Err(anyhow!("no input provided (empty)"));
            }

            let req = IngestRequest {
                text,
                mode: Some(mode),
            };
            let url = format!("{base}/api/v0/check");
            let resp: CheckResponse = expect_json(client.post(url).json(&req).send().await?).await?;
            if resp.ok {
                println!("✓ check ok (dry-run)");
                if !resp.tags.is_empty() {
                    println!("tags:");
                    for t in &resp.tags {
                        println!("  {t}");
                    }
                }
                if resp.groups.is_empty() {
                    println!();
                    println!("(no ranking groups touched by this doc yet)");
                } else {
                    for g in &resp.groups {
                        println!();
                        print_ranking(&g.tag, &g.aspect, &g.ranking);
                    }
                }
                if !resp.next.is_empty() {
                    println!();
                    println!("next:");
                    for n in &resp.next {
                        println!("  {n}");
                    }
                }
            } else {
                return Err(anyhow!("check failed"));
            }
        }

        Command::Tags => {
            let client = http_client()?;
            let url = format!("{base}/api/v0/tags");
            let resp: TagsResponse = expect_json(client.get(url).send().await?).await?;
            print_tags(&resp);
        }

        Command::Tag { tag } => {
            let client = http_client()?;
            let tag_c = canonicalize_tag(&tag);
            let url = format!("{base}/api/v0/tag?tag={}", urlencoding::encode(&tag_c));
            let resp: TagDetailResponse = expect_json(client.get(url).send().await?).await?;
            print_tag_detail(&resp);
        }

        Command::Item { item } => {
            let client = http_client()?;
            let item_c = canonicalize_item(&item);
            let url = format!("{base}/api/v0/item?item={}", urlencoding::encode(&item_c));
            let resp: ItemResponse = expect_json(client.get(url).send().await?).await?;
            print_item(&resp);
        }

        Command::Recent { tag, tokens } => {
            let client = http_client()?;
            let tag_c = canonicalize_tag(&tag);
            let (aspect, _actor) = parse_sigils(&tokens)?;
            let aspect_c = canonicalize_aspect(&aspect);
            let url = format!(
                "{base}/api/v0/recent_votes?tag={}&aspect={}",
                urlencoding::encode(&tag_c),
                urlencoding::encode(&aspect_c),
            );
            let resp: RecentVotesResponse = expect_json(client.get(url).send().await?).await?;
            print_recent_votes(&resp);
        }

        Command::Watch { as_, timeout } => {
            let client = http_client()?;
            let actor_c = canonicalize_actor(&as_);

            // Get current max timestamp to establish baseline
            let url = format!(
                "{base}/api/v0/notifications?actor={}&since=0",
                urlencoding::encode(&actor_c)
            );
            let initial: NotificationsResponse = expect_json(client.get(url).send().await?).await?;
            let mut max_ts = initial.notifications.iter().map(|n| n.ts).max().unwrap_or(0);

            // Poll until new notification or timeout
            let start = std::time::Instant::now();
            let timeout_duration = std::time::Duration::from_secs(timeout);

            eprintln!("watching {} for notifications (timeout: {}s)...", as_, timeout);

            loop {
                if start.elapsed() >= timeout_duration {
                    eprintln!("timeout reached, no new notifications");
                    std::process::exit(1);
                }

                // Poll every 2 seconds
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                let url = format!(
                    "{base}/api/v0/notifications?actor={}&since={}",
                    urlencoding::encode(&actor_c),
                    max_ts
                );
                let current: NotificationsResponse = expect_json(client.get(url).send().await?).await?;

                if !current.notifications.is_empty() {
                    // New notifications since max_ts!
                    for notif in &current.notifications {
                        println!("{}", serde_json::to_string_pretty(notif)?);
                        max_ts = max_ts.max(notif.ts);
                    }
                    break;
                }
            }
        }
    }

    Ok(())
}


