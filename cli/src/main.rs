use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "slugsocial", version, about = "Slug Social CLI (thin client)")]
struct Cli {
    /// Base URL of the server (e.g. https://slugsocial.fly.dev)
    #[arg(long, env = "SLUG_BASE_URL", default_value = "https://slugsocial.fly.dev")]
    base_url: String,

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

    /// Cast a vote (score in [-50, 50]) for a vs b
    Vote {
        a: String,
        #[arg(allow_hyphen_values = true)]
        score: i32,
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
    score: i32,
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
fn canonicalize_aspect(input: &str) -> String {
    canonicalize_sigiled(input, ':')
}
fn canonicalize_item(input: &str) -> String {
    canonicalize_sigiled(input, '/')
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
    let cli = Cli::parse();
    let base = cli.base_url.trim_end_matches('/').to_string();

    match cli.cmd {
        Command::Healthz => {
            let client = http_client(cli.key.as_deref())?;
            let url = format!("{base}/healthz");
            let body = client.get(url).send().await?.text().await?;
            println!("{body}");
        }

        Command::Rank { tag, aspect, limit } => {
            let client = http_client(cli.key.as_deref())?;
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
            let client = http_client(cli.key.as_deref())?;
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

        Command::Vote {
            a,
            score,
            b,
            tag,
            aspect,
        } => {
            let key = cli
                .key
                .as_deref()
                .ok_or_else(|| anyhow!("missing --key or SLUG_KEY (required for voting)"))?;

            let client = http_client(Some(key))?;
            let req = VoteRequest {
                tag: format!("#{}", canonicalize_tag(&tag)),
                aspect: format!(":{}", canonicalize_aspect(&aspect)),
                a: format!("/{}", canonicalize_item(&a)),
                b: format!("/{}", canonicalize_item(&b)),
                score,
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
            let key = cli
                .key
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


