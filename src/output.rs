//! JSON/YAML output serialization.

use crate::Project;
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// Compute execution order layers from project dependencies.
/// Returns a map from project name to layer number.
///
/// Layer 0 = no dependencies (can run first, in parallel)
/// Layer N = depends only on projects in layers 0..N-1
///
/// Dependencies in projects are absolute paths, so we need to convert them
/// to names using base_dir before comparison.
fn compute_layers(projects: &[Project], _base_dir: Option<&Utf8PathBuf>) -> HashMap<String, u32> {
    let mut layers: HashMap<String, u32> = HashMap::new();

    // Build a mapping from absolute path to project name
    // This allows us to look up dependency paths and find their project names
    let mut path_to_name: HashMap<String, String> = HashMap::new();
    for p in projects {
        path_to_name.insert(p.dir.to_string(), p.name.clone());
    }

    // Iteratively assign layers
    let mut changed = true;
    while changed {
        changed = false;

        for project in projects {
            if layers.contains_key(&project.name) {
                continue;
            }

            // Convert dependency paths to names for comparison
            let dep_names: Vec<String> = project
                .project_dependencies
                .iter()
                .filter_map(|dep_path| path_to_name.get(dep_path).cloned())
                .collect();

            // Check if all dependencies have been assigned layers
            let deps_layers: Option<Vec<u32>> =
                dep_names.iter().map(|dep_name| layers.get(dep_name).copied()).collect();

            match deps_layers {
                Some(dep_layers) if dep_names.is_empty() || dep_layers.len() == dep_names.len() => {
                    // All deps resolved (or no deps) - assign layer
                    let max_dep_layer = dep_layers.into_iter().max().unwrap_or(0);
                    let my_layer = if dep_names.is_empty() {
                        0
                    } else {
                        max_dep_layer + 1
                    };
                    layers.insert(project.name.clone(), my_layer);
                    changed = true;
                }
                _ => {
                    // Dependencies not yet resolved, try again next iteration
                }
            }
        }
    }

    // Assign layer 0 to any remaining projects (circular deps or unknown deps)
    for project in projects {
        layers.entry(project.name.clone()).or_insert(0);
    }

    layers
}

/// Output format containing all discovered projects
#[derive(Debug, Serialize, Deserialize)]
pub struct Output {
    pub projects: Vec<Project>,
}

impl Output {
    pub fn new(projects: Vec<Project>) -> Self {
        Self {
            projects,
        }
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }
}

/// Output format options
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Json,
    Yaml,
    Atlantis,
    Digger,
}

/// Configuration options for output generation
#[derive(Debug, Clone)]
pub struct OutputConfig {
    /// Base directory to make paths relative to
    pub base_dir: Option<Utf8PathBuf>,
    /// Terraform version (for Atlantis)
    pub terraform_version: Option<String>,
    /// Default workflow name
    pub workflow: Option<String>,
    /// Default workspace
    pub workspace: Option<String>,
    /// Whether to include the project's own directory in watch patterns
    pub include_self_in_watch: bool,
    /// Enable autoplan in Atlantis output (default: true)
    pub autoplan_enabled: bool,
    /// Enable automerge in Atlantis output (default: false)
    pub automerge: bool,
    /// Enable parallel_apply in Atlantis output (default: false)
    pub parallel_apply: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            base_dir: None,
            terraform_version: None,
            workflow: Some("terragrunt".to_string()),
            workspace: Some("default".to_string()),
            include_self_in_watch: true,
            autoplan_enabled: true,
            automerge: false,
            parallel_apply: false,
        }
    }
}

