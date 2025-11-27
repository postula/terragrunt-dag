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

/// Cached config data
#[derive(Debug, Clone)]
pub(crate) struct CachedConfig {
    pub deps: Vec<ExtractedDep>,
    pub has_terraform_block: bool,
}

/// Cache for parsed terragrunt configs.
///
/// Thread-safe for parallel processing with rayon.
/// Caches parsed configs to avoid re-parsing the same file.
pub struct ParseCache {
    /// Map from canonicalized file path to parsed config
    cache: DashMap<Utf8PathBuf, CachedConfig>,
}

impl ParseCache {
    /// Create a new empty parse cache
    pub fn new() -> Self {
        Self {
            cache: DashMap::new(),
        }
    }

    /// Get or parse a config file, returning the full config.
    ///
    /// Returns cached result if available, otherwise parses and caches.
    pub(crate) fn get_or_parse_config(
        &self,
        path: &Utf8Path,
    ) -> Result<CachedConfig, ProcessError> {
        use crate::parser::parse_terragrunt_file;

        // Normalize path for cache key
        let key = path
            .canonicalize_utf8()
            .unwrap_or_else(|_| path.to_path_buf());

        // Check cache first
        if let Some(config) = self.cache.get(&key) {
            return Ok(config.clone());
        }

        // Parse and cache
        let parsed = parse_terragrunt_file(path)?;
        let cached = CachedConfig {
            deps: parsed.deps,
            has_terraform_block: parsed.has_terraform_block,
        };
        self.cache.insert(key, cached.clone());
        Ok(cached)
    }

    /// Get or parse a config file, returning just the deps (for backwards compatibility).
    ///
    /// Returns cached result if available, otherwise parses and caches.
    pub fn get_or_parse(&self, path: &Utf8Path) -> Result<Vec<ExtractedDep>, ProcessError> {
        let config = self.get_or_parse_config(path)?;
        Ok(config.deps)
    }

    /// Get cache statistics (for debugging/logging)
    ///
    /// Returns (entries, total_deps)
    pub fn stats(&self) -> (usize, usize) {
        let entries = self.cache.len();
        let total_deps: usize = self.cache.iter().map(|e| e.value().deps.len()).sum();
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
///
/// If cascade=true, includes transitive dependencies (dependencies of dependencies).
pub fn process_all_projects(
    project_dirs: Vec<Utf8PathBuf>,
    cache: &ParseCache,
    cascade: bool,
) -> Vec<ProjectResult> {
    project_dirs
        .into_par_iter()
        .map(
            |dir| match process_project_with_deps(&dir, cache, cascade) {
                Ok(project) => ProjectResult::Ok(project),
                Err(e) => ProjectResult::Err {
                    path: dir,
                    error: e.to_string(),
                },
            },
        )
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
        has_terraform_source: false,
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
/// 6. If cascade=true, includes transitive dependencies (dependencies of dependencies)
///
/// Uses the provided cache for parsed configs to avoid re-parsing.
pub fn process_project_with_deps(
    project_dir: &Utf8Path,
    cache: &ParseCache,
    cascade: bool,
) -> Result<Project, ProcessError> {
    use std::collections::HashSet;

    let mut visited = HashSet::new();
    let mut project_dependencies: Vec<String> = Vec::new();
    let mut watch_files: Vec<Utf8PathBuf> = Vec::new();
    let mut has_terraform_source = false; // Tracks if any terraform block found (with or without source)

    // Start recursive loading from the project's terragrunt.hcl
    // Dependencies are resolved immediately with correct context during recursion
    let terragrunt_file = project_dir.join("terragrunt.hcl");
    let mut ctx = LoadContext {
        project_dir,
        visited: &mut visited,
        cache,
        project_dependencies: &mut project_dependencies,
        watch_files: &mut watch_files,
        has_terraform_source: &mut has_terraform_source,
        cascade,
    };
    load_config_recursive(&terragrunt_file, &mut ctx)?;

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
        has_terraform_source,
    })
}

/// Context for collecting dependencies during recursive config loading.
struct LoadContext<'a> {
    project_dir: &'a Utf8Path,
    visited: &'a mut std::collections::HashSet<Utf8PathBuf>,
    cache: &'a ParseCache,
    project_dependencies: &'a mut Vec<String>,
    watch_files: &'a mut Vec<Utf8PathBuf>,
    has_terraform_source: &'a mut bool,
    cascade: bool,
}

