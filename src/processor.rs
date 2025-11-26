//! Parallel processing of terragrunt projects.
//!
//! This module provides simple, testable parallel processing of discovered
//! terragrunt projects using rayon.

use camino::{Utf8Path, Utf8PathBuf};
use rayon::prelude::*;
use thiserror::Error;

use crate::project::Project;

/// Errors that can occur during project processing
#[derive(Error, Debug)]
pub enum ProcessError {
    #[error("Failed to parse config: {0}")]
    ParseError(#[from] crate::parser::ParseError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Circular reference detected: {0}")]
    CircularReference(String),
}

/// Result of processing a single project
#[derive(Debug)]
pub enum ProjectResult {
    /// Successfully processed project
    Ok(Project),
    /// Failed to process - path and error message
    Err { path: Utf8PathBuf, error: String },
}

/// Process discovered project paths in parallel.
///
/// Takes a list of project directories (containing terragrunt.hcl) and
/// processes each one in parallel, returning results for all.
pub fn process_projects(paths: Vec<Utf8PathBuf>) -> Vec<ProjectResult> {
    paths.into_par_iter().map(process_single_project).collect()
}

/// Process a single project directory.
///
/// This is the integration point where parsing, resolution, and project
/// building will happen. For now, creates a placeholder project.
fn process_single_project(path: Utf8PathBuf) -> ProjectResult {
    // Derive project name from path
    let name = derive_project_name(&path);

    ProjectResult::Ok(Project {
        name,
        dir: path,
        project_dependencies: vec![],
        watch_files: vec![],
    })
}

/// Derive a project name from its path.
///
/// Convention: use path components joined with underscores.
/// Skips common prefixes like "live", "terragrunt", "infrastructure", "repo".
///
/// Examples:
/// - "/repo/live/prod/vpc" -> "prod_vpc"
/// - "/terragrunt/staging/app" -> "staging_app"
fn derive_project_name(path: &Utf8PathBuf) -> String {
    let components: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            camino::Utf8Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    // Skip common root directories and repository names
    let skip_prefixes = [
        "live",
        "terragrunt",
        "infrastructure",
        "infra",
        "repo",
        "projects",
    ];

    // Filter out skip prefixes from all components
    let meaningful: Vec<&str> = components
        .iter()
        .filter(|c| !skip_prefixes.contains(c))
        .copied()
        .collect();

    if meaningful.is_empty() {
        "unknown".to_string()
    } else {
        meaningful.join("_")
    }
}

/// Process a single project with recursive config loading.
///
/// This function:
/// 1. Parses the project's terragrunt.hcl
/// 2. Resolves all paths
/// 3. Recursively loads read_terragrunt_config and include targets
/// 4. Merges dependencies and watch files
/// 5. Deduplicates the final result
pub fn process_project_with_deps(project_dir: &Utf8Path) -> Result<Project, ProcessError> {
    use crate::parser::{DependencyKind, ExtractedDep};
    use crate::resolver::ResolveContext;
    use std::collections::HashSet;

    let mut visited = HashSet::new();
    let mut all_deps: Vec<ExtractedDep> = Vec::new();
    let mut watch_files: Vec<Utf8PathBuf> = Vec::new();

    // Start recursive loading from the project's terragrunt.hcl
    let terragrunt_file = project_dir.join("terragrunt.hcl");
    load_config_recursive(
        &terragrunt_file,
        project_dir,
        &mut visited,
        &mut all_deps,
        &mut watch_files,
    )?;

    // Convert ExtractedDeps to project dependencies and watch files
    let ctx = ResolveContext::new(project_dir.to_path_buf());

    let mut project_dependencies: Vec<String> = Vec::new();

    for dep in &all_deps {
        match dep.kind {
            DependencyKind::Project => {
                // Resolve path and add to project dependencies
                if let Some(resolved) = ctx.resolve(&dep.path) {
                    // Use relative path or derive name
                    let dep_name = derive_dependency_name(&resolved, project_dir);
                    project_dependencies.push(dep_name);
                }
            }
            DependencyKind::TerraformSource => {
                // Local terraform source is a watch file (directory)
                if let Some(resolved) = ctx.resolve(&dep.path) {
                    watch_files.push(resolved);
                }
            }
            DependencyKind::FileRead => {
                // File reads are watch files
                if let Some(resolved) = ctx.resolve(&dep.path) {
                    watch_files.push(resolved);
                }
            }
            // Include and ReadConfig are handled during recursive loading
            DependencyKind::Include | DependencyKind::ReadConfig => {}
        }
    }

    // Deduplicate
    project_dependencies.sort();
    project_dependencies.dedup();

    watch_files.sort();
    watch_files.dedup();

    Ok(Project {
        name: derive_project_name(&project_dir.to_path_buf()),
        dir: project_dir.to_path_buf(),
        project_dependencies,
        watch_files,
    })
}

/// Recursively load a config file and collect its dependencies.
fn load_config_recursive(
    config_path: &Utf8Path,
    base_dir: &Utf8Path, // Directory for relative path resolution
    visited: &mut std::collections::HashSet<Utf8PathBuf>,
    all_deps: &mut Vec<crate::parser::ExtractedDep>,
    watch_files: &mut Vec<Utf8PathBuf>,
) -> Result<(), ProcessError> {
    use crate::parser::{DependencyKind, parse_terragrunt_file};
    use crate::resolver::ResolveContext;

    // Normalize path for consistent visited tracking
    let normalized = config_path
        .canonicalize_utf8()
        .unwrap_or_else(|_| config_path.to_path_buf());

    // Check for circular reference
    if !visited.insert(normalized.clone()) {
        // Already visited this file, skip to prevent infinite loop
        return Ok(());
    }

    // Check if file exists
    if !config_path.exists() {
        // File doesn't exist - skip silently (might be optional)
        return Ok(());
    }

    // Parse the config file
    let config = parse_terragrunt_file(config_path)?;

    // Create resolver context for this file's directory
    let config_dir = config_path.parent().unwrap_or(base_dir);
    let ctx = ResolveContext::new(config_dir.to_path_buf());

    // Process each dependency
    for dep in config.deps {
        match dep.kind {
            DependencyKind::Include | DependencyKind::ReadConfig => {
                // Resolve the path
                if let Some(resolved) = ctx.resolve(&dep.path) {
                    // Add the config file itself as a watch file
                    watch_files.push(resolved.clone());

                    // Recursively load the referenced config
                    let referenced_dir = resolved.parent().unwrap_or(config_dir);
                    load_config_recursive(
                        &resolved,
                        referenced_dir,
                        visited,
                        all_deps,
                        watch_files,
                    )?;
                }
            }
            _ => {
                // Collect other dependencies for later processing
                all_deps.push(dep);
            }
        }
    }

    Ok(())
}

/// Derive a dependency name from a resolved path.
fn derive_dependency_name(dep_path: &Utf8Path, project_dir: &Utf8Path) -> String {
    // Try to make it relative to project for readability
    if let Ok(relative) = dep_path.strip_prefix(project_dir.parent().unwrap_or(project_dir)) {
        relative.as_str().replace(['/', '\\'], "_")
    } else {
        // Fallback: use last components
        dep_path
            .components()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn test_process_empty_list() {
        let result = process_projects(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_process_single_project() {
        let paths = vec![Utf8PathBuf::from("/repo/live/prod/vpc")];
        let results = process_projects(paths);

        assert_eq!(results.len(), 1);
        match &results[0] {
            ProjectResult::Ok(project) => {
                assert_eq!(project.dir, Utf8PathBuf::from("/repo/live/prod/vpc"));
                // Name should skip "live" and "repo" prefixes
                assert_eq!(project.name, "prod_vpc");
            }
            ProjectResult::Err { .. } => panic!("Expected Ok result"),
        }
    }

    #[test]
    fn test_process_multiple_projects_parallel() {
        let paths: Vec<Utf8PathBuf> = (0..100)
            .map(|i| Utf8PathBuf::from(format!("/repo/live/env{}/app", i)))
            .collect();

        let results = process_projects(paths);

        assert_eq!(results.len(), 100);
        // All should succeed (placeholder implementation)
        assert!(results.iter().all(|r| matches!(r, ProjectResult::Ok(_))));
    }

    #[rstest]
    #[case("/repo/live/prod/vpc", "prod_vpc")]
    #[case("/repo/terragrunt/staging/app", "staging_app")]
    #[case("/repo/prod/vpc", "prod_vpc")]
    #[case("/repo/vpc", "vpc")]
    #[case("/projects/infra/staging/database", "staging_database")]
    #[case("/infrastructure/prod/network", "prod_network")]
    #[case("/some/path/without/skip/prefix", "some_path_without_skip_prefix")]
    fn test_derive_project_name(#[case] input_path: &str, #[case] expected_name: &str) {
        assert_eq!(
            derive_project_name(&Utf8PathBuf::from(input_path)),
            expected_name
        );
    }

    // ============== Recursive config loading tests ==============

    fn processor_fixture_path(name: &str) -> Utf8PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir)
            .join("tests/fixtures/processor")
            .join(name)
    }

    #[test]
    fn test_process_with_read_terragrunt_config() {
        let project_dir = processor_fixture_path("with_read_terragrunt_config/app");

        let project = process_project_with_deps(&project_dir).expect("should process successfully");

        // Should have dependencies from both app/terragrunt.hcl AND common.hcl
        // app has: dependency "rds"
        // common.hcl has: dependency "shared_vpc"
        assert_eq!(project.project_dependencies.len(), 2);
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.contains("rds"))
        );
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.contains("shared_vpc"))
        );

        // Watch files should include:
        // - common.hcl (the read_terragrunt_config target)
        // - data/common.yaml (file() in common.hcl)
        assert!(
            project
                .watch_files
                .iter()
                .any(|f| f.ends_with("common.hcl"))
        );
        assert!(
            project
                .watch_files
                .iter()
                .any(|f| f.ends_with("common.yaml"))
        );
    }

    #[test]
    fn test_process_with_include_loads_deps() {
        let project_dir = processor_fixture_path("with_include_deps/live/prod/app");

        let project = process_project_with_deps(&project_dir).expect("should process successfully");

        // Should have dependency from app/terragrunt.hcl
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.contains("vpc"))
        );

        // Watch files should include:
        // - root.hcl (the include target)
        // - config.yaml (file() in root.hcl)
        assert!(project.watch_files.iter().any(|f| f.ends_with("root.hcl")));
        assert!(
            project
                .watch_files
                .iter()
                .any(|f| f.ends_with("config.yaml"))
        );
    }

    #[test]
    fn test_process_with_nested_read_config() {
        let project_dir = processor_fixture_path("with_nested_read_config/app");

        let project = process_project_with_deps(&project_dir).expect("should process successfully");

        // app → env.hcl → base.hcl
        // Should collect watch files from all levels
        assert!(project.watch_files.iter().any(|f| f.ends_with("env.hcl")));
        assert!(project.watch_files.iter().any(|f| f.ends_with("base.hcl")));
        assert!(project.watch_files.iter().any(|f| f.ends_with("env.yaml")));
        assert!(project.watch_files.iter().any(|f| f.ends_with("base.yaml")));
    }

    #[test]
    fn test_process_with_circular_reference_does_not_loop() {
        let project_dir = processor_fixture_path("with_circular_reference/app");

        // Should complete without infinite loop
        // Circular references should be detected and skipped
        let result = process_project_with_deps(&project_dir);

        // Should succeed (not hang or stack overflow)
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_deduplicates_watch_files() {
        // If the same file is referenced multiple times, it should appear once
        let project_dir = processor_fixture_path("with_read_terragrunt_config/app");

        let project = process_project_with_deps(&project_dir).expect("should process successfully");

        // Check no duplicates in watch_files
        let mut seen = std::collections::HashSet::new();
        for file in &project.watch_files {
            assert!(seen.insert(file.clone()), "Duplicate watch file: {}", file);
        }
    }
}
