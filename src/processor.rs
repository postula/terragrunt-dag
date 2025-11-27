//! Parallel processing of terragrunt projects.
//!
//! This module provides simple, testable parallel processing of discovered
//! terragrunt projects using rayon.

use camino::{Utf8Path, Utf8PathBuf};
use dashmap::DashMap;
use rayon::prelude::*;
use thiserror::Error;

use crate::parser::ExtractedDep;
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

/// Cache for parsed terragrunt configs.
///
/// Thread-safe for parallel processing with rayon.
/// Caches parsed dependency results to avoid re-parsing the same file.
pub struct ParseCache {
    /// Map from canonicalized file path to parsed dependencies
    cache: DashMap<Utf8PathBuf, Vec<ExtractedDep>>,
}

impl ParseCache {
    /// Create a new empty parse cache
    pub fn new() -> Self {
        Self {
            cache: DashMap::new(),
        }
    }

    /// Get or parse a config file.
    ///
    /// Returns cached result if available, otherwise parses and caches.
    pub fn get_or_parse(&self, path: &Utf8Path) -> Result<Vec<ExtractedDep>, ProcessError> {
        use crate::parser::parse_terragrunt_file;

        // Normalize path for cache key
        let key = path
            .canonicalize_utf8()
            .unwrap_or_else(|_| path.to_path_buf());

        // Check cache first
        if let Some(deps) = self.cache.get(&key) {
            return Ok(deps.clone());
        }

        // Parse and cache
        let config = parse_terragrunt_file(path)?;
        let deps = config.deps;
        self.cache.insert(key, deps.clone());
        Ok(deps)
    }

    /// Get cache statistics (for debugging/logging)
    ///
    /// Returns (entries, total_deps)
    pub fn stats(&self) -> (usize, usize) {
        let entries = self.cache.len();
        let total_deps: usize = self.cache.iter().map(|e| e.value().len()).sum();
        (entries, total_deps)
    }
}

impl Default for ParseCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Process discovered project paths in parallel.
///
/// Takes a list of project directories (containing terragrunt.hcl) and
/// processes each one in parallel, returning results for all.
pub fn process_projects(paths: Vec<Utf8PathBuf>) -> Vec<ProjectResult> {
    paths.into_par_iter().map(process_single_project).collect()
}

