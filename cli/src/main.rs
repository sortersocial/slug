use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

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
    }

    Ok(())
}


