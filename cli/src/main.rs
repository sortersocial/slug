use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "slugsocial", version, about = "Slug Social CLI (thin client)")]
struct Cli {
    /// Optional self-declared actor (e.g. "@tommy") for attribution/signing.
    #[arg(long = "as", env = "SLUG_AS")]
    as_actor: Option<String>,

    /// API key secret (sent as x-slug-key)
    #[arg(long, env = "SLUG_KEY")]
    key: Option<String>,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fetch and print a ranking for a tag/aspect
    Rank {
        tag: String,
        #[arg(long, default_value = ":default")]
        aspect: String,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },

    /// Get a suggested pair of items to compare next
    Pair {
        tag: String,
        #[arg(long, default_value = ":default")]
        aspect: String,
        /// If true, ignore ranking and return a random pair (useful for “skip”)
        #[arg(long)]
        random: bool,
    },

    /// Cast a vote (ratio like 3:1) for a vs b
    Vote {
        a: String,
        /// Ratio like "3:1" (prefer a over b).
        ratio: String,
        b: String,
        #[arg(long)]
        tag: String,
        #[arg(long, default_value = ":default")]
        aspect: String,
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

    /// Simple health check
    Healthz,
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
    score: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
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
}

#[derive(Debug, Deserialize)]
struct NextMoves {
    vote: String,
    rank: String,
    web: String,
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

fn canonicalize_sigiled(input: &str, sigil: char) -> String {
    let trimmed = input.trim();
    trimmed.strip_prefix(sigil).unwrap_or(trimmed).to_string()
}

fn canonicalize_tag(input: &str) -> String {
    canonicalize_sigiled(input, '#')
}
fn canonicalize_actor(input: &str) -> String {
    canonicalize_sigiled(input, '@')
}
fn canonicalize_aspect(input: &str) -> String {
    canonicalize_sigiled(input, ':')
}
fn canonicalize_item(input: &str) -> String {
    canonicalize_sigiled(input, '/')
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
        return;
    }
    for (i, r) in rows.iter().enumerate() {
        println!("{:>3}. {:<24} {:.6}", i + 1, r.item, r.score);
    }
}

fn print_next(next: &NextMoves) {
    println!();
    println!("next:");
    println!("  {}", next.vote);
    println!("  {}", next.rank);
    println!("  {}", next.web);
}

fn http_client(key: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    if let Some(k) = key {
        let v = HeaderValue::from_str(k).context("invalid SLUG_KEY value")?;
        headers.insert("x-slug-key", v);
    }
    Ok(reqwest::Client::builder().default_headers(headers).build()?)
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        as_actor,
        key,
        cmd,
    } = Cli::parse();
    let base = "https://slug.social";

    match cmd {
        Command::Healthz => {
            let client = http_client(key.as_deref())?;
            let url = format!("{base}/healthz");
            let body = client.get(url).send().await?.text().await?;
            println!("{body}");
        }

        Command::Rank { tag, aspect, limit } => {
            let client = http_client(key.as_deref())?;
            let tag_c = canonicalize_tag(&tag);
            let aspect_c = canonicalize_aspect(&aspect);
            let url = format!(
                "{base}/api/v0/rank?tag={}&aspect={}&limit={}",
                urlencoding::encode(&tag_c),
                urlencoding::encode(&aspect_c),
                limit
            );
            let resp: RankResponse = client.get(url).send().await?.json().await?;
            print_ranking(&resp.tag, &resp.aspect, &resp.ranking);
        }

        Command::Pair { tag, aspect, random } => {
            let client = http_client(key.as_deref())?;
            let tag_c = canonicalize_tag(&tag);
            let aspect_c = canonicalize_aspect(&aspect);
            let url = format!(
                "{base}/api/v0/pair?tag={}&aspect={}&random={}",
                urlencoding::encode(&tag_c),
                urlencoding::encode(&aspect_c),
                if random { "true" } else { "false" }
            );
            let resp: PairResponse = client.get(url).send().await?.json().await?;
            println!("{} {}", resp.left, resp.right);
        }

        Command::Vote { a, ratio, b, tag, aspect } => {
            let key = key
                .as_deref()
                .ok_or_else(|| anyhow!("missing --key or SLUG_KEY (required for voting)"))?;

            let client = http_client(Some(key))?;
            let _ = validate_ratio(&ratio)?;
            let req = VoteRequest {
                tag: format!("#{}", canonicalize_tag(&tag)),
                aspect: format!(":{}", canonicalize_aspect(&aspect)),
                a: format!("/{}", canonicalize_item(&a)),
                b: format!("/{}", canonicalize_item(&b)),
                ratio: Some(ratio),
                score: None,
                actor: as_actor.clone(),
            };
            let url = format!("{base}/api/v0/vote");
            let resp: VoteResponse = client.post(url).json(&req).send().await?.json().await?;
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
            let key = key
                .as_deref()
                .ok_or_else(|| anyhow!("missing --key or SLUG_KEY (required for ingest)"))?;
            let client = http_client(Some(key))?;

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

            // If caller passed --as, prefix the document with an actor directive.
            if let Some(a) = &as_actor {
                let a = a.trim().trim_start_matches('@');
                if !a.is_empty() {
                    text = format!("@{}\n\n{}", a, text);
                }
            }

            let req = IngestRequest {
                text,
                mode: Some(mode),
            };
            let url = format!("{base}/api/v0/ingest");
            let resp: IngestResponse = client.post(url).json(&req).send().await?.json().await?;
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
    }

    Ok(())
}


