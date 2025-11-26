//! JSON/YAML output serialization.

use crate::Project;
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// Compute execution order layers from project dependencies.
/// Returns a map from project name to layer number.
///
/// Layer 0 = no dependencies (can run first, in parallel)
/// Layer N = depends only on projects in layers 0..N-1
fn compute_layers(projects: &[Project]) -> HashMap<String, u32> {
    let mut layers: HashMap<String, u32> = HashMap::new();

    // Build a set of all project names for validation
    let project_names: HashSet<&str> = projects.iter().map(|p| p.name.as_str()).collect();

    // Iteratively assign layers
    let mut changed = true;
    while changed {
        changed = false;

        for project in projects {
            if layers.contains_key(&project.name) {
                continue;
            }

            // Check if all dependencies have been assigned layers
            let deps_layers: Option<Vec<u32>> = project
                .project_dependencies
                .iter()
                .filter(|dep| project_names.contains(dep.as_str())) // Only known deps
                .map(|dep| layers.get(dep).copied())
                .collect();

            match deps_layers {
                Some(dep_layers)
                    if project.project_dependencies.is_empty()
                        || dep_layers.len()
                            == project
                                .project_dependencies
                                .iter()
                                .filter(|d| project_names.contains(d.as_str()))
                                .count() =>
                {
                    // All deps resolved (or no deps) - assign layer
                    let max_dep_layer = dep_layers.into_iter().max().unwrap_or(0);
                    let my_layer = if project.project_dependencies.is_empty() {
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
        Self { projects }
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
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            base_dir: None,
            terraform_version: None,
            workflow: Some("terragrunt".to_string()),
            workspace: Some("default".to_string()),
            include_self_in_watch: true,
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
        Some(base) => path
            .strip_prefix(base)
            .map(|p| p.to_string())
            .unwrap_or_else(|_| path.to_string()),
        None => path.to_string(),
    }
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
    GenericProject {
        name: project.name.clone(),
        dir: make_relative(&project.dir, config.base_dir.as_deref()),
        dependencies: project.project_dependencies.clone(),
        watch_files: project
            .watch_files
            .iter()
            .map(|f| make_relative(f, config.base_dir.as_deref()))
            .collect(),
    }
}

/// Generate JSON output
fn generate_json(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    let output = GenericOutput {
        projects: projects
            .iter()
            .map(|p| to_generic_project(p, config))
            .collect(),
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

/// Generate YAML output
fn generate_yaml(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    let output = GenericOutput {
        projects: projects
            .iter()
            .map(|p| to_generic_project(p, config))
            .collect(),
    };
    Ok(serde_yaml::to_string(&output)?)
}

/// Generate Atlantis YAML output
fn generate_atlantis(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    let layers = compute_layers(projects);

    let atlantis_projects: Vec<AtlantisProject> = projects
        .iter()
        .map(|p| {
            let dir = make_relative(&p.dir, config.base_dir.as_deref());

            let mut when_modified: Vec<String> = vec![];

            if config.include_self_in_watch {
                when_modified.push(format!("{}/**/*.hcl", dir));
                when_modified.push(format!("{}/**/*.tf", dir));
            }

            for watch_file in &p.watch_files {
                let relative = make_relative(watch_file, config.base_dir.as_deref());
                when_modified.push(relative);
            }

            AtlantisProject {
                name: p.name.clone(),
                dir,
                workspace: config
                    .workspace
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                terraform_version: config.terraform_version.clone(),
                workflow: config
                    .workflow
                    .clone()
                    .unwrap_or_else(|| "terragrunt".to_string()),
                autoplan: AtlantisAutoplan {
                    when_modified,
                    enabled: true,
                },
                execution_order_group: *layers.get(&p.name).unwrap_or(&0),
            }
        })
        .collect();

    let output = AtlantisOutput {
        version: 3,
        automerge: false,
        parallel_plan: true,
        parallel_apply: false,
        projects: atlantis_projects,
    };

    Ok(serde_yaml::to_string(&output)?)
}

/// Generate Digger YAML output
fn generate_digger(projects: &[Project], config: &OutputConfig) -> Result<String, OutputError> {
    let layers = compute_layers(projects);

    let digger_projects: Vec<DiggerProject> = projects
        .iter()
        .map(|p| {
            let dir = make_relative(&p.dir, config.base_dir.as_deref());

            let mut include_patterns: Vec<String> = vec![];

            if config.include_self_in_watch {
                include_patterns.push(format!("{}/**", dir));
            }

            for watch_file in &p.watch_files {
                let relative = make_relative(watch_file, config.base_dir.as_deref());
                include_patterns.push(relative);
            }

            DiggerProject {
                name: p.name.clone(),
                dir,
                workspace: config
                    .workspace
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                terragrunt: true,
                workflow: config
                    .workflow
                    .clone()
                    .unwrap_or_else(|| "default".to_string()),
                include_patterns,
                depends_on: p.project_dependencies.clone(),
                layer: *layers.get(&p.name).unwrap_or(&0),
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
                project_dependencies: vec!["prod_network".to_string()],
                watch_files: vec![
                    Utf8PathBuf::from("/repo/live/common/root.hcl"),
                    Utf8PathBuf::from("/repo/modules/vpc/main.tf"),
                ],
            },
            Project {
                name: "prod_app".to_string(),
                dir: Utf8PathBuf::from("/repo/live/prod/app"),
                project_dependencies: vec!["prod_vpc".to_string(), "prod_rds".to_string()],
                watch_files: vec![Utf8PathBuf::from("/repo/live/common/root.hcl")],
            },
        ]
    }

    // ============== JSON Output Tests ==============

    #[test]
    fn test_output_json_basic() {
        let projects = sample_projects();
        let config = OutputConfig::default();

        let output =
            generate_output(&projects, OutputFormat::Json, &config).expect("should generate JSON");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("should be valid JSON");

        assert!(parsed["projects"].is_array());
        assert_eq!(parsed["projects"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["projects"][0]["name"], "prod_vpc");
        assert_eq!(parsed["projects"][0]["dir"], "/repo/live/prod/vpc");
    }

    #[test]
    fn test_output_json_with_relative_paths() {
        let projects = sample_projects();
        let config = OutputConfig {
            base_dir: Some(Utf8PathBuf::from("/repo")),
            ..Default::default()
        };

        let output =
            generate_output(&projects, OutputFormat::Json, &config).expect("should generate JSON");

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(parsed["projects"][0]["dir"], "live/prod/vpc");
        assert!(
            parsed["projects"][0]["watch_files"][0]
                .as_str()
                .unwrap()
                .starts_with("live/")
        );
    }

    // ============== YAML Output Tests ==============

    #[test]
    fn test_output_yaml_basic() {
        let projects = sample_projects();
        let config = OutputConfig::default();

        let output =
            generate_output(&projects, OutputFormat::Yaml, &config).expect("should generate YAML");

        let parsed: serde_yaml::Value =
            serde_yaml::from_str(&output).expect("should be valid YAML");

        assert!(parsed["projects"].is_sequence());
        assert_eq!(parsed["projects"][0]["name"], "prod_vpc");
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

        let output = generate_output(&projects, OutputFormat::Atlantis, &config)
            .expect("should generate Atlantis YAML");

        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert_eq!(parsed["version"], 3);
        assert_eq!(parsed["parallel_plan"], true);
        assert!(parsed["projects"].is_sequence());

        let project = &parsed["projects"][0];
        assert_eq!(project["name"], "prod_vpc");
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

        let when_modified = parsed["projects"][0]["autoplan"]["when_modified"]
            .as_sequence()
            .unwrap();

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

        let output = generate_output(&projects, OutputFormat::Digger, &config)
            .expect("should generate Digger YAML");

        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        assert!(parsed["projects"].is_sequence());

        let project = &parsed["projects"][0];
        assert_eq!(project["name"], "prod_vpc");
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

        let include_patterns = parsed["projects"][0]["include_patterns"]
            .as_sequence()
            .unwrap();

        let patterns: Vec<&str> = include_patterns.iter().filter_map(|v| v.as_str()).collect();

        assert!(patterns.iter().any(|p| p.contains("live/prod/vpc")));
        assert!(patterns.iter().any(|p| p.contains("root.hcl")));
    }

    // ============== Edge Cases ==============

    #[test]
    fn test_output_empty_projects() {
        let projects: Vec<Project> = vec![];
        let config = OutputConfig::default();

        for format in [
            OutputFormat::Json,
            OutputFormat::Yaml,
            OutputFormat::Atlantis,
            OutputFormat::Digger,
        ] {
            let output = generate_output(&projects, format, &config);
            assert!(
                output.is_ok(),
                "Format {:?} should handle empty projects",
                format
            );
        }
    }

    #[test]
    fn test_output_project_no_dependencies() {
        let projects = vec![Project {
            name: "standalone".to_string(),
            dir: Utf8PathBuf::from("/repo/standalone"),
            project_dependencies: vec![],
            watch_files: vec![],
        }];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Digger, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let depends_on = &parsed["projects"][0]["depends_on"];
        assert!(
            depends_on.is_null()
                || depends_on
                    .as_sequence()
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
        );
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
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec![],
                watch_files: vec![],
            },
        ];

        let layers = compute_layers(&projects);

        assert_eq!(layers.get("a"), Some(&0));
        assert_eq!(layers.get("b"), Some(&0));
    }

    #[test]
    fn test_compute_layers_chain() {
        // a -> b -> c (chain dependency)
        let projects = vec![
            Project {
                name: "a".to_string(),
                dir: Utf8PathBuf::from("/a"),
                project_dependencies: vec![],
                watch_files: vec![],
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec!["a".to_string()],
                watch_files: vec![],
            },
            Project {
                name: "c".to_string(),
                dir: Utf8PathBuf::from("/c"),
                project_dependencies: vec!["b".to_string()],
                watch_files: vec![],
            },
        ];

        let layers = compute_layers(&projects);

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
        let projects = vec![
            Project {
                name: "a".to_string(),
                dir: Utf8PathBuf::from("/a"),
                project_dependencies: vec![],
                watch_files: vec![],
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec!["a".to_string()],
                watch_files: vec![],
            },
            Project {
                name: "c".to_string(),
                dir: Utf8PathBuf::from("/c"),
                project_dependencies: vec!["a".to_string()],
                watch_files: vec![],
            },
            Project {
                name: "d".to_string(),
                dir: Utf8PathBuf::from("/d"),
                project_dependencies: vec!["b".to_string(), "c".to_string()],
                watch_files: vec![],
            },
        ];

        let layers = compute_layers(&projects);

        assert_eq!(layers.get("a"), Some(&0));
        assert_eq!(layers.get("b"), Some(&1));
        assert_eq!(layers.get("c"), Some(&1)); // Same layer as b (parallel)
        assert_eq!(layers.get("d"), Some(&2));
    }

    #[test]
    fn test_compute_layers_unknown_dependencies() {
        // Project with dependency on non-existent project
        let projects = vec![
            Project {
                name: "a".to_string(),
                dir: Utf8PathBuf::from("/a"),
                project_dependencies: vec![],
                watch_files: vec![],
            },
            Project {
                name: "b".to_string(),
                dir: Utf8PathBuf::from("/b"),
                project_dependencies: vec!["a".to_string(), "nonexistent".to_string()],
                watch_files: vec![],
            },
        ];

        let layers = compute_layers(&projects);

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
            },
            Project {
                name: "app".to_string(),
                dir: Utf8PathBuf::from("/repo/app"),
                project_dependencies: vec!["vpc".to_string()],
                watch_files: vec![],
            },
        ];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Atlantis, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        // vpc should be group 0, app should be group 1
        let vpc = parsed["projects"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "vpc")
            .unwrap();
        let app = parsed["projects"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "app")
            .unwrap();

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
            },
            Project {
                name: "app".to_string(),
                dir: Utf8PathBuf::from("/repo/app"),
                project_dependencies: vec!["vpc".to_string()],
                watch_files: vec![],
            },
        ];
        let config = OutputConfig::default();

        let output = generate_output(&projects, OutputFormat::Digger, &config).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&output).unwrap();

        let vpc = parsed["projects"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "vpc")
            .unwrap();
        let app = parsed["projects"]
            .as_sequence()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "app")
            .unwrap();

        assert_eq!(vpc["layer"], 0);
        assert_eq!(app["layer"], 1);
    }
}
