//! terragrunt-dag CLI - Generate dependency graph for terragrunt projects

use camino::Utf8PathBuf;
use clap::{Parser, ValueEnum};
use std::process::ExitCode;

use terragrunt_dag::discovery::discover_projects;
use terragrunt_dag::output::{OutputConfig, OutputFormat, generate_output};
use terragrunt_dag::processor::{ParseCache, ProjectResult, process_all_projects};

#[derive(Parser)]
#[command(name = "terragrunt-dag")]
#[command(author, version, about = "Generate dependency graph for terragrunt projects")]
struct Cli {
    /// Root directory to scan for terragrunt projects
    root: Utf8PathBuf,

    /// Output format
    #[arg(short, long, default_value = "json", value_enum)]
    format: Format,

    /// Verbose output (debug info to stderr)
    #[arg(short, long)]
    verbose: bool,

    /// Filter projects by glob pattern (e.g., 'prod/*', '**/vpc')
    #[arg(long)]
    filter: Option<String>,

    /// Base directory for relative paths in output (defaults to root)
    #[arg(long)]
    base_dir: Option<Utf8PathBuf>,

    /// Terraform version (for Atlantis output)
    #[arg(long)]
    terraform_version: Option<String>,

    /// Workflow name (for Atlantis/Digger output)
    #[arg(long, default_value = "terragrunt")]
    workflow: String,

    /// Workspace name override (for Atlantis/Digger output). If not set, uses project name.
    #[arg(long)]
    workspace: Option<String>,

    /// Enable autoplan in Atlantis output (default: true)
    #[arg(long, default_value_t = true)]
    autoplan: bool,

    /// Enable automerge in Atlantis output (default: false)
    #[arg(long, default_value_t = false)]
    automerge: bool,

    /// Enable parallel apply in Atlantis output (default: false)
    #[arg(long, default_value_t = false)]
    parallel_apply: bool,

    /// When true, dependencies cascade to include all transitive dependencies.
    /// Use --no-cascade-dependencies to disable (default: true)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    cascade_dependencies: bool,
}

#[derive(Clone, ValueEnum)]
enum Format {
    Json,
    Yaml,
    Atlantis,
    Digger,
}

impl From<Format> for OutputFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Json => OutputFormat::Json,
            Format::Yaml => OutputFormat::Yaml,
            Format::Atlantis => OutputFormat::Atlantis,
            Format::Digger => OutputFormat::Digger,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if let Err(e) = run(cli) {
        eprintln!("Error: {}", e);
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Validate root directory exists
    if !cli.root.exists() {
        return Err(format!("Root directory does not exist: {}", cli.root).into());
    }

    if cli.verbose {
        eprintln!("Scanning directory: {}", cli.root);
    }

    // Step 1: Discover projects
    let project_paths = discover_projects(&cli.root)?;

    if cli.verbose {
        eprintln!("Found {} terragrunt projects", project_paths.len());
    }

    if project_paths.is_empty() {
        // Output empty result
        let config = build_output_config(&cli);
        let output = generate_output(&[], cli.format.into(), &config)?;
        println!("{}", output);
        return Ok(());
    }

    // Step 2: Filter projects if pattern provided
    let filtered_paths = if let Some(ref pattern) = cli.filter {
        filter_projects(project_paths, pattern, &cli.root)?
    } else {
        project_paths
    };

    if cli.verbose {
        eprintln!("Processing {} projects (after filter)", filtered_paths.len());
    }

    // Step 3: Process projects with caching
    let cache = ParseCache::new();
    let results = process_all_projects(filtered_paths, &cache, cli.cascade_dependencies);

    if cli.verbose {
        let (cache_entries, cache_deps) = cache.stats();
        eprintln!("Cache stats: {} files parsed, {} total dependencies", cache_entries, cache_deps);
    }

    // Step 4: Collect successful projects and report errors
    let mut projects = Vec::new();
    let mut errors = Vec::new();
    let mut filtered_no_terraform = 0;

    for result in results {
        match result {
            ProjectResult::Ok(project) => {
                // Filter out projects without terraform source
                if project.has_terraform_source {
                    projects.push(project);
                } else {
                    filtered_no_terraform += 1;
                    if cli.verbose {
                        eprintln!("Filtered out project (no terraform block): {}", project.dir);
                    }
                }
            }
            ProjectResult::Err {
                path,
                error,
            } => {
                errors.push((path, error));
            }
        }
    }

    // Report errors to stderr
    if !errors.is_empty() {
        eprintln!("Warning: {} projects failed to process:", errors.len());
        for (path, error) in &errors {
            eprintln!("  {}: {}", path, error);
        }
    }

    if cli.verbose {
        eprintln!("Successfully processed {} projects", projects.len());
        if filtered_no_terraform > 0 {
            eprintln!("Filtered out {} projects without terraform source", filtered_no_terraform);
        }
    }

    // Step 5: Generate output
    let config = build_output_config(&cli);
    let output = generate_output(&projects, cli.format.into(), &config)?;

    println!("{}", output);

    Ok(())
}

fn build_output_config(cli: &Cli) -> OutputConfig {
    OutputConfig {
        base_dir: cli.base_dir.clone().or_else(|| Some(cli.root.clone())),
        terraform_version: cli.terraform_version.clone(),
        workflow: Some(cli.workflow.clone()),
        workspace: cli.workspace.clone(), // None means derive from project name
        include_self_in_watch: true,
        autoplan_enabled: cli.autoplan,
        automerge: cli.automerge,
        parallel_apply: cli.parallel_apply,
    }
}

/// Filter project paths by glob pattern
fn filter_projects(
    paths: Vec<Utf8PathBuf>,
    pattern: &str,
    base_dir: &Utf8PathBuf,
) -> Result<Vec<Utf8PathBuf>, Box<dyn std::error::Error>> {
    use glob::Pattern;

    let glob_pattern = Pattern::new(pattern)?;

    Ok(paths
        .into_iter()
        .filter(|path| {
            // Try to match against relative path from base_dir
            if let Ok(relative) = path.strip_prefix(base_dir) {
                glob_pattern.matches(relative.as_str())
            } else {
                glob_pattern.matches(path.as_str())
            }
        })
        .collect())
}
