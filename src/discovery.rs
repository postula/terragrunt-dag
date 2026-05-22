//! Find terragrunt.hcl and terragrunt.stack.hcl files in a directory tree.

use camino::{Utf8Path, Utf8PathBuf};
use std::io::Result;
use walkdir::{DirEntry, WalkDir};

/// Directories to skip during discovery.
/// Note: `.terragrunt-stack` is intentionally NOT in this list so generated
/// stack units can be discovered when present.
const IGNORED_DIRS: &[&str] =
    &[".terragrunt-cache", ".terraform", ".git", "node_modules", ".venv", "venv", "__pycache__"];

fn is_ignored(entry: &DirEntry) -> bool {
    entry.file_name().to_str().map(|s| IGNORED_DIRS.contains(&s)).unwrap_or(false)
}

/// Files discovered by a single tree walk.
pub struct DiscoveredFiles {
    /// Directories containing a `terragrunt.hcl` file
    pub units: Vec<Utf8PathBuf>,
    /// Paths to `terragrunt.stack.hcl` files
    pub stack_files: Vec<Utf8PathBuf>,
}

/// Discover all `terragrunt.hcl` and `terragrunt.stack.hcl` files under `root`.
pub fn discover_files(root: &Utf8Path) -> Result<DiscoveredFiles> {
    if !root.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("root path does not exist: {}", root)));
    }

    let mut units = Vec::new();
    let mut stack_files = Vec::new();

    for entry in WalkDir::new(root).into_iter().filter_entry(|e| !is_ignored(e)).filter_map(|e| e.ok()) {
        let Some(file_name) = entry.file_name().to_str() else {
            continue;
        };

        match file_name {
            "terragrunt.hcl" => {
                if let Some(path) =
                    Utf8PathBuf::from_path_buf(entry.into_path()).ok().and_then(|p| p.parent().map(|p| p.to_owned()))
                {
                    units.push(path);
                }
            }
            "terragrunt.stack.hcl" => {
                if let Ok(path) = Utf8PathBuf::from_path_buf(entry.into_path()) {
                    stack_files.push(path);
                }
            }
            _ => {}
        }
    }

    Ok(DiscoveredFiles {
        units,
        stack_files,
    })
}

/// Discover all `terragrunt.hcl` directories under `root`.
///
/// Retained as a convenience wrapper for callers/tests that don't need stacks.
pub fn discover_projects(root: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    Ok(discover_files(root)?.units)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use camino::Utf8PathBuf;

    use super::*;

    fn fixture_path(name: &str) -> Utf8PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir).join("tests/fixtures").join(name)
    }

    #[rstest]
    #[case::empty("empty", vec![])]
    #[case::single_project("single_project", vec!["."])]
    #[case::nested_projects("nested_projects", vec!["live/prod/app", "live/prod/vpc", "live/staging/vpc"])]
    #[case::with_ignored_dirs("with_ignored_dirs", vec!["vpc"])]
    #[case::terragrunt_infrastructure_live_example("terragrunt_infrastructure_live_example", vec!["non-prod/us-east-1/qa/mysql", "non-prod/us-east-1/qa/webserver-cluster", "non-prod/us-east-1/stage/mysql", "non-prod/us-east-1/stage/webserver-cluster", "prod/us-east-1/prod/mysql", "prod/us-east-1/prod/webserver-cluster"])]
    #[case::no_projects("no_projects", vec![])]
    fn test_discover_projects(#[case] fixture: &str, #[case] expected: Vec<&str>) {
        let root = fixture_path(fixture);
        let mut result = discover_projects(&root).expect("should not error");
        result.sort();

        let mut expected: Vec<Utf8PathBuf> = expected
            .iter()
            .map(|p| {
                if *p == "." {
                    root.clone()
                } else {
                    root.join(p)
                }
            })
            .collect();
        expected.sort();

        assert_eq!(result, expected);
    }

    #[test]
    fn test_nonexistent_path_returns_error() {
        let result = discover_projects(Utf8Path::new("/does/not/exist"));
        assert!(result.is_err());
    }
}
