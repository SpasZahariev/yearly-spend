mod cashback;
mod categorize;
mod detect;
mod neon;
mod revolut;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tokio::runtime::Runtime;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub parsed_rows: usize,
    pub inserted_or_updated_rows: usize,
    pub skipped: bool,
    pub llm_batches: usize,
}

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
        Command::Ingest { paths } => Runtime::new()?.block_on(ingest(&paths)),
    }
}

async fn ingest(paths: &[PathBuf]) -> anyhow::Result<()> {
    let config = spend_core::config::Config::load()?;
    let mut conn = spend_core::db::ingest_connection(&config.db_path)?;

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

    for file in files {
        match detect::detect_source(&file) {
            detect::Source::Neon => {
                let report = neon::ingest_file(&mut conn, &file, &config).await?;
                if report.skipped {
                    println!(
                        "neon       {} (already ingested; skipped before parsing)",
                        file.display()
                    );
                } else {
                    println!(
                        "neon       {} ({} rows, {} LLM batches)",
                        file.display(),
                        report.inserted_or_updated_rows,
                        report.llm_batches
                    );
                }
            }
            detect::Source::Revolut => {
                let report = revolut::ingest_file(&mut conn, &file, &config).await?;
                if report.skipped {
                    println!(
                        "revolut    {} (already ingested; skipped before parsing)",
                        file.display()
                    );
                } else {
                    println!(
                        "revolut    {} ({} rows, {} LLM batches)",
                        file.display(),
                        report.inserted_or_updated_rows,
                        report.llm_batches
                    );
                }
            }
            detect::Source::Cashback => {
                let report = cashback::ingest_file(&mut conn, &file, &config).await?;
                if report.skipped {
                    println!(
                        "cashback   {} (already ingested; skipped before parsing)",
                        file.display()
                    );
                } else {
                    println!(
                        "cashback   {} ({} rows, {} LLM batches)",
                        file.display(),
                        report.inserted_or_updated_rows,
                        report.llm_batches
                    );
                }
            }
            detect::Source::Unknown(source) => {
                println!("skip       {} (unknown source {source})", file.display());
            }
        }
    }

    Ok(())
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
