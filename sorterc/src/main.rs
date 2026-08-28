use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use slugsocial_server::{
    offline::{self, CompileError, CompileResult, ScanResult},
    reducer::ReducerState,
};

#[derive(Parser)]
#[command(
    name = "sorterc",
    about = "Offline .sorter compiler and events.jsonl linter (dev only)",
    version
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse and simulate a .sorter document; emit ranking JSON to stdout.
    Compile {
        /// `.sorter` file, or `-` for stdin. Omit when using --ingest.
        file: Option<PathBuf>,
        /// Compile one ingest event from a log (by event id / uuid).
        #[arg(long)]
        ingest: Option<String>,
        /// events.jsonl containing the ingest (required with --ingest).
        #[arg(long)]
        from: Option<PathBuf>,
        /// Room wire id (`public` or private room id). Ignored with --ingest.
        #[arg(long, default_value = "public")]
        room: String,
        /// Optional events.jsonl to replay before compiling (seed garden state).
        #[arg(long)]
        base: Option<PathBuf>,
        /// Pretty-print JSON.
        #[arg(long)]
        pretty: bool,
    },
    /// Scan an events.jsonl for corrupt JSON lines and DSL parse failures.
    Scan {
        file: PathBuf,
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
        /// Pretty-print JSON (requires --json).
        #[arg(long)]
        pretty: bool,
    },
}

fn read_input(path: &Path) -> Result<String> {
    if path.as_os_str() == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        Ok(buf)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
    }
}

fn load_base_state(base: Option<&Path>) -> Result<ReducerState> {
    let Some(path) = base else {
        return Ok(ReducerState::default());
    };
    eprintln!(
        "sorterc: replaying {} for base state (may take a while on large logs)…",
        path.display()
    );
    let (state, bad_lines) = offline::load_reducer_from_jsonl(path)
        .with_context(|| format!("load base jsonl {}", path.display()))?;
    if !bad_lines.is_empty() {
        bail!(
            "base jsonl has {} corrupt line(s); fix or omit --base",
            bad_lines.len()
        );
    }
    Ok(state)
}

fn print_json<T: serde::Serialize>(value: &T, pretty: bool) -> Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}

fn run_compile(
    file: Option<PathBuf>,
    ingest: Option<String>,
    from: Option<PathBuf>,
    room: String,
    base: Option<PathBuf>,
    pretty: bool,
) -> Result<()> {
    if ingest.is_some() ^ from.is_some() {
        bail!("--ingest and --from must be used together");
    }
    if ingest.is_some() && (file.is_some() || base.is_some()) {
        bail!("with --ingest/--from, omit file and --base");
    }
    if file.is_none() && ingest.is_none() {
        bail!("pass a .sorter file or --ingest <id> --from events.jsonl");
    }

    if let (Some(ingest_id), Some(log_path)) = (ingest, from) {
        eprintln!(
            "sorterc: compiling ingest {ingest_id} from {}…",
            log_path.display()
        );
        match offline::compile_ingest_from_log(&log_path, &ingest_id) {
            Ok(result) => {
                print_json::<CompileResult>(&result, pretty)?;
                Ok(())
            }
            Err(err) => {
                print_json::<CompileError>(&err.into_compile_error(), pretty)?;
                std::process::exit(1);
            }
        }
    } else {
        let file = file.expect("checked above");
        let text = read_input(&file)?;
        let base_state = load_base_state(base.as_deref())?;
        match offline::compile_document(&base_state, &room, &text) {
            Ok(result) => {
                print_json::<CompileResult>(&result, pretty)?;
                Ok(())
            }
            Err(err) => {
                print_json::<CompileError>(&err, pretty)?;
                std::process::exit(1);
            }
        }
    }
}

fn print_scan_human(report: &ScanResult) {
    if report.ok {
        println!(
            "ok: {} ({} lines, {} events, {} ingests)",
            report.path, report.total_lines, report.parsed_events, report.ingest_events
        );
        return;
    }

    let problems = report.bad_json_lines.len() + report.malformed_ingests.len();
    println!(
        "{}: {} lines, {} events, {} ingests, {problems} problem(s)",
        report.path, report.total_lines, report.parsed_events, report.ingest_events
    );

    let mut first = true;
    for bad in &report.bad_json_lines {
        if !first {
            println!();
        }
        first = false;
        println!("line {}: invalid JSON", bad.line);
        println!("{}", bad.message);
    }

    for bad in &report.malformed_ingests {
        if !first {
            println!();
        }
        first = false;
        println!(
            "line {}: ingest {} (#{} in {})",
            bad.line, bad.id, bad.thread_tag, bad.room_id
        );
        println!("{}", bad.parse_error);
    }
}

fn run_scan(file: PathBuf, json: bool, pretty: bool) -> Result<()> {
    let report = offline::scan_jsonl(&file).with_context(|| format!("scan {}", file.display()))?;
    if json {
        print_json::<ScanResult>(&report, pretty)?;
    } else {
        print_scan_human(&report);
    }
    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Compile {
            file,
            ingest,
            from,
            room,
            base,
            pretty,
        } => run_compile(file, ingest, from, room, base, pretty),
        Command::Scan { file, json, pretty } => run_scan(file, json, pretty),
    }
}
