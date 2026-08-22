mod detect;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "spend",
    version,
    about = "Ingest bank statements into the yearly-spend DuckDB database"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse statement files or directories (searched recursively)
    Ingest {
        /// CSV files or directories; the source is auto-detected from the parent directory name
        #[arg(required = true, value_name = "PATH")]
        paths: Vec<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Ingest { paths } => ingest(&paths),
    }
}

fn ingest(paths: &[PathBuf]) -> anyhow::Result<()> {
    let config = spend_core::config::Config::load()?;
    spend_core::db::ingest_connection(&config.db_path)?;

    let files = detect::collect_csvs(paths)?;
    println!("database: {}", config.db_path.display());

    if files.is_empty() {
        println!("no statement files found under: {}", display_paths(paths));
        return Ok(());
    }

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for file in &files {
        let source = detect::detect_source(file);
        let name = source.name().to_string();
        *counts.entry(name.clone()).or_insert(0) += 1;
        println!("{name:<10} {}", file.display());
    }
    let summary = counts
        .iter()
        .map(|(source, n)| format!("{n} {source}"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("found {summary}");
    println!("statement parsers land in the follow-up ingestion tickets");
    Ok(())
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
