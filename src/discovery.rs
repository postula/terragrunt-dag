//! Find terragrunt.hcl files in a directory tree.

use camino::{Utf8Path, Utf8PathBuf};
use std::io::Result;
use walkdir::{DirEntry, WalkDir};

/// Directories to skip during discovery
const IGNORED_DIRS: &[&str] = &[
    ".terragrunt-cache",
    ".terraform",
    ".git",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
];

fn is_ignored(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| IGNORED_DIRS.contains(&s) || s.starts_with('.'))
        .unwrap_or(false)
}

/// Discovers all terragrunt.hcl files under the given root directory.
pub fn discover_projects(root: &Utf8Path) -> Result<Vec<Utf8PathBuf>> {
    if !root.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("root path does not exist: {}", root),
        ));
    }
    Ok(WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_ignored(e))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() == "terragrunt.hcl")
        .filter_map(|e| Utf8PathBuf::from_path_buf(e.into_path()).ok())
        .map(|p| p.parent().unwrap().to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use camino::Utf8PathBuf;

    use super::*;

    fn fixture_path(name: &str) -> Utf8PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir)
            .join("tests/fixtures")
            .join(name)
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
