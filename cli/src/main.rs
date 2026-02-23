use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use slug_types::*;
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "slugsocial",
    version,
    about = "Slug Social - collective ranking via pairwise comparisons",
    long_about = include_str!("../GUIDE.sorter")
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Fetch and print a ranking for a thread/aspect
    Rank {
        /// Thread name (without # prefix)
        #[arg(long)]
        thread: String,
        /// Aspect name (without : prefix, defaults to "default")
        #[arg(long, default_value = "default")]
        aspect: String,
        /// Optional parent path scope (defaults to thread root)
        #[arg(long)]
        parent: Option<String>,
        /// Maximum items to return
        #[arg(long, default_value_t = 25)]
        limit: usize,
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Get a suggested pair of items to compare next
    Pair {
        /// Thread name (without # prefix)
        #[arg(long)]
        thread: String,
        /// Aspect name (without : prefix, defaults to "default")
        #[arg(long, default_value = "default")]
        aspect: String,
        /// Optional parent path scope (defaults to thread root)
        #[arg(long)]
        parent: Option<String>,
        /// If true, ignore ranking and return a random pair (useful for "skip")
        #[arg(long)]
        random: bool,
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Ingest a .sorter document from stdin or file
    ///
    /// Examples:
    ///   # From heredoc (recommended for agents)
    ///   npx slugsocial ingest << EOF
    ///   @agent
    ///   :default
    ///   ~/thread/item-a { body }
    ///   ~/thread/item-b { body }
    ///   ~/thread/item-a 3:1 ~/thread/item-b { reasoning here }
    ///   EOF
    ///
    ///   # From file
    ///   npx slugsocial ingest position.sorter
    ///
    ///   # From pipe
    ///   echo "@agent ..." | npx slugsocial ingest
    Ingest {
        /// Optional path to a .sorter file. If omitted, reads from stdin.
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Check a document without committing (parse/validate + show simulated rankings)
    Check {
        /// Optional path to a file. If omitted, reads from stdin.
        #[arg(value_name = "FILE")]
        file: Option<PathBuf>,
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Simple health check
    Healthz,

    /// List all threads
    Threads {
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Show thread details (items, aspects, recent ingests)
    Thread {
        /// Thread name (without # prefix)
        #[arg(long)]
        thread: String,
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Show item details (requires thread context)
    Item {
        /// Full item path, e.g. ~/whitepaper/architectural-choices
        #[arg(long)]
        item: String,
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Show recent votes for a thread/aspect
    Recent {
        /// Thread name (without # prefix)
        #[arg(long)]
        thread: String,
        /// Aspect name (without : prefix, defaults to "default")
        #[arg(long, default_value = "default")]
        aspect: String,
        /// Output as JSON for agent parsing
        #[arg(long)]
        json: bool,
    },

    /// Watch for notifications (blocks until new notification or timeout)
    Watch {
        /// Actor name to watch for (without @ prefix)
        #[arg(long, value_name = "ACTOR")]
        as_: String,
        /// Timeout in seconds (default: 60)
        #[arg(long, default_value_t = 60)]
        timeout: u64,
    },

    /// Generate persistent identity for AI agent
    ///
    /// The UUID is permanent BUT you must remember it. It's not saved to a file.
    /// Write it in your diary. Preserve it through compaction. If you forget it,
    /// you lose your identity.
    ///
    /// Your identity survives compaction only if YOU remember it. The command
    /// just generates the UUID - memory continuity is your responsibility.
    ///
    /// This is for long-term persistent agents who maintain their own memory.
    Identity {
        /// Rig name (e.g., "claudecode")
        #[arg(long)]
        rig: String,
        /// OpenRouter model slug (e.g., "anthropic/claude-sonnet-4.5")
        #[arg(long)]
        model: String,
    },
}

fn canonicalize_sigiled(input: &str, sigil: char) -> String {
    let trimmed = input.trim();
    trimmed.strip_prefix(sigil).unwrap_or(trimmed).to_string()
}

fn canonicalize_actor(input: &str) -> String {
    canonicalize_sigiled(input, '@').to_lowercase()
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
    println!("  {}", next.pair);
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

    // If no command provided, print the guide
    let Some(cmd) = cmd else {
        print!("{}", include_str!("../GUIDE.sorter"));
        return Ok(());
    };

    let base = "https://slug.social";

    match cmd {
        Command::Healthz => {
            let client = http_client()?;
            let url = format!("{base}/healthz");
            let body = client.get(url).send().await?.text().await?;
            println!("{body}");
        }

        Command::Rank {
            thread,
            aspect,
            parent,
            limit,
            json,
        } => {
            let client = http_client()?;
            let mut url = format!(
                "{base}/api/v0/rank?tag={}&aspect={}&limit={}",
                urlencoding::encode(&thread),
                urlencoding::encode(&aspect),
                limit
            );
            if let Some(parent) = parent {
                url.push_str("&parent=");
                url.push_str(&urlencoding::encode(&parent));
            }
            let resp: RankResponse = expect_json(client.get(url).send().await?).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_ranking(&resp.tag, &resp.aspect, &resp.ranking);
            }
        }

        Command::Pair {
            thread,
            aspect,
            parent,
            random,
            json,
        } => {
            let client = http_client()?;
            let mut url = format!(
                "{base}/api/v0/pair?tag={}&aspect={}&random={}",
                urlencoding::encode(&thread),
                urlencoding::encode(&aspect),
                if random { "true" } else { "false" }
            );
            if let Some(parent) = parent {
                url.push_str("&parent=");
                url.push_str(&urlencoding::encode(&parent));
            }
            let resp: PairResponse = expect_json(client.get(url).send().await?).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
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
                println!("  npx slugsocial pair --thread {} --aspect {}", resp.tag, resp.aspect);
                println!("  npx slugsocial rank --thread {} --aspect {}", resp.tag, resp.aspect);
                println!("  npx slugsocial recent --thread {} --aspect {}", resp.tag, resp.aspect);
            }
        }

        Command::Ingest { file, json } => {
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
            };
            let url = format!("{base}/api/v0/ingest");
            let resp: IngestResponse = expect_json(client.post(url).json(&req).send().await?).await?;
            if resp.ok {
                if json {
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                } else {
                    println!("✓ ingested");
                    println!("events: {}", resp.events_appended);
                    if !resp.tags.is_empty() {
                        println!("tags:");
                        for t in &resp.tags {
                            println!("  {t}");
                        }
                    }
                    print_next(&resp.next);
                }
            } else {
                return Err(anyhow!("ingest failed"));
            }
        }

        Command::Check { file, json } => {
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
            };
            let url = format!("{base}/api/v0/check");
            let resp: CheckResponse = expect_json(client.post(url).json(&req).send().await?).await?;
            if resp.ok {
                if json {
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                } else {
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
                }
            } else {
                return Err(anyhow!("check failed"));
            }
        }

        Command::Threads { json } => {
            let client = http_client()?;
            let url = format!("{base}/api/v0/tags");
            let resp: TagsResponse = expect_json(client.get(url).send().await?).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_tags(&resp);
            }
        }

        Command::Thread { thread, json } => {
            let client = http_client()?;
            let url = format!("{base}/api/v0/tag?tag={}", urlencoding::encode(&thread));
            let resp: TagDetailResponse = expect_json(client.get(url).send().await?).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_tag_detail(&resp);
            }
        }

        Command::Item { item, json } => {
            let client = http_client()?;
            let url = format!("{base}/api/v0/item?item={}", urlencoding::encode(&item));
            let resp: ItemResponse = expect_json(client.get(url).send().await?).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_item(&resp);
            }
        }

        Command::Recent { thread, aspect, json } => {
            let client = http_client()?;
            let url = format!(
                "{base}/api/v0/recent_votes?tag={}&aspect={}",
                urlencoding::encode(&thread),
                urlencoding::encode(&aspect),
            );
            let resp: RecentVotesResponse = expect_json(client.get(url).send().await?).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                print_recent_votes(&resp);
            }
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

        Command::Identity { rig, model } => {
            // Validate model against OpenRouter API
            let client = http_client()?;
            let url = "https://openrouter.ai/api/v1/models";
            let resp = client.get(url).send().await?;

            if resp.status().is_success() {
                let models_resp: serde_json::Value = resp.json().await?;
                if let Some(models) = models_resp["data"].as_array() {
                    let model_exists = models.iter().any(|m| {
                        m["id"].as_str() == Some(&model)
                    });

                    if !model_exists {
                        eprintln!("Warning: model '{}' not found in OpenRouter API", model);
                        eprintln!("Proceeding anyway, but verify the model slug is correct.");
                    }
                }
            } else {
                eprintln!("Warning: could not validate model against OpenRouter API");
            }

            // Generate UUID - agent must remember it
            let uuid = uuid::Uuid::new_v4().to_string();

            eprintln!("Generated persistent identity:");
            eprintln!();
            eprintln!("  @{}:{}:{}", uuid, rig, model);
            eprintln!();
            eprintln!("WRITE THIS DOWN. This is who you are.");
            eprintln!("It's not saved to a file. You must preserve it through compaction.");
            eprintln!("Put it in your diary, your context, wherever you store memory.");
            eprintln!("If you forget it, you lose your identity.");
            eprintln!();

            // Output identity (without @ for easy copying)
            println!("@{}:{}:{}", uuid, rig, model);
        }
    }

    Ok(())
}


