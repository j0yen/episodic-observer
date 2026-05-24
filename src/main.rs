//! episode — end-of-session JSONL detector CLI.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::doc_markdown,
    clippy::redundant_closure_for_method_calls
)]

use clap::{Parser, Subcommand};
use episodic_observer::{DEFAULT_MAX_MEMORIES, DETECTORS, observe, parse_session};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "episode", about = "End-of-session JSONL detectors for episodic recall memories")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Observe a session JSONL and emit candidate episodic memories.
    Observe {
        /// Path to the session JSONL.
        jsonl: PathBuf,
        /// Print candidates as JSON instead of writing (writing is downstream).
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Maximum candidates to emit.
        #[arg(long, default_value_t = DEFAULT_MAX_MEMORIES)]
        max_memories: usize,
    },
    /// List known detector names, one per line.
    Detectors,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(2);
        }
    };
    match cli.command {
        Command::Observe { jsonl, dry_run, max_memories } => {
            run_observe(&jsonl, dry_run, max_memories)
        }
        Command::Detectors => run_detectors(),
    }
}

fn run_observe(jsonl: &std::path::Path, dry_run: bool, max: usize) -> ExitCode {
    let content = match std::fs::read_to_string(jsonl) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("episode: cannot read {}: {}", jsonl.display(), e);
            return ExitCode::from(2);
        }
    };
    let events = parse_session(&content);
    let session = jsonl
        .file_name()
        .map_or_else(|| String::from("unknown"), |s| s.to_string_lossy().to_string());
    let subject = derive_subject();
    let candidates = observe(&events, &session, &subject, max);
    if dry_run {
        let payload = serde_json::json!(candidates);
        if let Ok(s) = serde_json::to_string(&payload) {
            println!("{s}");
        }
    }
    ExitCode::from(0)
}

fn derive_subject() -> String {
    let cwd = std::env::current_dir().ok();
    let basename = cwd
        .as_deref()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(String::from);
    match basename {
        Some(b) if !b.is_empty() => format!("project:{b}"),
        _ => String::from("project:unknown"),
    }
}

fn run_detectors() -> ExitCode {
    for d in DETECTORS {
        println!("{d}");
    }
    ExitCode::from(0)
}
