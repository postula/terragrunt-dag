//! Parallel processing of terragrunt projects.
//!
//! This module provides simple, testable parallel processing of discovered
//! terragrunt projects using rayon.

use camino::Utf8PathBuf;
use rayon::prelude::*;

use crate::project::Project;

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
}