/// Recursively load a config file and collect its dependencies.
///
/// This function resolves dependencies immediately using the correct context:
/// - Relative paths are resolved from the config file's directory (config_dir)
/// - find_in_parent_folders uses the original project directory (project_dir)
///
/// This matches terragrunt's behavior where included configs use the project's
/// context for parent folder lookups.
///
/// If cascade=true, also includes transitive dependencies (dependencies of dependencies).
fn load_config_recursive(
    config_path: &Utf8Path,
    ctx: &mut LoadContext,
) -> Result<(), ProcessError> {
    use crate::parser::DependencyKind;
    use crate::resolver::ResolveContext;

    // Normalize path for consistent visited tracking
    let normalized = config_path
        .canonicalize_utf8()
        .unwrap_or_else(|_| config_path.to_path_buf());

    // Check for circular reference (within this project's recursion)
    if !ctx.visited.insert(normalized.clone()) {
        // Already visited this file, skip to prevent infinite loop
        return Ok(());
    }

    // Check if file exists
    if !config_path.exists() {
        // File doesn't exist - skip silently (might be optional)
        return Ok(());
    }

    // Use cache instead of direct parsing
    let config = ctx.cache.get_or_parse_config(config_path)?;

    // If this config has a terraform block, mark the project as having one
    if config.has_terraform_block {
        *ctx.has_terraform_source = true;
    }

    let deps = config.deps;

    // Create resolver context:
    // - config_dir: the current file's directory (for relative path resolution)
    // - project_dir: the original project directory (for find_in_parent_folders)
    let config_dir = config_path
        .parent()
        .unwrap_or(ctx.project_dir)
        .to_path_buf();
    let resolve_ctx =
        ResolveContext::for_included_config(config_dir.clone(), ctx.project_dir.to_path_buf());

    // Process each dependency
    for dep in deps {
        match dep.kind {
            DependencyKind::Include | DependencyKind::ReadConfig => {
                // Resolve the path
                if let Some(resolved) = resolve_ctx.resolve(&dep.path) {
                    // Add the config file itself as a watch file
                    ctx.watch_files.push(resolved.clone());

                    // Recursively load the referenced config
                    // Pass the SAME project_dir to preserve find_in_parent_folders context
                    load_config_recursive(&resolved, ctx)?;
                }
            }
            DependencyKind::Project => {
                // Resolve NOW with correct context
                // Store absolute path as string - output module will convert to name
                if let Some(resolved) = resolve_ctx.resolve(&dep.path) {
                    ctx.project_dependencies.push(resolved.to_string());

                    // Add the dependency's files to watch files so changes trigger this project
                    // We watch both HCL and TF files because:
                    // - HCL changes might alter dependencies/config
                    // - TF changes might alter outputs that this project consumes
                    ctx.watch_files.push(resolved.join("**/*.hcl"));
                    ctx.watch_files.push(resolved.join("**/*.tf*"));
                }
            }
            DependencyKind::TerraformSource => {
                // Resolve terraform source as a watch file
                if let Some(resolved) = resolve_ctx.resolve(&dep.path) {
                    ctx.watch_files.push(resolved);
                }
            }
            DependencyKind::FileRead => {
                // Resolve NOW with correct context
                if let Some(resolved) = resolve_ctx.resolve(&dep.path) {
                    ctx.watch_files.push(resolved);
                }
            }
        }
    }

    // After processing all direct dependencies, cascade if enabled
    if ctx.cascade {
        use std::collections::HashSet;
        use std::collections::VecDeque;

        // Track which dependencies we've already processed to prevent infinite loops
        let mut processed = HashSet::new();

        // Use a queue for iterative BFS instead of recursive cascading
        let mut to_process: VecDeque<String> = ctx.project_dependencies.iter().cloned().collect();

        while let Some(dep_path) = to_process.pop_front() {
            // Parse the dependency's path to get its directory
            let dep_dir = if dep_path.ends_with("terragrunt.hcl") {
                Utf8PathBuf::from(
                    dep_path
                        .strip_suffix("/terragrunt.hcl")
                        .unwrap_or(&dep_path),
                )
            } else if dep_path.ends_with(".hcl") {
                // Generic .hcl file, take parent directory
                Utf8PathBuf::from(&dep_path)
                    .parent()
                    .unwrap_or(Utf8Path::new(&dep_path))
                    .to_path_buf()
            } else {
                // Assume it's already a directory path
                Utf8PathBuf::from(&dep_path)
            };

            // Recursively get this dependency's dependencies
            let dep_hcl_path = if dep_path.ends_with(".hcl") {
                Utf8PathBuf::from(&dep_path)
            } else {
                dep_dir.join("terragrunt.hcl")
            };

            // Normalize path to check if we've already processed this project
            let normalized_dep_path = dep_hcl_path
                .canonicalize_utf8()
                .unwrap_or_else(|_| dep_hcl_path.clone());

            // Skip if we've already processed this dependency (prevents infinite loops)
            if !processed.insert(normalized_dep_path.clone()) {
                continue;
            }

            if dep_hcl_path.exists() {
                // Use a new visited set for the dependency's subtree
                let mut dep_visited = HashSet::new();
                let mut dep_project_deps = Vec::new();
                let mut dep_watch_files = Vec::new();
                let mut dep_has_terraform = false;

                // Load this dependency WITHOUT cascading (we'll handle cascading iteratively)
                let mut dep_ctx = LoadContext {
                    project_dir: &dep_dir,
                    visited: &mut dep_visited,
                    cache: ctx.cache,
                    project_dependencies: &mut dep_project_deps,
                    watch_files: &mut dep_watch_files,
                    has_terraform_source: &mut dep_has_terraform,
                    cascade: false, // Don't cascade recursively - we handle it iteratively here
                };
                load_config_recursive(&dep_hcl_path, &mut dep_ctx)?;

                // Add transitive dependencies to both our list and the queue for further processing
                for transitive_dep in dep_project_deps {
                    if !ctx.project_dependencies.contains(&transitive_dep) {
                        ctx.project_dependencies.push(transitive_dep.clone());
                        to_process.push_back(transitive_dep);
                    }
                }

                // Also cascade watch files (avoiding duplicates)
                for transitive_watch in dep_watch_files {
                    if !ctx.watch_files.contains(&transitive_watch) {
                        ctx.watch_files.push(transitive_watch);
                    }
                }
            }
        }
    }

    Ok(())
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

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

        // Should have dependencies from both app/terragrunt.hcl AND common.hcl
        // app has: dependency "rds"
        // common.hcl has: dependency "shared_vpc"
        // Dependencies are now absolute paths
        assert_eq!(project.project_dependencies.len(), 2);
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("rds")),
            "Should have rds dependency. Got: {:?}",
            project.project_dependencies
        );
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("vpc")),
            "Should have vpc dependency. Got: {:?}",
            project.project_dependencies
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

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

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

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

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
        let result = process_project_with_deps(&project_dir, &cache, false);

        // Should succeed (not hang or stack overflow)
        assert!(result.is_ok());
    }

    #[test]
    fn test_process_deduplicates_watch_files() {
        // If the same file is referenced multiple times, it should appear once
        let project_dir = processor_fixture_path("with_read_terragrunt_config/app");
        let cache = ParseCache::new();

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

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
        let project1 = process_project_with_deps(&project_dir, &cache, false)
            .expect("first processing failed");
        let project2 = process_project_with_deps(&project_dir, &cache, false)
            .expect("second processing failed");

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

        let results = process_all_projects(projects, &cache, false);

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
        let _ = process_project_with_deps(&project_dir, &cache, false);

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
        let result = process_project_with_deps(&project_dir, &cache, false);
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

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

        // Verify dependency from common.hcl is present
        // Dependencies are now absolute paths
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("vpc")),
            "Should have vpc dependency from common.hcl. Got: {:?}",
            project.project_dependencies
        );

        // Verify dependency from app/terragrunt.hcl is present
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("rds")),
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

    // ============== Terraform source detection tests ==============

    #[test]
    fn test_project_without_terraform_source_marked() {
        // Project with empty terragrunt.hcl (no terraform block, no includes)
        let cache = ParseCache::new();
        let project_dir = processor_fixture_path("empty_no_terraform");

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

        // Should be marked as having no terraform source
        assert!(
            !project.has_terraform_source,
            "Project without terraform block should have has_terraform_source=false"
        );
    }

    #[test]
    fn test_project_with_terraform_source_marked() {
        // Project with terraform block
        let cache = ParseCache::new();
        let project_dir = processor_fixture_path("has_terraform_source");

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

        // Should be marked as having terraform source
        assert!(
            project.has_terraform_source,
            "Project with terraform block should have has_terraform_source=true"
        );
    }

    #[test]
    fn test_project_with_include_terraform_source_marked() {
        // Project that gets terraform source through include
        let cache = ParseCache::new();
        let project_dir = processor_fixture_path("with_include_deps/live/prod/app");

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

        // Should be marked as having terraform source (from included file)
        assert!(
            project.has_terraform_source,
            "Project with terraform source in include should have has_terraform_source=true"
        );
    }

    #[test]
    fn test_project_with_empty_terraform_block_marked() {
        // Project with terraform {} block but no source attribute (uses generate blocks)
        // This should be marked as having a terraform block
        let cache = ParseCache::new();

        // Create a test fixture directory
        let project_dir = processor_fixture_path("empty_terraform_block");

        // This test will fail initially because we haven't created the fixture yet
        // and because has_terraform_source is only set when source attribute exists
        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

        // Should be marked as having terraform block
        assert!(
            project.has_terraform_source,
            "Project with empty terraform block should have has_terraform_source=true (uses generate blocks)"
        );
    }

    #[test]
    fn test_dependency_names_are_absolute_paths() {
        // This test verifies that dependencies are stored as absolute paths
        // in the Project struct, NOT as derived names.
        // The output module will convert paths to names using base_dir.
        //
        // Structure:
        // dependency_naming/
        //   platform/
        //     vpc/
        //     load_balancer/ -> depends on ../vpc
        //   apps/
        //     ecs_instances/ -> depends on ../../platform/vpc, ../../platform/load_balancer
        //
        // Before fix: ecs_instances.project_dependencies = ["platform_vpc", "platform_load_balancer"]
        // After fix: ecs_instances.project_dependencies = ["/abs/path/to/platform/vpc", "/abs/path/to/platform/load_balancer"]

        let cache = ParseCache::new();
        let fixture_root = processor_fixture_path("dependency_naming");
        let project_dir = fixture_root.join("apps/ecs_instances");

        let project = process_project_with_deps(&project_dir, &cache, false)
            .expect("should process successfully");

        // Dependencies should be absolute paths, not derived names
        assert_eq!(
            project.project_dependencies.len(),
            2,
            "Should have 2 dependencies"
        );

        // Check that dependencies are absolute paths (contain full path components)
        for dep in &project.project_dependencies {
            assert!(
                dep.contains("platform"),
                "Dependency should be absolute path containing 'platform', got: {}",
                dep
            );
            assert!(
                dep.starts_with('/') || dep.contains(":\\"),
                "Dependency should be absolute path, got: {}",
                dep
            );
        }

        // Should have paths ending in vpc and load_balancer
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("vpc")),
            "Should have vpc dependency. Got: {:?}",
            project.project_dependencies
        );
        assert!(
            project
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("load_balancer")),
            "Should have load_balancer dependency. Got: {:?}",
            project.project_dependencies
        );
    }

    // ============== Cascade Dependencies Tests (TDD - FAILING) ==============

    /// Test basic cascade behavior with a 3-level chain: A→B→C
    ///
    /// With cascade=true (default):
    /// - A should get dependencies: [B, C]
    /// - B should get dependencies: [C]
    /// - C should get dependencies: []
    ///
    /// Without cascade (direct only):
    /// - A should get dependencies: [B]
    /// - B should get dependencies: [C]
    /// - C should get dependencies: []
    #[test]
    fn test_cascade_basic_chain() {
        let cache = ParseCache::new();
        let fixture_root = processor_fixture_path("cascade/chain");

        // Test cascade=true (default behavior)
        let project_a_dir = fixture_root.join("a");
        let project_a = process_project_with_deps(&project_a_dir, &cache, true)
            .expect("should process successfully");

        // A should have both B and C as dependencies (transitive closure)
        assert_eq!(
            project_a.project_dependencies.len(),
            2,
            "A should have 2 dependencies with cascade=true. Got: {:?}",
            project_a.project_dependencies
        );

        assert!(
            project_a
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("b")),
            "A should have direct dependency B. Got: {:?}",
            project_a.project_dependencies
        );

        assert!(
            project_a
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("c")),
            "A should have transitive dependency C with cascade=true. Got: {:?}",
            project_a.project_dependencies
        );

        // Verify B also has C
        let project_b_dir = fixture_root.join("b");
        let project_b = process_project_with_deps(&project_b_dir, &cache, true)
            .expect("should process successfully");

        assert_eq!(
            project_b.project_dependencies.len(),
            1,
            "B should have 1 dependency. Got: {:?}",
            project_b.project_dependencies
        );

        assert!(
            project_b
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("c")),
            "B should have dependency C. Got: {:?}",
            project_b.project_dependencies
        );

        // Verify C has no dependencies
        let project_c_dir = fixture_root.join("c");
        let project_c = process_project_with_deps(&project_c_dir, &cache, true)
            .expect("should process successfully");

        assert_eq!(
            project_c.project_dependencies.len(),
            0,
            "C should have no dependencies. Got: {:?}",
            project_c.project_dependencies
        );
    }

    /// Test cascade disabled: only direct dependencies should be included
    #[test]
    fn test_cascade_disabled_direct_only() {
        let cache = ParseCache::new();
        let fixture_root = processor_fixture_path("cascade/chain");

        // Test with cascade=false
        let project_a_dir = fixture_root.join("a");
        let project_a = process_project_with_deps(&project_a_dir, &cache, false)
            .expect("should process successfully");

        // A should have ONLY B (direct dependency), not C
        assert_eq!(
            project_a.project_dependencies.len(),
            1,
            "A should have only 1 direct dependency with cascade=false. Got: {:?}",
            project_a.project_dependencies
        );

        assert!(
            project_a
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("b")),
            "A should have direct dependency B. Got: {:?}",
            project_a.project_dependencies
        );

        assert!(
            !project_a
                .project_dependencies
                .iter()
                .any(|d| d.ends_with("c")),
            "A should NOT have transitive dependency C with cascade=false. Got: {:?}",
            project_a.project_dependencies
        );
    }

    /// Test diamond dependency pattern: A→B, A→C, B→D, C→D
    ///
    /// With cascade=true:
    /// - A should get dependencies: [B, C, D] (D appears via both B and C)
    /// - No duplicates should appear in the final list
    #[test]
    fn test_cascade_diamond_no_duplicates() {
        let cache = ParseCache::new();
        let fixture_root = processor_fixture_path("cascade/diamond");

        let project_a_dir = fixture_root.join("a");
        let project_a = process_project_with_deps(&project_a_dir, &cache, true)
            .expect("should process successfully");

        // A should have B, C, and D (D deduplicated even though it comes from both B and C)
        assert_eq!(
            project_a.project_dependencies.len(),
            3,
            "A should have 3 unique dependencies (B, C, D). Got: {:?}",
            project_a.project_dependencies
        );

        // Verify each dependency appears exactly once
        let deps = &project_a.project_dependencies;
        assert!(
            deps.iter().filter(|d| d.ends_with("b")).count() == 1,
            "B should appear exactly once. Got: {:?}",
            deps
        );
        assert!(
            deps.iter().filter(|d| d.ends_with("c")).count() == 1,
            "C should appear exactly once. Got: {:?}",
            deps
        );
        assert!(
            deps.iter().filter(|d| d.ends_with("d")).count() == 1,
            "D should appear exactly once (deduplicated). Got: {:?}",
            deps
        );
    }

    /// Test that watch files cascade from dependencies
    ///
    /// Structure:
    /// - A depends on B
    /// - B depends on C and reads config.yaml
    /// - C reads data.yaml
    ///
    /// With cascade=true:
    /// - A should have watch_files from B and C (config.yaml, data.yaml)
    #[test]
    fn test_cascade_watch_files() {
        let cache = ParseCache::new();
        let fixture_root = processor_fixture_path("cascade/with_watch_files");

        let project_a_dir = fixture_root.join("a");
        let project_a = process_project_with_deps(&project_a_dir, &cache, true)
            .expect("should process successfully");

        // A should have dependencies: B, C
        assert_eq!(
            project_a.project_dependencies.len(),
            2,
            "A should have 2 dependencies. Got: {:?}",
            project_a.project_dependencies
        );

        // A should have watch files from B and C
        assert!(
            project_a
                .watch_files
                .iter()
                .any(|f| f.ends_with("config.yaml")),
            "A should have config.yaml from B's watch files. Got: {:?}",
            project_a.watch_files
        );

        assert!(
            project_a
                .watch_files
                .iter()
                .any(|f| f.ends_with("data.yaml")),
            "A should have data.yaml from C's watch files. Got: {:?}",
            project_a.watch_files
        );

        // Test with cascade=false - should NOT include transitive watch files
        let cache_no_cascade = ParseCache::new();
        let project_a_no_cascade =
            process_project_with_deps(&project_a_dir, &cache_no_cascade, false)
                .expect("should process successfully");

        // With cascade=false, A should only have its own watch files (none in this case)
        // and NOT the watch files from B and C
        assert!(
            !project_a_no_cascade
                .watch_files
                .iter()
                .any(|f| f.ends_with("config.yaml")),
            "A should NOT have config.yaml from B with cascade=false. Got: {:?}",
            project_a_no_cascade.watch_files
        );

        assert!(
            !project_a_no_cascade
                .watch_files
                .iter()
                .any(|f| f.ends_with("data.yaml")),
            "A should NOT have data.yaml from C with cascade=false. Got: {:?}",
            project_a_no_cascade.watch_files
        );
    }

    /// Test that circular dependencies don't cause infinite loops with cascade
    ///
    /// Structure: A→B→C→A (circular)
    ///
    /// With cascade=true:
    /// - Should detect the cycle and not loop infinitely
    /// - Should collect all reachable dependencies without stack overflow
    #[test]
    fn test_cascade_circular_no_infinite_loop() {
        let cache = ParseCache::new();
        let fixture_root = processor_fixture_path("cascade/circular");

        let project_a_dir = fixture_root.join("a");

        // Should complete without hanging or stack overflow
        let result = process_project_with_deps(&project_a_dir, &cache, true);

        // Should succeed (not hang or panic)
        assert!(
            result.is_ok(),
            "Should handle circular dependencies with cascade enabled"
        );

        let project_a = result.unwrap();

        // Should have detected and handled the circular reference
        // Dependencies should include at least B and C, but should be finite
        assert!(
            project_a.project_dependencies.len() >= 2,
            "Should have collected some dependencies before detecting cycle. Got: {:?}",
            project_a.project_dependencies
        );

        assert!(
            project_a.project_dependencies.len() <= 3,
            "Should not have infinite dependencies due to cycle detection. Got: {:?}",
            project_a.project_dependencies
        );
    }

    /// Test cascade with complex multi-level dependencies
    ///
    /// This tests the transitive closure works correctly across multiple levels
    /// and that deduplication happens properly.
    #[test]
    fn test_cascade_complex_transitive_closure() {
        let cache = ParseCache::new();
        let fixture_root = processor_fixture_path("cascade/diamond");

        // Test each node to verify correct transitive closure
        let project_a_dir = fixture_root.join("a");
        let project_a = process_project_with_deps(&project_a_dir, &cache, true)
            .expect("should process successfully");

        // A → [B, C] direct, D transitive via both
        assert_eq!(project_a.project_dependencies.len(), 3);

        let project_b_dir = fixture_root.join("b");
        let project_b = process_project_with_deps(&project_b_dir, &cache, true)
            .expect("should process successfully");

        // B → [D] direct
        assert_eq!(
            project_b.project_dependencies.len(),
            1,
            "B should have 1 dependency. Got: {:?}",
            project_b.project_dependencies
        );

        let project_c_dir = fixture_root.join("c");
        let project_c = process_project_with_deps(&project_c_dir, &cache, true)
            .expect("should process successfully");

        // C → [D] direct
        assert_eq!(
            project_c.project_dependencies.len(),
            1,
            "C should have 1 dependency. Got: {:?}",
            project_c.project_dependencies
        );

        let project_d_dir = fixture_root.join("d");
        let project_d = process_project_with_deps(&project_d_dir, &cache, true)
            .expect("should process successfully");

        // D → [] no dependencies
        assert_eq!(
            project_d.project_dependencies.len(),
            0,
            "D should have no dependencies. Got: {:?}",
            project_d.project_dependencies
        );
    }
}
