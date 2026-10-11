//! x-listener — stream `#slugsocial` from X into slug.social.
//!
//! Uses raw reqwest against X API v2 filtered stream (no abandoned Twitter SDK).
//! Ingests via slug RPC `Post` with a bearer + agent delegate.

mod map;
mod seen;
mod slug_rpc;
mod x_api;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use map::tweet_to_sorter;
use seen::SeenStore;
use slug_rpc::SlugClient;
use x_api::{parse_stream_line, XClient};

#[derive(Parser, Debug)]
#[command(
    name = "x-listener",
    about = "Listen to #slugsocial on X and ingest posts into slug.social"
)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Connect to X filtered stream and ingest matching posts.
    Stream {
        /// X API app bearer token.
        #[arg(long, env = "X_BEARER_TOKEN")]
        x_bearer: String,
        /// Filtered-stream rule value (X query syntax).
        #[arg(long, default_value = "#slugsocial")]
        rule: String,
        /// Tag stored on the X stream rule (for cleanup).
        #[arg(long, default_value = "slugsocial-listener")]
        rule_tag: String,
        #[command(flatten)]
        slug: SlugArgs,
        /// File of ingested tweet ids (dedupe across restarts).
        #[arg(long, env = "X_LISTENER_SEEN", default_value = "x-listener-seen.txt")]
        seen: PathBuf,
        /// Print mapped sorter text and skip slug RPC.
        #[arg(long)]
        dry_run: bool,
    },
    /// Ingest from a JSONL fixture of X stream payloads (no X network).
    IngestFile {
        /// Path to JSONL (one stream payload object per line).
        path: PathBuf,
        #[command(flatten)]
        slug: SlugArgs,
        #[arg(long, env = "X_LISTENER_SEEN", default_value = "x-listener-seen.txt")]
        seen: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    /// Map a single stream JSON payload to sorter text (stdout).
    Map {
        /// Raw stream JSON object, or `-` for stdin.
        json: String,
    },
}

#[derive(clap::Args, Debug, Clone)]
struct SlugArgs {
    #[arg(long, env = "SLUG_SERVER", default_value = "https://slug.social")]
    slug_server: String,
    #[arg(long, env = "SLUG_BEARER_TOKEN")]
    slug_bearer: Option<String>,
    #[arg(long, env = "SLUG_DELEGATE")]
    slug_delegate: Option<String>,
    #[arg(long, default_value = "public")]
    room: String,
    /// Forum thread that receives ingested tweets.
    #[arg(long, default_value = "slugsocial")]
    thread: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::Stream {
            x_bearer,
            rule,
            rule_tag,
            slug,
            seen,
            dry_run,
        } => run_stream(x_bearer, rule, rule_tag, slug, seen, dry_run).await,
        Cmd::IngestFile {
            path,
            slug,
            seen,
            dry_run,
        } => run_file(path, slug, seen, dry_run).await,
        Cmd::Map { json } => {
            let raw = if json == "-" {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                json
            };
            let (tweet, username) = parse_stream_line(raw.trim())?;
            let mapped = tweet_to_sorter(&tweet, &username);
            print!("{}", mapped.text);
            Ok(())
        }
    }
}

async fn run_stream(
    x_bearer: String,
    rule: String,
    rule_tag: String,
    slug: SlugArgs,
    seen_path: PathBuf,
    dry_run: bool,
) -> Result<()> {
    let x = XClient::new(x_bearer);
    x.ensure_rule(&rule, &rule_tag).await?;
    let mut seen = SeenStore::open(&seen_path).await?;
    let poster = if dry_run {
        None
    } else {
        Some(slug_client_from_args(&slug)?)
    };
    eprintln!(
        "x-listener: streaming rule={rule:?} → {} /#{} (seen={} dry_run={dry_run})",
        slug.room,
        slug.thread,
        seen.path().display()
    );
    let mut rx = x.spawn_filtered_stream();
    while let Some(ev) = rx.recv().await {
        handle_tweet(ev.tweet, &ev.username, &mut seen, poster.as_ref(), dry_run).await?;
    }
    Ok(())
}

async fn run_file(
    path: PathBuf,
    slug: SlugArgs,
    seen_path: PathBuf,
    dry_run: bool,
) -> Result<()> {
    let raw = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let mut seen = SeenStore::open(&seen_path).await?;
    let poster = if dry_run {
        None
    } else {
        Some(slug_client_from_args(&slug)?)
    };
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (tweet, username) =
            parse_stream_line(line).with_context(|| format!("line {}", i + 1))?;
        handle_tweet(tweet, &username, &mut seen, poster.as_ref(), dry_run).await?;
    }
    Ok(())
}

fn slug_client_from_args(slug: &SlugArgs) -> Result<SlugClient> {
    let bearer = slug
        .slug_bearer
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("SLUG_BEARER_TOKEN / --slug-bearer required (or pass --dry-run)"))?;
    let delegate = slug
        .slug_delegate
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("SLUG_DELEGATE / --slug-delegate required (or pass --dry-run)"))?;
    if !delegate.contains(':') {
        bail!("delegate must look like uuid:rig:provider/model (got {delegate})");
    }
    Ok(SlugClient::new(
        &slug.slug_server,
        bearer,
        delegate,
        &slug.room,
        &slug.thread,
    ))
}

async fn handle_tweet(
    tweet: map::Tweet,
    username: &str,
    seen: &mut SeenStore,
    poster: Option<&SlugClient>,
    dry_run: bool,
) -> Result<()> {
    if seen.contains(&tweet.id) {
        eprintln!("x-listener: skip already-seen {}", tweet.id);
        return Ok(());
    }
    let mapped = tweet_to_sorter(&tweet, username);
    eprintln!(
        "x-listener: {} (@{}) → {}",
        mapped.tweet_id, mapped.author, mapped.status_url
    );
    if dry_run || poster.is_none() {
        println!("----- {} -----\n{}\n", mapped.tweet_id, mapped.text);
        seen.insert(&mapped.tweet_id).await?;
        return Ok(());
    }
    let poster = poster.expect("poster");
    match poster.post_text(&mapped.text).await {
        Ok(msg) => {
            eprintln!("x-listener: {msg}");
            seen.insert(&mapped.tweet_id).await?;
        }
        Err(e) => {
            // Don't mark seen — retry on restart.
            eprintln!("x-listener: post failed for {}: {e:#}", mapped.tweet_id);
        }
    }
    Ok(())
}
