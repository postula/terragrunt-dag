use clap::{Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "terragrunt-dag")]
#[command(about = "Generate dependency graph for terragrunt projects")]
struct Cli {
    /// Root directory to scan
    root: PathBuf,

    /// Output format
    #[arg(short, long, default_value = "json")]
    format: OutputFormat,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,

    /// Filter projects by glob pattern
    #[arg(long)]
    filter: Option<String>,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Json,
    Yaml,
}

fn main() {
    let _args = Cli::parse();
    println!("Not implemented yet");
}
