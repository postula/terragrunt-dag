//! Resolve paths and evaluate terragrunt functions.

use camino::{Utf8Path, Utf8PathBuf};
use std::cell::OnceCell;

use crate::parser::PathExpr;

/// Resolution context - provides base paths for resolution
pub struct ResolveContext {
    /// The directory containing the current config file (for relative paths)
    pub config_dir: Utf8PathBuf,
    /// The original project directory (for find_in_parent_folders)
    /// This is the directory where the main terragrunt.hcl is located
    pub project_dir: Utf8PathBuf,
    /// Cached repo root (lazily computed)
    repo_root: OnceCell<Option<Utf8PathBuf>>,
}

impl ResolveContext {
    /// Create a new resolution context for a project directory
    pub fn new(project_dir: Utf8PathBuf) -> Self {
        Self {
            config_dir: project_dir.clone(),
            project_dir,
            repo_root: OnceCell::new(),
        }
    }

    /// Create a context for an included config file
    /// Uses config_dir for relative paths but project_dir for find_in_parent_folders
    pub fn for_included_config(config_dir: Utf8PathBuf, project_dir: Utf8PathBuf) -> Self {
        Self {
            config_dir,
            project_dir,
            repo_root: OnceCell::new(),
        }
    }

    /// Resolve a PathExpr to an actual filesystem path.
    /// Returns None if the path cannot be resolved.
    pub fn resolve(&self, path_expr: &PathExpr) -> Option<Utf8PathBuf> {
        match path_expr {
            PathExpr::Literal(path) => {
                // Join with config dir (current file's location) and normalize
                let resolved = self.config_dir.join(path);
                Some(normalize_path(&resolved))
            }

            PathExpr::FindInParentFolders(filename) => {
                // Use PROJECT dir (original terragrunt.hcl location) for find_in_parent_folders
                // This matches terragrunt behavior where includes use the project's context
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

            PathExpr::Dirname(inner) => {
                // Resolve the inner expression first, then get its parent directory
                let resolved = self.resolve(inner)?;
                resolved.parent().map(|p| p.to_path_buf())
            }

            PathExpr::Format {
                fmt,
                args,
            } => {
                // Resolve all arguments first
                let resolved_args: Vec<String> =
                    args.iter().map(|arg| self.resolve(arg).map(|p| p.to_string())).collect::<Option<Vec<_>>>()?;

                // Replace %s placeholders with resolved arguments
                let mut result = fmt.clone();
                for arg in resolved_args {
                    // Replace first occurrence of %s with the argument
                    if let Some(pos) = result.find("%s") {
                        result.replace_range(pos..pos + 2, &arg);
                    }
                }

                // Parse result as a path and normalize
                let path = Utf8PathBuf::from(result);
                Some(normalize_path(&path))
            }

            PathExpr::Interpolation(parts) => {
                // Resolve each part and concatenate them
                // IMPORTANT: Literals within interpolations are string fragments,
                // not full paths to be resolved. Only function calls get resolved.
                if parts.is_empty() {
                    return None;
                }

                let mut result = String::new();

                for part in parts {
                    match part {
                        // Literals in interpolation are string fragments - use as-is
                        PathExpr::Literal(s) => {
                            result.push_str(s);
                        }
                        // Other expressions need to be resolved
                        _ => match self.resolve(part) {
                            Some(resolved) => {
                                result.push_str(resolved.as_str());
                            }
                            None => {
                                return None;
                            }
                        },
                    }
                }

                // Parse the concatenated result as a path and normalize it
                let path = Utf8PathBuf::from(result);
                Some(normalize_path(&path))
            }

            PathExpr::Unresolvable {
                ..
            } => {
                // Can't resolve - return None
                None
            }
        }
    }

    /// Get the repository root (directory containing .git).
    /// Result is cached after first computation.
    pub fn repo_root(&self) -> Option<&Utf8PathBuf> {
        self.repo_root.get_or_init(|| find_repo_root(&self.project_dir)).as_ref()
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
                if !components.is_empty() && !matches!(components.last(), Some(Utf8Component::ParentDir)) {
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
    use std::sync::Once;

    static INIT: Once = Once::new();

    /// Ensure test fixtures are set up (creates .git directories that git won't track)
    fn setup_fixtures() {
        INIT.call_once(|| {
            let manifest_dir = env!("CARGO_MANIFEST_DIR");
            let resolver_fixtures = std::path::Path::new(manifest_dir).join("tests/fixtures/resolver");

            // Create .git directory for repo fixture
            let repo_git = resolver_fixtures.join("repo/.git");
            if !repo_git.exists() {
                std::fs::create_dir_all(&repo_git).ok();
            }
        });
    }

    fn fixture_path(name: &str) -> Utf8PathBuf {
        setup_fixtures();
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir).join("tests/fixtures/resolver").join(name)
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

        let result = ctx.resolve(&PathExpr::FindInParentFolders(Some("region.hcl".to_string())));

        // Should find repo/live/prod/region.hcl
        assert_eq!(result, Some(fixture_path("repo/live/prod/region.hcl")));
    }

    #[test]
    fn test_resolve_find_in_parent_folders_not_found() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::FindInParentFolders(Some("nonexistent.hcl".to_string())));

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

    // ============== dirname() function ==============

    #[test]
    fn test_resolve_dirname_of_literal() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let path_expr = PathExpr::Dirname(Box::new(PathExpr::Literal("/path/to/file.hcl".to_string())));

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, Some(Utf8PathBuf::from("/path/to")));
    }

    #[test]
    fn test_resolve_dirname_of_find_in_parent_folders() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // dirname(find_in_parent_folders("root.hcl"))
        let path_expr = PathExpr::Dirname(Box::new(PathExpr::FindInParentFolders(Some("root.hcl".to_string()))));

        let result = ctx.resolve(&path_expr);

        // find_in_parent_folders("root.hcl") resolves to repo/root.hcl
        // dirname of that is repo/
        assert_eq!(result, Some(fixture_path("repo")));
    }

    #[test]
    fn test_resolve_dirname_unresolvable_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // dirname of something that can't be resolved should return None
        let path_expr = PathExpr::Dirname(Box::new(PathExpr::Unresolvable {
            func: "get_env()".to_string(),
        }));

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, None);
    }

    // ============== Interpolation tests ==============

    #[test]
    fn test_resolve_interpolation_simple() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir.clone());

        // "${get_repo_root()}/modules/vpc"
        let path_expr =
            PathExpr::Interpolation(vec![PathExpr::GetRepoRoot, PathExpr::Literal("/modules/vpc".to_string())]);

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, Some(fixture_path("repo/modules/vpc")));
    }

    #[test]
    fn test_resolve_interpolation_with_dirname() {
        let project_dir = fixture_path("repo_with_template/live/prod/app");
        let ctx = ResolveContext::new(project_dir);

        // "${dirname(find_in_parent_folders("root.hcl"))}/_envcommon/service.hcl"
        let path_expr = PathExpr::Interpolation(vec![
            PathExpr::Dirname(Box::new(PathExpr::FindInParentFolders(Some("root.hcl".to_string())))),
            PathExpr::Literal("/_envcommon/service.hcl".to_string()),
        ]);

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, Some(fixture_path("repo_with_template/_envcommon/service.hcl")));
    }

    #[test]
    fn test_resolve_interpolation_complex() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir.clone());

        // "${get_repo_root()}/config/${get_terragrunt_dir()}.yaml"
        // Should resolve to: repo/config/[project_dir].yaml
        let path_expr = PathExpr::Interpolation(vec![
            PathExpr::GetRepoRoot,
            PathExpr::Literal("/config/".to_string()),
            PathExpr::GetTerragruntDir,
            PathExpr::Literal(".yaml".to_string()),
        ]);

        let result = ctx.resolve(&path_expr);

        // The result should contain the full interpolated path
        assert!(result.is_some());
        let resolved = result.unwrap();
        assert!(resolved.as_str().contains("/config/"));
        assert!(resolved.as_str().ends_with(".yaml"));
    }

    #[test]
    fn test_resolve_interpolation_with_unresolvable_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // If any part can't be resolved, the whole interpolation fails
        let path_expr = PathExpr::Interpolation(vec![
            PathExpr::GetRepoRoot,
            PathExpr::Unresolvable {
                func: "get_env()".to_string(),
            },
            PathExpr::Literal("/config.yaml".to_string()),
        ]);

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_interpolation_empty_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // Empty interpolation should return None
        let path_expr = PathExpr::Interpolation(vec![]);

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, None);
    }

    // ============== format() function tests ==============

    #[test]
    fn test_resolve_format_simple() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // format("%s/alarm_topic", get_repo_root())
        let path_expr = PathExpr::Format {
            fmt: "%s/alarm_topic".to_string(),
            args: vec![PathExpr::GetRepoRoot],
        };

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, Some(fixture_path("repo/alarm_topic")));
    }

    #[test]
    fn test_resolve_format_with_dirname() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // format("%s/alarm_topic", dirname(find_in_parent_folders("root.hcl")))
        let path_expr = PathExpr::Format {
            fmt: "%s/alarm_topic".to_string(),
            args: vec![PathExpr::Dirname(Box::new(PathExpr::FindInParentFolders(Some("root.hcl".to_string()))))],
        };

        let result = ctx.resolve(&path_expr);

        // dirname(find_in_parent_folders("root.hcl")) = dirname(repo/root.hcl) = repo
        assert_eq!(result, Some(fixture_path("repo/alarm_topic")));
    }

    #[test]
    fn test_resolve_format_multiple_args() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir.clone());

        // format("%s/modules/%s", get_repo_root(), get_terragrunt_dir())
        let path_expr = PathExpr::Format {
            fmt: "%s/modules/%s".to_string(),
            args: vec![PathExpr::GetRepoRoot, PathExpr::GetTerragruntDir],
        };

        let result = ctx.resolve(&path_expr);

        assert!(result.is_some());
        let resolved = result.unwrap();
        assert!(resolved.as_str().contains("/modules/"));
    }

    #[test]
    fn test_resolve_format_with_unresolvable_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // If any argument can't be resolved, the whole format fails
        let path_expr = PathExpr::Format {
            fmt: "%s/alarm_topic".to_string(),
            args: vec![PathExpr::Unresolvable {
                func: "get_env()".to_string(),
            }],
        };

        let result = ctx.resolve(&path_expr);

        assert_eq!(result, None);
    }
}
