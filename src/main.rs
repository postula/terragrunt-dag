//! terragrunt-dag CLI - Generate dependency graph for terragrunt projects

use camino::Utf8PathBuf;
use clap::{Parser, Subcommand, ValueEnum};
use std::io;
use std::process::ExitCode;

use terragrunt_dag::changes;
use terragrunt_dag::cycle::{DependencyEdge, EdgeType, analyze_cycles, detect_cycles, report_cycles};
use terragrunt_dag::discovery::discover_files;
use terragrunt_dag::output::{OutputConfig, OutputFormat, generate_output};
use terragrunt_dag::processor::{ParseCache, ProjectResult, process_all_projects};
use terragrunt_dag::project::Project;
use terragrunt_dag::stack;

#[derive(Parser)]
#[command(name = "terragrunt-dag")]
#[command(author, version, about = "Generate dependency graph for terragrunt projects")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Root directory to scan for terragrunt projects
    #[arg(global = true)]
    root: Option<Utf8PathBuf>,

    /// Output format
    #[arg(short, long, default_value = "json", value_enum, global = true)]
    format: Format,

    /// Verbose output (debug info to stderr)
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Filter projects by glob pattern (e.g., 'prod/*', '**/vpc')
    #[arg(long, global = true)]
    filter: Option<String>,

    /// Base directory for relative paths in output (defaults to root)
    #[arg(long, global = true)]
    base_dir: Option<Utf8PathBuf>,

    /// Terraform version (for Atlantis output)
    #[arg(long, global = true)]
    terraform_version: Option<String>,

    /// Workflow name (for Atlantis/Digger output)
    #[arg(long, default_value = "terragrunt", global = true)]
    workflow: String,

    /// Workspace name override (for Atlantis/Digger output). If not set, uses project name.
    #[arg(long, global = true)]
    workspace: Option<String>,

    /// Enable autoplan in Atlantis output (default: true)
    #[arg(long, default_value_t = true, global = true)]
    autoplan: bool,

    /// Enable automerge in Atlantis output (default: false)
    #[arg(long, default_value_t = false, global = true)]
    automerge: bool,

    /// Enable parallel apply in Atlantis output (default: false)
    #[arg(long, default_value_t = false, global = true)]
    parallel_apply: bool,

    /// When true, dependencies cascade to include all transitive dependencies.
    /// Use --no-cascade-dependencies to disable (default: true)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, global = true)]
    cascade_dependencies: bool,

    /// Exit with error if cycles are detected (default: false, only warn)
    #[arg(long, default_value_t = false, global = true)]
    strict: bool,

    /// Base ref for change detection (e.g., "origin/main"). Runs
    /// `git diff --name-only <ref>...HEAD`. Only used by --format gha.
    /// If unset, all units are marked changed.
    #[arg(long, global = true)]
    base_ref: Option<String>,

    /// For --format gha: emit only units with changed=true. Honors
    /// --cascade-dependencies for downstream cascading.
    #[arg(long, default_value_t = false, global = true)]
    gha_filter_unchanged: bool,

    /// For --format gha: maximum number of layer buckets the consuming workflow has hardcoded.
    /// terragrunt-dag exits non-zero if the DAG produces more layers than this.
    /// (A DAG with max layer index L requires L+1 buckets.)
    #[arg(long, global = true)]
    max_layers: Option<u32>,
}

#[derive(Subcommand)]
enum Commands {
    /// Analyze dependency cycles in detail
    Cycles,
}

#[derive(Clone, ValueEnum)]
enum Format {
    Json,
    Yaml,
    Atlantis,
    Digger,
    Gha,
}