/// Process all discovered projects in parallel using shared cache.
///
/// Uses a shared ParseCache to avoid re-parsing the same config files
/// when they are referenced by multiple projects.
pub fn process_all_projects(
    project_dirs: Vec<Utf8PathBuf>,
    cache: &ParseCache,
) -> Vec<ProjectResult> {
    project_dirs
        .into_par_iter()
        .map(|dir| match process_project_with_deps(&dir, cache) {
            Ok(project) => ProjectResult::Ok(project),
            Err(e) => ProjectResult::Err {
                path: dir,
                error: e.to_string(),
            },
        })
        .collect()
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
/// 1. Parses the project's terragrunt.hcl (using cache)
/// 2. Recursively loads read_terragrunt_config and include targets
/// 3. Resolves all paths with correct context (relative to their source file)
/// 4. Merges dependencies and watch files
/// 5. Deduplicates the final result
///
/// Uses the provided cache for parsed configs to avoid re-parsing.
pub fn process_project_with_deps(
    project_dir: &Utf8Path,
    cache: &ParseCache,
) -> Result<Project, ProcessError> {
    use std::collections::HashSet;

    let mut visited = HashSet::new();
    let mut project_dependencies: Vec<String> = Vec::new();
    let mut watch_files: Vec<Utf8PathBuf> = Vec::new();

    // Start recursive loading from the project's terragrunt.hcl
    // Dependencies are resolved immediately with correct context during recursion
    let terragrunt_file = project_dir.join("terragrunt.hcl");
    load_config_recursive(
        &terragrunt_file,
        project_dir,
        &mut visited,
        cache,
        &mut project_dependencies,
        &mut watch_files,
    )?;

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
///
/// This function resolves dependencies immediately using the correct context:
/// - Relative paths are resolved from the config file's directory (config_dir)
/// - find_in_parent_folders uses the original project directory (project_dir)
///
/// This matches terragrunt's behavior where included configs use the project's
/// context for parent folder lookups.
fn load_config_recursive(
    config_path: &Utf8Path,
    project_dir: &Utf8Path, // Original project directory (for find_in_parent_folders)
    visited: &mut std::collections::HashSet<Utf8PathBuf>, // For cycle detection
    cache: &ParseCache,     // Shared cache across all projects
    project_dependencies: &mut Vec<String>, // Resolved project dependencies
    watch_files: &mut Vec<Utf8PathBuf>, // Resolved watch files
) -> Result<(), ProcessError> {
    use crate::parser::DependencyKind;
    use crate::resolver::ResolveContext;

    // Normalize path for consistent visited tracking
    let normalized = config_path
        .canonicalize_utf8()
        .unwrap_or_else(|_| config_path.to_path_buf());

    // Check for circular reference (within this project's recursion)
    if !visited.insert(normalized.clone()) {
        // Already visited this file, skip to prevent infinite loop
        return Ok(());
    }

    // Check if file exists
    if !config_path.exists() {
        // File doesn't exist - skip silently (might be optional)
        return Ok(());
    }

    // Use cache instead of direct parsing
    let deps = cache.get_or_parse(config_path)?;

    // Create resolver context:
    // - config_dir: the current file's directory (for relative path resolution)
    // - project_dir: the original project directory (for find_in_parent_folders)
    let config_dir = config_path.parent().unwrap_or(project_dir).to_path_buf();
    let ctx = ResolveContext::for_included_config(config_dir.clone(), project_dir.to_path_buf());

    // Process each dependency
    for dep in deps {
        match dep.kind {
            DependencyKind::Include | DependencyKind::ReadConfig => {
                // Resolve the path
                if let Some(resolved) = ctx.resolve(&dep.path) {
                    // Add the config file itself as a watch file
                    watch_files.push(resolved.clone());

                    // Recursively load the referenced config
                    // Pass the SAME project_dir to preserve find_in_parent_folders context
                    load_config_recursive(
                        &resolved,
                        project_dir, // Keep original project dir!
                        visited,
                        cache,
                        project_dependencies,
                        watch_files,
                    )?;
                }
            }
            DependencyKind::Project => {
                // Resolve NOW with correct context
                if let Some(resolved) = ctx.resolve(&dep.path) {
                    let dep_name = derive_dependency_name(&resolved, project_dir);
                    project_dependencies.push(dep_name);
                }
            }
            DependencyKind::TerraformSource | DependencyKind::FileRead => {
                // Resolve NOW with correct context
                if let Some(resolved) = ctx.resolve(&dep.path) {
                    watch_files.push(resolved);
                }
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
        let cache = ParseCache::new();

        let project =
            process_project_with_deps(&project_dir, &cache).expect("should process successfully");

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
        let cache = ParseCache::new();

        let project =
            process_project_with_deps(&project_dir, &cache).expect("should process successfully");

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
        let cache = ParseCache::new();

        let project =
            process_project_with_deps(&project_dir, &cache).expect("should process successfully");

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
        let cache = ParseCache::new();

        // Should complete without infinite loop
        // Circular references should be detected and skipped
        let result = process_project_with_deps(&project_dir, &cache);

        // Should succeed (not hang or stack overflow)
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_deduplicates_watch_files() {
        // If the same file is referenced multiple times, it should appear once
        let project_dir = processor_fixture_path("with_read_terragrunt_config/app");
        let cache = ParseCache::new();

        let project =
            process_project_with_deps(&project_dir, &cache).expect("should process successfully");

        // Check no duplicates in watch_files
        let mut seen = std::collections::HashSet::new();
        for file in &project.watch_files {
            assert!(seen.insert(file.clone()), "Duplicate watch file: {}", file);
        }
    }

    // ============== ParseCache tests ==============

    #[test]
    fn test_cache_reuses_parsed_config() {
        let cache = ParseCache::new();
        let project_dir = processor_fixture_path("with_read_terragrunt_config/app");

        // Process twice
        let project1 =
            process_project_with_deps(&project_dir, &cache).expect("first processing failed");
        let project2 =
            process_project_with_deps(&project_dir, &cache).expect("second processing failed");

        // Both should have same results
        assert_eq!(project1.project_dependencies, project2.project_dependencies);
        assert_eq!(project1.watch_files, project2.watch_files);

        // Cache should have entries (common.hcl and app/terragrunt.hcl parsed, reused)
        let (entries, total_deps) = cache.stats();
        assert!(entries > 0, "Cache should have entries");

        // Log cache stats for demonstration
        println!(
            "Cache stats after processing twice: {} files cached, {} total dependencies",
            entries, total_deps
        );
    }

    #[test]
    fn test_parallel_processing_shares_cache() {
        let cache = ParseCache::new();

        // Two projects that might share common files
        let projects = vec![
            processor_fixture_path("with_read_terragrunt_config/app"),
            processor_fixture_path("with_nested_read_config/app"),
        ];

        let results = process_all_projects(projects, &cache);

        // All should succeed
        assert_eq!(results.len(), 2);
        assert!(
            results.iter().all(|r| matches!(r, ProjectResult::Ok(_))),
            "All projects should process successfully"
        );

        // Cache should have entries from both projects
        let (entries, total_deps) = cache.stats();
        assert!(entries > 0, "Cache should have entries");

        // Log cache stats for demonstration
        println!(
            "Cache stats after parallel processing: {} files cached, {} total dependencies",
            entries, total_deps
        );
    }

    #[test]
    fn test_cache_stats() {
        let cache = ParseCache::new();
        let project_dir = processor_fixture_path("with_read_terragrunt_config/app");

        // Initially empty
        let (entries, total_deps) = cache.stats();
        assert_eq!(entries, 0);
        assert_eq!(total_deps, 0);

        // After processing, should have entries
        let _ = process_project_with_deps(&project_dir, &cache);

        let (entries_after, _total_deps_after) = cache.stats();
        assert!(
            entries_after > 0,
            "Cache should have entries after processing"
        );
    }

    #[test]
    fn test_cache_with_circular_reference() {
        // Cache should not break circular reference detection
        let cache = ParseCache::new();
        let project_dir = processor_fixture_path("with_circular_reference/app");

        // Should complete without infinite loop even with cache
        let result = process_project_with_deps(&project_dir, &cache);
        assert!(
            result.is_ok(),
            "Should handle circular references with cache"
        );
    }

    #[test]
    fn test_deps_resolved_relative_to_source_file() {
        // This test verifies that dependencies from included files are resolved
        // relative to the included file's directory, not the project directory.
        //
        // Structure:
        // with_read_terragrunt_config/
        //   app/
        //     terragrunt.hcl    -> reads ../common.hcl
        //   common.hcl          -> dependency "../shared/vpc"
        //
        // The dependency "../shared/vpc" in common.hcl should resolve to
        // "<root>/shared/vpc", NOT "app/../shared/vpc".
        //
        // Before fix: would resolve relative to app/ (wrong)
        // After fix: resolves relative to common.hcl's directory (correct)

        let cache = ParseCache::new();
        let project_dir = processor_fixture_path("with_read_terragrunt_config/app");

        let project =
            process_project_with_deps(&project_dir, &cache).expect("should process successfully");

        // Verify dependency from common.hcl is present
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.contains("shared_vpc")),
            "Should have shared_vpc dependency from common.hcl. Got: {:?}",
            project.project_dependencies
        );

        // Verify dependency from app/terragrunt.hcl is present
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.contains("rds")),
            "Should have rds dependency from app/terragrunt.hcl. Got: {:?}",
            project.project_dependencies
        );

        // Verify watch file from common.hcl is present
        assert!(
            project
                .watch_files
                .iter()
                .any(|f| f.ends_with("common.yaml")),
            "Should have common.yaml watch file from common.hcl. Got: {:?}",
            project.watch_files
        );

        // Both dependencies should be resolved correctly
        assert_eq!(
            project.project_dependencies.len(),
            2,
            "Should have exactly 2 dependencies"
        );
    }
}