#[derive(Error, Debug)]
pub enum OutputError {
    #[error("JSON serialization failed: {0}")]
    JsonError(#[from] serde_json::Error),
    #[error("YAML serialization failed: {0}")]
    YamlError(#[from] serde_yaml::Error),
}

/// Convert a path to relative form if a base directory is provided
fn make_relative(path: &camino::Utf8Path, base_dir: Option<&camino::Utf8Path>) -> String {
    match base_dir {
        Some(base) => path.strip_prefix(base).map(|p| p.to_string()).unwrap_or_else(|_| path.to_string()),
        None => path.to_string(),
    }
}

/// Derive a project name from a relative directory path.
///
/// Simply replaces slashes and backslashes with underscores.
/// This matches the behavior of terragrunt-atlantis-config.
///
/// Examples:
/// - "alarm_topic" -> "alarm_topic"
/// - "apps/pass/ecs_service" -> "apps_pass_ecs_service"
/// - "live/prod/vpc" -> "live_prod_vpc"
fn derive_name_from_dir(relative_dir: &str) -> String {
    relative_dir.replace(['/', '\\'], "_")
}

/// Convert a dependency path (absolute) to a dependency name using base_dir.
///
/// Dependencies are stored as absolute paths in Project.project_dependencies.
/// This function converts them to names by making them relative to base_dir
/// and then deriving a name from the relative path.
///
/// If base_dir is None, tries to use the path as-is.
fn dependency_path_to_name(dep_path: &str, base_dir: Option<&camino::Utf8Path>) -> String {
    let dep_path_buf = Utf8PathBuf::from(dep_path);
    let relative = make_relative(&dep_path_buf, base_dir);
    derive_name_from_dir(&relative)
}

/// Generic output format for JSON/YAML
#[derive(Serialize)]
struct GenericOutput {
    projects: Vec<GenericProject>,
}

#[derive(Serialize)]
struct GenericProject {
    name: String,
    dir: String,
    dependencies: Vec<String>,
    watch_files: Vec<String>,
}

/// Atlantis output format
#[derive(Serialize)]
struct AtlantisOutput {
    version: u8,
    automerge: bool,
    parallel_plan: bool,
    parallel_apply: bool,
    projects: Vec<AtlantisProject>,
}

#[derive(Serialize)]
struct AtlantisProject {
    name: String,
    dir: String,
    workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    terraform_version: Option<String>,
    workflow: String,
    autoplan: AtlantisAutoplan,
    execution_order_group: u32,
}

#[derive(Serialize)]
struct AtlantisAutoplan {
    when_modified: Vec<String>,
    enabled: bool,
}

/// Digger output format
#[derive(Serialize)]
struct DiggerOutput {
    projects: Vec<DiggerProject>,
}

#[derive(Serialize)]
struct DiggerProject {
    name: String,
    dir: String,
    workspace: String,
    terragrunt: bool,
    workflow: String,
    include_patterns: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    layer: u32,
}

/// Convert a Project to GenericProject format
fn to_generic_project(project: &Project, config: &OutputConfig) -> GenericProject {
    let dir = make_relative(&project.dir, config.base_dir.as_deref());
    let name = derive_name_from_dir(&dir);

    // Convert dependency paths to names
    let dependencies: Vec<String> = project
        .project_dependencies
        .iter()
        .map(|dep_path| dependency_path_to_name(dep_path, config.base_dir.as_deref()))
        .collect();

    GenericProject {
        name,
        dir,
        dependencies,
        watch_files: project.watch_files.iter().map(|f| make_relative(f, config.base_dir.as_deref())).collect(),
    }
}

/// Generate JSON output
fn generate_json(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    let output = GenericOutput {
        projects: projects.iter().map(|p| to_generic_project(p, config)).collect(),
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

/// Generate YAML output
fn generate_yaml(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    let output = GenericOutput {
        projects: projects.iter().map(|p| to_generic_project(p, config)).collect(),
    };
    Ok(serde_yaml::to_string(&output)?)
}

/// Check if a path looks like a directory (no file extension).
/// Used to detect terraform module source paths which need glob patterns.
fn looks_like_directory(path: &camino::Utf8Path) -> bool {
    // Get the last component of the path
    if let Some(file_name) = path.file_name() {
        // If it has no extension or doesn't contain a dot, it's likely a directory
        // Common file extensions we expect: .hcl, .tf, .yaml, .yml, .json, .tfvars
        !file_name.contains('.')
    } else {
        // No file name means it's a root path or similar
        true
    }
}

/// Convert a watch file path to the appropriate pattern for CI tools.
/// For directories (terraform modules), adds /**/*.tf glob pattern.
fn to_watch_pattern(relative_path: String) -> String {
    // Check if this looks like a directory (terraform module source)
    // by checking if it has no file extension
    let path = camino::Utf8Path::new(&relative_path);
    if looks_like_directory(path) {
        format!("{}/**/*.tf", relative_path)
    } else {
        relative_path
    }
}

/// Compute relative path from one directory to another file/directory.
/// Returns a path that, when resolved from `from_dir`, reaches `to_path`.
fn compute_relative_path(from_dir: &camino::Utf8Path, to_path: &camino::Utf8Path) -> String {
    // Try to find common ancestor and compute relative path
    let from_components: Vec<_> = from_dir.components().collect();
    let to_components: Vec<_> = to_path.components().collect();

    // Find common prefix length
    let common_len = from_components.iter().zip(to_components.iter()).take_while(|(a, b)| a == b).count();

    // Number of ".." needed to go up from from_dir to common ancestor
    let ups = from_components.len() - common_len;

    // Remaining path from common ancestor to to_path
    let remaining: Vec<_> = to_components[common_len..].iter().collect();

    let mut result = String::new();

    // Add ".." for each level we need to go up
    for i in 0..ups {
        if i > 0 {
            result.push('/');
        }
        result.push_str("..");
    }

    // Add remaining path components
    for component in remaining {
        if !result.is_empty() {
            result.push('/');
        }
        result.push_str(component.as_str());
    }

    if result.is_empty() {
        ".".to_string()
    } else {
        result
    }
}

/// Generate Atlantis YAML output
fn generate_atlantis(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    // Compute layers using internal project names
    let layers = compute_layers(projects, config.base_dir.as_ref());

    // Build a mapping from internal name to relative-dir-based name
    let mut name_mapping: HashMap<String, String> = HashMap::new();
    for p in projects {
        let dir = make_relative(&p.dir, config.base_dir.as_deref());
        let new_name = derive_name_from_dir(&dir);
        name_mapping.insert(p.name.clone(), new_name);
    }

    let atlantis_projects: Vec<AtlantisProject> = projects
        .iter()
        .map(|p| {
            let dir = make_relative(&p.dir, config.base_dir.as_deref());
            // Derive name from relative dir (not from processor's name)
            let name = derive_name_from_dir(&dir);

            let mut when_modified: Vec<String> = vec![];

            if config.include_self_in_watch {
                // Self-references use simple glob patterns (relative to project dir)
                when_modified.push("**/*.hcl".to_string());
                when_modified.push("**/*.tf".to_string());
            }

            // For watch files, compute paths relative to the PROJECT directory
            // Atlantis evaluates when_modified relative to the project dir
            for watch_file in &p.watch_files {
                let relative_to_project = compute_relative_path(&p.dir, watch_file);
                // For directories (terraform modules), add /**/*.tf pattern
                let pattern = to_watch_pattern(relative_to_project);
                when_modified.push(pattern);
            }

            // Get layer using internal name, default to 0 if not found
            let layer = layers.get(&p.name).copied().unwrap_or(0);

            AtlantisProject {
                name: name.clone(),
                dir,
                // Workspace defaults to name if not overridden
                workspace: config.workspace.clone().unwrap_or_else(|| name.clone()),
                terraform_version: config.terraform_version.clone(),
                workflow: config.workflow.clone().unwrap_or_else(|| "terragrunt".to_string()),
                autoplan: AtlantisAutoplan {
                    when_modified,
                    enabled: config.autoplan_enabled,
                },
                execution_order_group: layer,
            }
        })
        .collect();

    let output = AtlantisOutput {
        version: 3,
        automerge: config.automerge,
        parallel_plan: true,
        parallel_apply: config.parallel_apply,
        projects: atlantis_projects,
    };

    Ok(serde_yaml::to_string(&output)?)
}

/// Generate Digger YAML output
fn generate_digger(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    // Compute layers using internal project names
    let layers = compute_layers(projects, config.base_dir.as_ref());

    let digger_projects: Vec<DiggerProject> = projects
        .iter()
        .map(|p| {
            let dir = make_relative(&p.dir, config.base_dir.as_deref());
            // Derive name from relative dir (not from processor's name)
            let name = derive_name_from_dir(&dir);

            let mut include_patterns: Vec<String> = vec![];

            if config.include_self_in_watch {
                // Self-reference uses ./** (relative to project dir)
                include_patterns.push("./**".to_string());
            }

            // For watch files, compute paths relative to the PROJECT directory
            // Digger resolves patterns starting with . or .. relative to project dir
            for watch_file in &p.watch_files {
                let relative_to_project = compute_relative_path(&p.dir, watch_file);
                // For directories (terraform modules), add /**/*.tf pattern
                let with_glob = to_watch_pattern(relative_to_project);
                // Ensure path starts with ../ for Digger to resolve it from project dir
                let pattern = if with_glob.starts_with("..") {
                    with_glob
                } else {
                    // If it's in the same dir or below, prefix with ./
                    format!("./{}", with_glob)
                };
                include_patterns.push(pattern);
            }

            // Get layer using internal name, default to 0 if not found
            let layer = layers.get(&p.name).copied().unwrap_or(0);

            // Convert dependency paths to names
            let depends_on: Vec<String> = p
                .project_dependencies
                .iter()
                .map(|dep_path| dependency_path_to_name(dep_path, config.base_dir.as_deref()))
                .collect();

            DiggerProject {
                name: name.clone(),
                dir,
                // Workspace defaults to name if not overridden
                workspace: config.workspace.clone().unwrap_or_else(|| name.clone()),
                terragrunt: true,
                workflow: config.workflow.clone().unwrap_or_else(|| "default".to_string()),
                include_patterns,
                depends_on,
                layer,
            }
        })
        .collect();

    let output = DiggerOutput {
        projects: digger_projects,
    };

    Ok(serde_yaml::to_string(&output)?)
}

/// Generate output in the specified format
pub fn generate_output(
    projects: &[Project],
    format: OutputFormat,
    config: &OutputConfig,
) -> Result<String, OutputError> {
    match format {
        OutputFormat::Json => generate_json(projects, config),
        OutputFormat::Yaml => generate_yaml(projects, config),
        OutputFormat::Atlantis => generate_atlantis(projects, config),
        OutputFormat::Digger => generate_digger(projects, config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_projects() -> Vec<Project> {
        vec![
            Project {
                name: "prod_vpc".to_string(),
                dir: Utf8PathBuf::from("/repo/live/prod/vpc"),
                // Dependencies are now absolute paths
                project_dependencies: vec!["/repo/live/prod/network".to_string()],
                watch_files: vec![
                    Utf8PathBuf::from("/repo/live/common/root.hcl"),
                    Utf8PathBuf::from("/repo/modules/vpc/main.tf"),
                ],
                has_terraform_source: true,
            },
            Project {
                name: "prod_app".to_string(),
                dir: Utf8PathBuf::from("/repo/live/prod/app"),
                // Dependencies are now absolute paths
                project_dependencies: vec!["/repo/live/prod/vpc".to_string(), "/repo/live/prod/rds".to_string()],
                watch_files: vec![Utf8PathBuf::from("/repo/live/common/root.hcl")],
                has_terraform_source: true,
            },
        ]
    }

    // ============== JSON Output Tests ==============

    #[test]
    fn test_output_json_basic() {
        let projects = sample_projects();
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Json, &config).expect("should generate JSON");

        let parsed: serde_json::Value = serde_json::from_str(&output).expect("should be valid JSON");

        assert!(parsed["projects"].is_array());
        assert_eq!(parsed["projects"].as_array().unwrap().len(), 2);
        // Without base_dir, name is derived from absolute path
        assert_eq!(parsed["projects"][0]["name"], "_repo_live_prod_vpc");
        assert_eq!(parsed["projects"][0]["dir"], "/repo/live/prod/vpc");
    }

    #[test]
    fn test_output_json_with_relative_paths() {
        let projects = sample_projects();
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Json, &config).expect("should generate JSON");

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["projects"][0]["dir"], "live/prod/vpc");
        assert!(parsed["projects"][0]["watch_files"][0].as_str().unwrap().starts_with("live/"));
    }

    // ============== YAML Output Tests ==============

    #[test]
    fn test_output_yaml_basic() {
        let projects = sample_projects();
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Yaml, &config).expect("should generate YAML");

        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).expect("should be valid YAML");

        assert!(parsed["projects"].is_sequence());
        // Without base_dir, name is derived from absolute path
        assert_eq!(parsed["projects"][0]["name"], "_repo_live_prod_vpc");
    }

    // ============== Atlantis Output Tests ==============

    #[test]
    fn test_output_atlantis_structure() {
        let projects = sample_projects();
        let config = OutputConfig {
            terraform_version: Some("v1.5.0".to_string()),
            workflow: Some("terragrunt".to_string()),
            ..Default::default()
        };

        let output =
            generate_output(&projects, OutputFormat::Atlantis, &config).expect("should generate Atlantis YAML");

        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["version"], 3);
        assert_eq!(parsed["parallel_plan"], true);
        assert!(parsed["projects"].is_sequence());

        let project = &parsed["projects"][0];
        // Without base_dir, name is derived from absolute path
        assert_eq!(project["name"], "_repo_live_prod_vpc");
        assert_eq!(project["workspace"], "default");
        assert_eq!(project["workflow"], "terragrunt");

        assert!(project["autoplan"]["when_modified"].is_sequence());
        assert_eq!(project["autoplan"]["enabled"], true);
    }

