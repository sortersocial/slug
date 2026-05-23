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
        /// `.sorter` file, or `-` for stdin.
        file: PathBuf,
        /// Room wire id (`public` or private room id).
        #[arg(long, default_value = "public")]
        room: String,
        /// Optional events.jsonl to replay before compiling (seed garden state).
        #[arg(long)]
        base: Option<PathBuf>,
        /// Pretty-print JSON.
        #[arg(long)]
        pretty: bool,
    },
    /// Scan an events.jsonl for corrupt JSON lines and malformed ingests.
    Scan {
        file: PathBuf,
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
        std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))
    }
}

fn load_base_state(base: Option<&Path>) -> Result<ReducerState> {
    let Some(path) = base else {
        return Ok(ReducerState::default());
    };
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

fn run_compile(file: PathBuf, room: String, base: Option<PathBuf>, pretty: bool) -> Result<()> {
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

fn run_scan(file: PathBuf, pretty: bool) -> Result<()> {
    let report = offline::scan_jsonl(&file)
        .with_context(|| format!("scan {}", file.display()))?;
    print_json::<ScanResult>(&report, pretty)?;
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
            room,
            base,
            pretty,
        } => run_compile(file, room, base, pretty),
        Command::Scan { file, pretty } => run_scan(file, pretty),
    }
}