impl From<Format> for OutputFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Json => OutputFormat::Json,
            Format::Yaml => OutputFormat::Yaml,
            Format::Atlantis => OutputFormat::Atlantis,
            Format::Digger => OutputFormat::Digger,
            Format::Gha => OutputFormat::Gha,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Handle subcommands first
    if let Some(ref command) = cli.command {
        return match command {
            Commands::Cycles => {
                if let Err(e) = run_cycles_command(&cli) {
                    eprintln!("Error: {}", e);
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
        };
    }

    // Normal run
    if let Err(e) = run(cli) {
        eprintln!("Error: {}", e);
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

struct DiscoveredProjects {
    root: Utf8PathBuf,
    projects: Vec<Project>,
}

fn discover_and_process(cli: &Cli) -> Result<DiscoveredProjects, Box<dyn std::error::Error>> {
    let root = cli.root.clone().ok_or("Root directory is required")?;

    if !root.exists() {
        return Err(format!("Root directory does not exist: {}", root).into());
    }

    let root = root.canonicalize_utf8().map_err(|e| format!("Failed to canonicalize root directory: {}", e))?;

    if cli.verbose {
        eprintln!("Scanning directory: {}", root);
    }

    let discovered = discover_files(&root)?;

    if cli.verbose {
        eprintln!(
            "Found {} terragrunt projects and {} stack files",
            discovered.units.len(),
            discovered.stack_files.len()
        );
    }

    let cache = ParseCache::new();
    let expanded = stack::expand(discovered.units, &discovered.stack_files, &cache);

    if cli.verbose && !expanded.synthetic_projects.is_empty() {
        eprintln!("Expanded stacks into {} synthetic units", expanded.synthetic_projects.len());
    }

    let filtered_paths = if let Some(ref pattern) = cli.filter {
        filter_projects(expanded.unit_dirs, pattern, &root)?
    } else {
        expanded.unit_dirs
    };

    if cli.verbose {
        eprintln!("Processing {} projects (after filter)", filtered_paths.len());
    }

    let results = process_all_projects(filtered_paths, &cache, cli.cascade_dependencies);

    if cli.verbose {
        let (cache_entries, cache_deps) = cache.stats();
        eprintln!("Cache stats: {} files parsed, {} total dependencies", cache_entries, cache_deps);
    }

    let mut projects = Vec::new();
    let mut errors = Vec::new();
    let mut filtered_no_terraform = 0;

    for result in results {
        match result {
            ProjectResult::Ok(project) => {
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

    if !errors.is_empty() {
        eprintln!("Warning: {} projects failed to process:", errors.len());
        for (path, error) in &errors {
            eprintln!("  {}: {}", path, error);
        }
    }

    // Append synthetic stack-generated projects (filtered by pattern if set).
    let synthetic = if let Some(ref pattern) = cli.filter {
        filter_synthetic(expanded.synthetic_projects, pattern, &root)?
    } else {
        expanded.synthetic_projects
    };
    projects.extend(synthetic);

    if cli.verbose {
        eprintln!("Successfully processed {} projects", projects.len());
        if filtered_no_terraform > 0 {
            eprintln!("Filtered out {} projects without terraform source", filtered_no_terraform);
        }
    }

    Ok(DiscoveredProjects {
        root,
        projects,
    })
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let discovered = discover_and_process(&cli)?;
    let format: OutputFormat = cli.format.clone().into();

    if discovered.projects.is_empty() {
        let config = build_output_config(&cli, &discovered.root, format)?;
        let output = generate_output(&[], format, &config)?;
        println!("{}", output);
        return Ok(());
    }

    // Cycle detection
    let edges = build_edges_from_projects(&discovered.projects);
    let cycle_result = detect_cycles(&edges);

    if !cycle_result.cycles.is_empty() {
        report_cycles(&cycle_result, &mut io::stderr())?;

        if cli.strict {
            return Err("Dependency cycles detected (use without --strict to generate output anyway)".into());
        }
    }

    // Generate output
    let config = build_output_config(&cli, &discovered.root, format)?;
    let output = generate_output(&discovered.projects, format, &config)?;
    println!("{}", output);

    Ok(())
}

fn format_label(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Json => "json",
        OutputFormat::Yaml => "yaml",
        OutputFormat::Atlantis => "atlantis",
        OutputFormat::Digger => "digger",
        OutputFormat::Gha => "gha",
    }
}

fn build_output_config(
    cli: &Cli,
    root: &Utf8PathBuf,
    format: OutputFormat,
) -> Result<OutputConfig, Box<dyn std::error::Error>> {
    if format != OutputFormat::Gha {
        let format_name = format_label(format);
        if cli.base_ref.is_some() {
            eprintln!("Warning: --base-ref is ignored for --format {} (only used by --format gha)", format_name);
        }
        if cli.gha_filter_unchanged {
            eprintln!(
                "Warning: --gha-filter-unchanged is ignored for --format {} (only used by --format gha)",
                format_name
            );
        }
        if cli.max_layers.is_some() {
            eprintln!("Warning: --max-layers is ignored for --format {} (only used by --format gha)", format_name);
        }
    }

    // Resolve the git diff only for --format gha when --base-ref is provided.
    // On failure, warn and fall back to an empty set (i.e. nothing changed).
    let changed_files = match (format, &cli.base_ref) {
        (OutputFormat::Gha, Some(base_ref)) => {
            let repo_root = std::path::Path::new(root.as_str());
            match changes::diff_against(base_ref, repo_root) {
                Ok(set) => Some(set),
                Err(e) => {
                    eprintln!("Warning: git diff failed, treating all units as unchanged: {}", e);
                    Some(std::collections::HashSet::new())
                }
            }
        }
        _ => None,
    };

    Ok(OutputConfig {
        base_dir: cli
            .base_dir
            .as_ref()
            .map(|p| p.canonicalize_utf8().map_err(|e| format!("Failed to canonicalize base directory '{}': {}", p, e)))
            .transpose()?
            .or_else(|| Some(root.clone())),
        terraform_version: cli.terraform_version.clone(),
        workflow: Some(cli.workflow.clone()),
        workspace: cli.workspace.clone(),
        include_self_in_watch: true,
        autoplan_enabled: cli.autoplan,
        automerge: cli.automerge,
        parallel_apply: cli.parallel_apply,
        changed_files,
        gha_filter_unchanged: cli.gha_filter_unchanged,
        cascade_unchanged: cli.cascade_dependencies,
        gha_max_layers: cli.max_layers,
    })
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

/// Filter synthetic projects by glob pattern
fn filter_synthetic(
    projects: Vec<Project>,
    pattern: &str,
    base_dir: &Utf8PathBuf,
) -> Result<Vec<Project>, Box<dyn std::error::Error>> {
    use glob::Pattern;

    let glob_pattern = Pattern::new(pattern)?;

    Ok(projects
        .into_iter()
        .filter(|p| {
            if let Ok(relative) = p.dir.strip_prefix(base_dir) {
                glob_pattern.matches(relative.as_str())
            } else {
                glob_pattern.matches(p.dir.as_str())
            }
        })
        .collect())
}

/// Build dependency edges from projects
///
/// Currently only tracks "Dependency" type edges (from project_dependencies).
/// Future: Include, ReadConfig, etc. from watch files.
fn build_edges_from_projects(projects: &[Project]) -> Vec<DependencyEdge> {
    projects
        .iter()
        .flat_map(|p| {
            p.project_dependencies.iter().map(move |dep| DependencyEdge {
                from: p.dir.to_string(),
                to: dep.clone(),
                edge_type: EdgeType::Dependency,
            })
        })
        .collect()
}

/// Run the cycles subcommand
fn run_cycles_command(cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let discovered = discover_and_process(cli)?;

    let edges = build_edges_from_projects(&discovered.projects);
    let cycle_result = detect_cycles(&edges);

    analyze_cycles(&cycle_result, &edges, &mut io::stderr())?;

    if !cycle_result.cycles.is_empty() {
        return Err("Dependency cycles detected".into());
    }

    Ok(())
}
