//! Resolve paths and evaluate terragrunt functions.

use camino::{Utf8Path, Utf8PathBuf};
use std::cell::OnceCell;

use crate::parser::PathExpr;

/// Resolution context - provides base paths for resolution
pub struct ResolveContext {
    /// The directory containing the current terragrunt.hcl
    pub project_dir: Utf8PathBuf,
    /// Cached repo root (lazily computed)
    repo_root: OnceCell<Option<Utf8PathBuf>>,
}

impl ResolveContext {
    /// Create a new resolution context for a project directory
    pub fn new(project_dir: Utf8PathBuf) -> Self {
        Self {
            project_dir,
            repo_root: OnceCell::new(),
        }
    }

    /// Resolve a PathExpr to an actual filesystem path.
    /// Returns None if the path cannot be resolved.
    pub fn resolve(&self, path_expr: &PathExpr) -> Option<Utf8PathBuf> {
        match path_expr {
            PathExpr::Literal(path) => {
                // Join with project dir and normalize
                let resolved = self.project_dir.join(path);
                Some(normalize_path(&resolved))
            }

            PathExpr::FindInParentFolders(filename) => {
                let filename = filename.as_deref().unwrap_or("terragrunt.hcl");
                find_in_parent_folders(&self.project_dir, filename)
            }

            PathExpr::GetRepoRoot => self.repo_root().cloned(),

            PathExpr::GetTerragruntDir => Some(self.project_dir.clone()),

            PathExpr::GetParentTerragruntDir => {
                // Would need include context - not implemented in basic resolver
                None
            }

            PathExpr::PathRelativeToInclude | PathExpr::PathRelativeFromInclude => {
                // Would need include context - not implemented in basic resolver
                None
            }

            PathExpr::Interpolation(_parts) => {
                // String interpolation is complex - skip for now
                None
            }

            PathExpr::Unresolvable { .. } => {
                // Can't resolve - return None
                None
            }
        }
    }

    /// Get the repository root (directory containing .git).
    /// Result is cached after first computation.
    pub fn repo_root(&self) -> Option<&Utf8PathBuf> {
        self.repo_root
            .get_or_init(|| find_repo_root(&self.project_dir))
            .as_ref()
    }
}

/// Normalize a path by resolving . and .. components.
/// Does NOT require the path to exist (pure string manipulation).
fn normalize_path(path: &Utf8Path) -> Utf8PathBuf {
    use camino::Utf8Component;

    let mut components = Vec::new();

    for component in path.components() {
        match component {
            Utf8Component::ParentDir => {
                // Go up one level if possible
                if !components.is_empty()
                    && !matches!(components.last(), Some(Utf8Component::ParentDir))
                {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            Utf8Component::CurDir => {
                // Skip current dir references
            }
            _ => {
                components.push(component);
            }
        }
    }

    components.iter().collect()
}

/// Find a file by walking up the directory tree.
/// Starts from the PARENT of `from` (not from itself).
/// Returns the path to the file if found, None otherwise.
pub fn find_in_parent_folders(from: &Utf8Path, filename: &str) -> Option<Utf8PathBuf> {
    // Start from parent directory (terragrunt convention)
    let mut current = from.parent()?.to_path_buf();

    loop {
        let candidate = current.join(filename);
        if candidate.exists() {
            return Some(candidate);
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return None,
        }
    }
}

/// Find the repository root by looking for .git directory.
/// Walks up from the given path until .git is found.
pub fn find_repo_root(from: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut current = from.to_path_buf();

    // Ensure we start from a directory
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }

    loop {
        // Check for boundary marker (used in tests)
        if current.join(".gitboundary").exists() {
            return None;
        }

        if current.join(".git").exists() {
            return Some(current);
        }

        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(name: &str) -> Utf8PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir)
            .join("tests/fixtures/resolver")
            .join(name)
    }

    // ============== Literal path resolution ==============

    #[test]
    fn test_resolve_literal_relative_path() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir.clone());

        let result = ctx.resolve(&PathExpr::Literal("../sg".to_string()));

        assert_eq!(result, Some(fixture_path("repo/live/prod/sg")));
    }

    #[test]
    fn test_resolve_literal_parent_path() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::Literal("../../staging/app".to_string()));

        assert_eq!(result, Some(fixture_path("repo/live/staging/app")));
    }

    // ============== find_in_parent_folders ==============

    #[test]
    fn test_resolve_find_in_parent_folders_default() {
        // Default: looks for terragrunt.hcl
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::FindInParentFolders(None));

        // Should find live/terragrunt.hcl (first one going up)
        assert_eq!(result, Some(fixture_path("repo/live/terragrunt.hcl")));
    }

    #[test]
    fn test_resolve_find_in_parent_folders_with_filename() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::FindInParentFolders(Some("root.hcl".to_string())));

        // Should find repo/root.hcl
        assert_eq!(result, Some(fixture_path("repo/root.hcl")));
    }

    #[test]
    fn test_resolve_find_in_parent_folders_region_file() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::FindInParentFolders(Some(
            "region.hcl".to_string(),
        )));

        // Should find repo/live/prod/region.hcl
        assert_eq!(result, Some(fixture_path("repo/live/prod/region.hcl")));
    }

    #[test]
    fn test_resolve_find_in_parent_folders_not_found() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::FindInParentFolders(Some(
            "nonexistent.hcl".to_string(),
        )));

        assert_eq!(result, None);
    }

    // ============== get_repo_root ==============

    #[test]
    fn test_resolve_get_repo_root() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::GetRepoRoot);

        assert_eq!(result, Some(fixture_path("repo")));
    }

    #[test]
    fn test_resolve_get_repo_root_not_in_repo() {
        let project_dir = fixture_path("no_git_repo/project");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::GetRepoRoot);

        assert_eq!(result, None);
    }

    #[test]
    fn test_repo_root_is_cached() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // First call computes
        let first = ctx.repo_root();
        // Second call should return same cached value
        let second = ctx.repo_root();

        assert_eq!(first, second);
        assert!(first.is_some());
    }

    // ============== get_terragrunt_dir ==============

    #[test]
    fn test_resolve_get_terragrunt_dir() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir.clone());

        let result = ctx.resolve(&PathExpr::GetTerragruntDir);

        assert_eq!(result, Some(project_dir));
    }

    // ============== Unresolvable ==============

    #[test]
    fn test_resolve_unresolvable_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::Unresolvable {
            func: "get_env()".to_string(),
        });

        assert_eq!(result, None);
    }

    // ============== Standalone functions ==============

    #[test]
    fn test_find_in_parent_folders_standalone() {
        let from = fixture_path("repo/live/prod/vpc");

        let result = find_in_parent_folders(&from, "root.hcl");

        assert_eq!(result, Some(fixture_path("repo/root.hcl")));
    }

    #[test]
    fn test_find_repo_root_standalone() {
        let from = fixture_path("repo/live/prod/vpc");

        let result = find_repo_root(&from);

        assert_eq!(result, Some(fixture_path("repo")));
    }
}