    #[test]
    fn test_output_atlantis_autoplan_includes_watch_files() {
        let projects = sample_projects();
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let when_modified = parsed["projects"][0]["autoplan"]["when_modified"].as_sequence().unwrap();

        let patterns: Vec<&str> = when_modified.iter().filter_map(|v| v.as_str()).collect();

        assert!(patterns.iter().any(|p| p.contains("root.hcl")));
    }

    // ============== Digger Output Tests ==============

    #[test]
    fn test_output_digger_structure() {
        let projects = sample_projects();
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            workflow: Some("default".to_string()),
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Digger, &config).expect("should generate Digger YAML");

        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert!(parsed["projects"].is_sequence());

        let project = &parsed["projects"][0];
        // With base_dir, name is derived from relative path
        assert_eq!(project["name"], "live_prod_vpc");
        assert_eq!(project["dir"], "live/prod/vpc");
        assert_eq!(project["terragrunt"], true);
        assert_eq!(project["workflow"], "default");
    }

    #[test]
    fn test_output_digger_depends_on() {
        let projects = sample_projects();
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Digger, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let depends_on = parsed["projects"][0]["depends_on"].as_sequence();
        assert!(depends_on.is_some());
        assert_eq!(depends_on.unwrap().len(), 1);

        let depends_on_2 = parsed["projects"][1]["depends_on"].as_sequence().unwrap();
        assert_eq!(depends_on_2.len(), 2);
    }

    #[test]
    fn test_output_digger_include_patterns() {
        let projects = sample_projects();
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            include_self_in_watch: true,
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Digger, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let include_patterns = parsed["projects"][0]["include_patterns"].as_sequence().unwrap();

        let patterns: Vec<&str> = include_patterns.iter().filter_map(|v| v.as_str()).collect();

        // Self-reference should be ./**
        assert!(patterns.contains(&"./**"), "Expected ./** for self-reference, got {:?}", patterns);
        // Watch files should be relative to project dir with ../ prefix
        assert!(patterns.iter().any(|p| p.contains("root.hcl")), "Expected path to root.hcl, got {:?}", patterns);
    }

    #[test]
    fn test_output_digger_include_patterns_relative_to_project() {
        let projects = vec![Project {
            name: "prod_app".to_string(),
            dir: Utf8PathBuf::from("/repo/live/prod/app"),
            project_dependencies: vec![],
            watch_files: vec![Utf8PathBuf::from("/repo/root.hcl"), Utf8PathBuf::from("/repo/live/common.hcl")],
            has_terraform_source: true,
        }];
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            include_self_in_watch: true,
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Digger, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let include_patterns = parsed["projects"][0]["include_patterns"].as_sequence().unwrap();

        let patterns: Vec<&str> = include_patterns.iter().filter_map(|v| v.as_str()).collect();

        // Paths should be relative to project dir with ../ prefix for Digger
        assert!(patterns.contains(&"../../../root.hcl"), "Expected ../../../root.hcl, got {:?}", patterns);
        assert!(patterns.contains(&"../../common.hcl"), "Expected ../../common.hcl, got {:?}", patterns);
    }

    // ============== Edge Cases ==============

    #[test]
    fn test_output_empty_projects() {
        let projects: Vec<Project> = vec![];
        let config = OutputConfig::default();

        for format in [OutputFormat::Json, OutputFormat::Yaml, OutputFormat::Atlantis, OutputFormat::Digger] {
            let output = generate_output(&projects, format, &config);
            assert!(output.is_ok(), "Format {:?} should handle empty projects", format);
        }
    }

    #[test]
    fn test_output_project_no_dependencies() {
        let projects = vec![Project {
            name: "standalone".to_string(),
            dir: Utf8PathBuf::from("/repo/standalone"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Digger, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let depends_on = &parsed["projects"][0]["depends_on"];
        assert!(depends_on.is_null() || depends_on.as_sequence().map(|s| s.is_empty()).unwrap_or(true));
    }

    // ============== Layer Computation Tests ==============

    #[test]
    fn test_compute_layers_no_deps() {
        let projects = vec![
            Project {
                name: "a".to_string(),
                dir: Utf8PathBuf::from("/a"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];

        let layers = compute_layers(&projects, None);

        assert_eq!(layers.get("a"), Some(&0));
        assert_eq!(layers.get("b"), Some(&0));
    }

    #[test]
    fn test_compute_layers_chain() {
        // a -> b -> c (chain dependency)
        // Dependencies are now absolute paths
        let projects = vec![
            Project {
                name: "a".to_string(),
                dir: Utf8PathBuf::from("/a"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec!["/a".to_string()], // Absolute path
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "c".to_string(),
                dir: Utf8PathBuf::from("/c"),
                project_dependencies: vec!["/b".to_string()], // Absolute path
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];

        let layers = compute_layers(&projects, None);

        assert_eq!(layers.get("a"), Some(&0));
        assert_eq!(layers.get("b"), Some(&1));
        assert_eq!(layers.get("c"), Some(&2));
    }

    #[test]
    fn test_compute_layers_diamond() {
        //     a
        //    / \
        //   b   c
        //    \ /
        //     d
        // Dependencies are now absolute paths
        let projects = vec![
            Project {
                name: "a".to_string(),
                dir: Utf8PathBuf::from("/a"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec!["/a".to_string()],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "c".to_string(),
                dir: Utf8PathBuf::from("/c"),
                project_dependencies: vec!["/a".to_string()],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "d".to_string(),
                dir: Utf8PathBuf::from("/d"),
                project_dependencies: vec!["/b".to_string(), "/c".to_string()],
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];

        let layers = compute_layers(&projects, None);

        assert_eq!(layers.get("a"), Some(&0));
        assert_eq!(layers.get("b"), Some(&1));
        assert_eq!(layers.get("c"), Some(&1)); // Same layer as b (parallel)
        assert_eq!(layers.get("d"), Some(&2));
    }

    #[test]
    fn test_compute_layers_unknown_dependencies() {
        // Project with dependency on non-existent project
        // Dependencies are now absolute paths
        let projects = vec![
            Project {
                name: "a".to_string(),
                dir: Utf8PathBuf::from("/a"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec!["/a".to_string(), "/nonexistent".to_string()],
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];

        let layers = compute_layers(&projects, None);

        // Unknown dependencies should be ignored
        assert_eq!(layers.get("a"), Some(&0));
        assert_eq!(layers.get("b"), Some(&1));
    }

    #[test]
    fn test_output_atlantis_execution_order_group() {
        let projects = vec![
            Project {
                name: "vpc".to_string(),
                dir: Utf8PathBuf::from("/repo/vpc"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "app".to_string(),
                dir: Utf8PathBuf::from("/repo/app"),
                project_dependencies: vec!["/repo/vpc".to_string()], // Absolute path
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        // vpc should be group 0, app should be group 1
        // Without base_dir, names are derived from absolute paths
        let vpc = parsed["projects"].as_sequence().unwrap().iter().find(|p| p["name"] == "_repo_vpc").unwrap();
        let app = parsed["projects"].as_sequence().unwrap().iter().find(|p| p["name"] == "_repo_app").unwrap();

        assert_eq!(vpc["execution_order_group"], 0);
        assert_eq!(app["execution_order_group"], 1);
    }

    #[test]
    fn test_output_digger_layer() {
        let projects = vec![
            Project {
                name: "vpc".to_string(),
                dir: Utf8PathBuf::from("/repo/vpc"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "app".to_string(),
                dir: Utf8PathBuf::from("/repo/app"),
                project_dependencies: vec!["/repo/vpc".to_string()], // Absolute path
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Digger, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        // Without base_dir, names are derived from absolute paths
        let vpc = parsed["projects"].as_sequence().unwrap().iter().find(|p| p["name"] == "_repo_vpc").unwrap();
        let app = parsed["projects"].as_sequence().unwrap().iter().find(|p| p["name"] == "_repo_app").unwrap();

        assert_eq!(vpc["layer"], 0);
        assert_eq!(app["layer"], 1);
    }

    // ============== compute_relative_path Tests ==============

    #[test]
    fn test_compute_relative_path_sibling() {
        // From /repo/live/prod/app to /repo/live/prod/vpc
        let from = camino::Utf8Path::new("/repo/live/prod/app");
        let to = camino::Utf8Path::new("/repo/live/prod/vpc");

        assert_eq!(compute_relative_path(from, to), "../vpc");
    }

    #[test]
    fn test_compute_relative_path_up_to_root() {
        // From /repo/live/prod/app to /repo/root.hcl
        let from = camino::Utf8Path::new("/repo/live/prod/app");
        let to = camino::Utf8Path::new("/repo/root.hcl");

        assert_eq!(compute_relative_path(from, to), "../../../root.hcl");
    }

    #[test]
    fn test_compute_relative_path_nested_up() {
        // From /repo/prod/us-east-1/prod/webserver-cluster to /repo/_envcommon/webserver-cluster.hcl
        let from = camino::Utf8Path::new("/repo/prod/us-east-1/prod/webserver-cluster");
        let to = camino::Utf8Path::new("/repo/_envcommon/webserver-cluster.hcl");

        assert_eq!(compute_relative_path(from, to), "../../../../_envcommon/webserver-cluster.hcl");
    }

    #[test]
    fn test_compute_relative_path_same_dir() {
        // From /repo/live/prod to /repo/live/prod/env.hcl
        let from = camino::Utf8Path::new("/repo/live/prod");
        let to = camino::Utf8Path::new("/repo/live/prod/env.hcl");

        assert_eq!(compute_relative_path(from, to), "env.hcl");
    }

    #[test]
    fn test_output_atlantis_when_modified_relative_to_project() {
        let projects = vec![Project {
            name: "prod_app".to_string(),
            dir: Utf8PathBuf::from("/repo/live/prod/app"),
            project_dependencies: vec![],
            watch_files: vec![Utf8PathBuf::from("/repo/root.hcl"), Utf8PathBuf::from("/repo/live/common.hcl")],
            has_terraform_source: true,
        }];
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let when_modified = parsed["projects"][0]["autoplan"]["when_modified"].as_sequence().unwrap();

        let patterns: Vec<&str> = when_modified.iter().filter_map(|v| v.as_str()).collect();

        // Paths should be relative to project dir (/repo/live/prod/app)
        assert!(patterns.contains(&"../../../root.hcl"), "Expected ../../../root.hcl, got {:?}", patterns);
        assert!(patterns.contains(&"../../common.hcl"), "Expected ../../common.hcl, got {:?}", patterns);
    }

    // ============== Project Name Derivation Tests ==============

    #[test]
    fn test_derive_name_from_relative_dir_simple() {
        // Name should be derived from relative dir path with underscores
        assert_eq!(derive_name_from_dir("alarm_topic"), "alarm_topic");
        assert_eq!(derive_name_from_dir("vpc"), "vpc");
    }

    #[test]
    fn test_derive_name_from_relative_dir_nested() {
        // Nested paths should have slashes replaced with underscores
        assert_eq!(derive_name_from_dir("apps/pass/ecs_service"), "apps_pass_ecs_service");
        assert_eq!(derive_name_from_dir("live/prod/vpc"), "live_prod_vpc");
    }

    #[test]
    fn test_derive_name_from_relative_dir_with_backslash() {
        // Handle Windows-style paths (though we use UTF-8 paths)
        assert_eq!(derive_name_from_dir("apps\\pass\\ecs_service"), "apps_pass_ecs_service");
    }

    #[test]
    fn test_atlantis_output_uses_derived_names() {
        // Test that Atlantis output uses names derived from relative dirs
        let projects = vec![
            Project {
                name: "wrong_name_from_processor".to_string(), // This should be overridden
                dir: Utf8PathBuf::from("/repo/alarm_topic"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "also_wrong".to_string(),
                dir: Utf8PathBuf::from("/repo/apps/pass/ecs_service"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        // Check that names are derived from relative dirs, not from processor
        assert_eq!(parsed["projects"][0]["name"], "alarm_topic");
        assert_eq!(parsed["projects"][1]["name"], "apps_pass_ecs_service");
    }

    // ============== Workspace Generation Tests ==============

    #[test]
    fn test_atlantis_workspace_defaults_to_name() {
        // Without workspace override, workspace should match the name
        let projects = vec![Project {
            name: "ignored".to_string(),
            dir: Utf8PathBuf::from("/repo/alarm_topic"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            workspace: None, // No override
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        // Workspace should match name when no override
        assert_eq!(parsed["projects"][0]["name"], "alarm_topic");
        assert_eq!(parsed["projects"][0]["workspace"], "alarm_topic");
    }

    #[test]
    fn test_atlantis_workspace_can_be_overridden() {
        // With workspace override, all projects should use the override
        let projects = vec![
            Project {
                name: "ignored".to_string(),
                dir: Utf8PathBuf::from("/repo/alarm_topic"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
            Project {
                name: "ignored2".to_string(),
                dir: Utf8PathBuf::from("/repo/apps/pass"),
                project_dependencies: vec![],
                watch_files: vec![],
                has_terraform_source: true,
            },
        ];
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            workspace: Some("production".to_string()), // Override
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        // Both should use the override workspace
        assert_eq!(parsed["projects"][0]["workspace"], "production");
        assert_eq!(parsed["projects"][1]["workspace"], "production");
    }

    // ============== Atlantis Config Options Tests ==============

    #[test]
    fn test_atlantis_autoplan_can_be_disabled() {
        let projects = vec![Project {
            name: "test".to_string(),
            dir: Utf8PathBuf::from("/repo/test"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig {
            autoplan_enabled: false,
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["projects"][0]["autoplan"]["enabled"], false);
    }

    #[test]
    fn test_atlantis_autoplan_enabled_by_default() {
        let projects = vec![Project {
            name: "test".to_string(),
            dir: Utf8PathBuf::from("/repo/test"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["projects"][0]["autoplan"]["enabled"], true);
    }

    #[test]
    fn test_atlantis_automerge_disabled_by_default() {
        let projects = vec![Project {
            name: "test".to_string(),
            dir: Utf8PathBuf::from("/repo/test"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["automerge"], false);
    }

    #[test]
    fn test_atlantis_automerge_can_be_enabled() {
        let projects = vec![Project {
            name: "test".to_string(),
            dir: Utf8PathBuf::from("/repo/test"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig {
            automerge: true,
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["automerge"], true);
    }

    #[test]
    fn test_atlantis_parallel_apply_disabled_by_default() {
        let projects = vec![Project {
            name: "test".to_string(),
            dir: Utf8PathBuf::from("/repo/test"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["parallel_apply"], false);
    }

    #[test]
    fn test_atlantis_parallel_apply_can_be_enabled() {
        let projects = vec![Project {
            name: "test".to_string(),
            dir: Utf8PathBuf::from("/repo/test"),
            project_dependencies: vec![],
            watch_files: vec![],
            has_terraform_source: true,
        }];
        let config = OutputConfig {
            parallel_apply: true,
            ..Default::default()
        };

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["parallel_apply"], true);
    }
}
