//! Resolve paths and evaluate terragrunt functions.

use camino::{Utf8Path, Utf8PathBuf};
use std::cell::{OnceCell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use crate::parser::PathExpr;
use crate::stack::EvalReport;

/// Shared dedup set for file-IO stub calls.
///
/// Keyed by `(function name, stack file path)` so a `file("x")` call repeated
/// across N units of the same stack file only increments `file_io_failures`
/// once. Shared via `Rc<RefCell<...>>` so every nested HCL evaluation sees
/// the same set.
pub type FileIoSeen = Rc<RefCell<HashSet<(String, Utf8PathBuf)>>>;

/// Caller-facing knobs that influence soft-failure handling.
#[derive(Debug, Clone, Copy)]
pub struct ResolveOptions {
    /// When `true`, the CLI escalates any recorded eval failure into a hard
    /// error after expansion completes.
    pub strict: bool,
    /// Hard limit on how deep `stack { source = "..." }` recursion is followed
    /// during expansion. Prevents pathological loops from `terragrunt.stack.hcl`
    /// trees that reference each other.
    pub max_recursion_depth: usize,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self {
            strict: false,
            max_recursion_depth: 32,
        }
    }
}

/// Resolution context - provides base paths for resolution
pub struct ResolveContext {
    /// The directory containing the current config file (for relative paths)
    pub config_dir: Utf8PathBuf,
    /// The original project directory (for find_in_parent_folders)
    /// This is the directory where the main terragrunt.hcl is located
    pub project_dir: Utf8PathBuf,
    /// Bound `values` from a parent `terragrunt.stack.hcl` for the current
    /// expanded unit. `None` when this context is not evaluating a stack unit.
    pub values: Option<hcl::Value>,
    /// Bound `local` object visible to lazy HCL evaluation of source paths.
    pub locals: Option<hcl::Value>,
    /// Directory of the parent stack file, exposed to HCL evaluation via
    /// `get_parent_terragrunt_dir()`.
    pub parent_dir: Option<Utf8PathBuf>,
    /// Resolution options inherited from the CLI.
    pub options: ResolveOptions,
    /// Shared mutable eval report used to record soft failures from inside
    /// HCL function stubs.
    pub eval_report: Option<Rc<RefCell<EvalReport>>>,
    /// Shared dedup set so repeated file-IO stub calls inside one stack
    /// expansion are counted once.
    pub file_io_seen: Option<FileIoSeen>,
    /// Identifier (typically the stack file path) used as part of the
    /// dedup key when present.
    pub stack_file: Option<Utf8PathBuf>,
    /// Cached repo root (lazily computed)
    repo_root: OnceCell<Option<Utf8PathBuf>>,
}

impl ResolveContext {
    /// Create a new resolution context for a project directory
    pub fn new(project_dir: Utf8PathBuf) -> Self {
        Self {
            config_dir: project_dir.clone(),
            project_dir,
            values: None,
            locals: None,
            parent_dir: None,
            options: ResolveOptions::default(),
            eval_report: None,
            file_io_seen: None,
            stack_file: None,
            repo_root: OnceCell::new(),
        }
    }

    /// Create a context for an included config file
    /// Uses config_dir for relative paths but project_dir for find_in_parent_folders
    pub fn for_included_config(config_dir: Utf8PathBuf, project_dir: Utf8PathBuf) -> Self {
        Self {
            config_dir,
            project_dir,
            values: None,
            locals: None,
            parent_dir: None,
            options: ResolveOptions::default(),
            eval_report: None,
            file_io_seen: None,
            stack_file: None,
            repo_root: OnceCell::new(),
        }
    }

    /// Attach bound `values` from a parent `terragrunt.stack.hcl` unit.
    pub fn with_values(mut self, values: hcl::Value) -> Self {
        self.values = Some(values);
        self
    }

    /// Attach bound `locals` for HCL evaluation of source paths.
    pub fn with_locals(mut self, locals: hcl::Value) -> Self {
        self.locals = Some(locals);
        self
    }

    /// Attach the parent stack directory, exposed via `get_parent_terragrunt_dir()`.
    pub fn with_parent_dir(mut self, parent_dir: Utf8PathBuf) -> Self {
        self.parent_dir = Some(parent_dir);
        self
    }

    /// Apply caller-facing resolution options.
    pub fn with_options(mut self, opts: ResolveOptions) -> Self {
        self.options = opts;
        self
    }

    /// Attach a shared eval report that HCL function stubs will mutate when
    /// they record a soft failure.
    pub fn with_eval_report(mut self, eval_report: Rc<RefCell<EvalReport>>) -> Self {
        self.eval_report = Some(eval_report);
        self
    }

    /// Attach a shared file-IO dedup set so repeated stub calls within one
    /// stack expansion are counted once.
    pub fn with_file_io_seen(mut self, seen: FileIoSeen) -> Self {
        self.file_io_seen = Some(seen);
        self
    }

    /// Attach the parent stack file path, used as part of the file-IO
    /// dedup key.
    pub fn with_stack_file(mut self, stack_file: Utf8PathBuf) -> Self {
        self.stack_file = Some(stack_file);
        self
    }

    /// Increment `source_path_failures` on the attached eval report, if any.
    pub(crate) fn record_source_path_failure(&self) {
        if let Some(report) = self.eval_report.as_ref() {
            report.borrow_mut().source_path_failures += 1;
        }
    }

    /// Resolve a PathExpr to an actual filesystem path.
    /// Returns None if the path cannot be resolved.
    /// For conditional expressions, returns the first resolved path.
    pub fn resolve(&self, path_expr: &PathExpr) -> Option<Utf8PathBuf> {
        self.resolve_all(path_expr).into_iter().next()
    }

    /// Resolve a PathExpr to all possible filesystem paths.
    /// For conditional expressions, returns both branches.
    /// Returns empty Vec if no paths can be resolved.
    pub fn resolve_all(&self, path_expr: &PathExpr) -> Vec<Utf8PathBuf> {
        match path_expr {
            PathExpr::Literal(path) => {
                // Join with config dir (current file's location) and normalize
                let resolved = self.config_dir.join(path);
                vec![normalize_path(&resolved)]
            }

            PathExpr::FindInParentFolders(filename) => {
                // Use PROJECT dir (original terragrunt.hcl location) for find_in_parent_folders
                // This matches terragrunt behavior where includes use the project's context
                let filename = filename.as_deref().unwrap_or("terragrunt.hcl");
                find_in_parent_folders(&self.project_dir, filename).into_iter().collect()
            }

            PathExpr::GetRepoRoot => self.repo_root().cloned().into_iter().collect(),

            PathExpr::GetTerragruntDir => vec![self.project_dir.clone()],

            PathExpr::GetParentTerragruntDir => {
                // Would need include context - not implemented in basic resolver
                vec![]
            }

            PathExpr::PathRelativeToInclude | PathExpr::PathRelativeFromInclude => {
                // Would need include context - not implemented in basic resolver
                vec![]
            }

            PathExpr::Dirname(inner) => {
                // Resolve inner expression first, then get parent directory for each
                self.resolve_all(inner)
                    .into_iter()
                    .filter_map(|resolved| resolved.parent().map(|p| p.to_path_buf()))
                    .collect()
            }

            PathExpr::Format {
                fmt,
                args,
            } => {
                // Resolve all arguments first
                let resolved_args: Option<Vec<String>> =
                    args.iter().map(|arg| self.resolve(arg).map(|p| p.to_string())).collect();

                match resolved_args {
                    Some(args) => {
                        // Replace %s placeholders with resolved arguments
                        let mut result = fmt.clone();
                        for arg in args {
                            // Replace first occurrence of %s with the argument
                            if let Some(pos) = result.find("%s") {
                                result.replace_range(pos..pos + 2, &arg);
                            }
                        }
                        // If result is a relative path, join with config_dir
                        // If it's already absolute (starts with /), normalize directly
                        let path = if result.starts_with('/') {
                            Utf8PathBuf::from(result)
                        } else {
                            self.config_dir.join(&result)
                        };
                        vec![normalize_path(&path)]
                    }
                    None => vec![],
                }
            }

            PathExpr::Interpolation(parts) => {
                // Resolve each part and concatenate them
                // IMPORTANT: Literals within interpolations are string fragments,
                // not full paths to be resolved. Only function calls get resolved.
                if parts.is_empty() {
                    return vec![];
                }

                let mut result = String::new();

                for part in parts {
                    match part {
                        // Literals in interpolation are string fragments - use as-is
                        PathExpr::Literal(s) => {
                            result.push_str(s);
                        }
                        // Raw HCL expressions evaluate to a string fragment
                        // (e.g. `${local.unseal_type}` → "gcp"). Splice the
                        // raw string in without re-resolving as a path.
                        PathExpr::HclExpr(expr) => {
                            let Some(fragment) = self.evaluate_hcl_to_string(expr) else {
                                return vec![];
                            };
                            result.push_str(&fragment);
                        }
                        // Other expressions need to be resolved
                        _ => match self.resolve(part) {
                            Some(resolved) => {
                                result.push_str(resolved.as_str());
                            }
                            None => {
                                return vec![];
                            }
                        },
                    }
                }

                // Parse the concatenated result as a path and normalize it
                let path = Utf8PathBuf::from(result);
                vec![normalize_path(&path)]
            }

            PathExpr::Conditional {
                true_branch,
                false_branch,
            } => {
                // Resolve both branches since we can't evaluate the condition
                let mut results = self.resolve_all(true_branch);
                results.extend(self.resolve_all(false_branch));
                results
            }

            PathExpr::ValuesRef(keys) => {
                let Some(bound) = self.values.as_ref() else {
                    return vec![];
                };
                let Some(literal) = lookup_values_string(bound, keys) else {
                    return vec![];
                };
                // Reuse Literal resolution semantics: join with config_dir + normalize.
                self.resolve_all(&PathExpr::Literal(literal))
            }

            PathExpr::HclExpr(expr) => self.resolve_hcl_expr(expr),

            PathExpr::Unresolvable {
                ..
            } => {
                // Can't resolve - return empty vec
                vec![]
            }
        }
    }

    /// Evaluate a raw HCL expression and, on success, feed the resulting
    /// string back through `Literal` resolution. Records a soft failure if
    /// evaluation fails so the CLI can escalate under `--strict`.
    fn resolve_hcl_expr(&self, expr: &hcl::Expression) -> Vec<Utf8PathBuf> {
        let values = self.values.clone().unwrap_or_else(|| hcl::Value::Object(hcl::Map::new()));
        let locals = self.locals.clone().unwrap_or_else(|| hcl::Value::Object(hcl::Map::new()));
        let ctx = crate::hcl_eval::build_context(&values, &locals, self);
        match crate::hcl_eval::evaluate_expression_with_ctx(expr, &ctx, self) {
            Ok(hcl::Value::String(s)) => self.resolve_all(&PathExpr::Literal(s)),
            Ok(_) => {
                self.record_source_path_failure();
                vec![]
            }
            Err(_) => {
                self.record_source_path_failure();
                vec![]
            }
        }
    }

    /// Evaluate a raw HCL expression and return the resulting string
    /// without applying path-resolution semantics. Used to splice
    /// `${expr}` fragments into an enclosing interpolation.
    fn evaluate_hcl_to_string(&self, expr: &hcl::Expression) -> Option<String> {
        let values = self.values.clone().unwrap_or_else(|| hcl::Value::Object(hcl::Map::new()));
        let locals = self.locals.clone().unwrap_or_else(|| hcl::Value::Object(hcl::Map::new()));
        let ctx = crate::hcl_eval::build_context(&values, &locals, self);
        match crate::hcl_eval::evaluate_expression_with_ctx(expr, &ctx, self) {
            Ok(hcl::Value::String(s)) => Some(s),
            _ => {
                self.record_source_path_failure();
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

/// Walk `bound` along `keys`, returning the terminal string value.
///
/// Returns `None` if any segment is missing, the traversal hits a non-object,
/// or the terminal value is not a string.
fn lookup_values_string(bound: &hcl::Value, keys: &[String]) -> Option<String> {
    let mut cursor = bound;
    for key in keys {
        let hcl::Value::Object(obj) = cursor else {
            return None;
        };
        cursor = obj.get(key.as_str())?;
    }
    match cursor {
        hcl::Value::String(s) => Some(s.clone()),
        _ => None,
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

    /// Ensure test fixtures are set up at runtime.
    ///
    /// Several fixtures cannot be committed to the repository: `.git/` directories
    /// would make git treat the fixture root as a submodule boundary, hiding any
    /// committed files beneath it. Tests that exercise `find_repo_root` and
    /// `find_in_parent_folders` therefore depend on placeholder files being
    /// materialized here at test-run time.
    fn setup_fixtures() {
        INIT.call_once(|| {
            let manifest_dir = env!("CARGO_MANIFEST_DIR");
            let resolver_fixtures = std::path::Path::new(manifest_dir).join("tests/fixtures/resolver");

            let dirs = ["repo/.git", "repo_with_template/.git"];
            for dir in dirs {
                let path = resolver_fixtures.join(dir);
                if !path.exists() {
                    std::fs::create_dir_all(&path).ok();
                }
            }

            let files = [
                "repo/root.hcl",
                "repo/live/terragrunt.hcl",
                "repo/live/prod/region.hcl",
                "repo_with_template/root.hcl",
            ];
            for file in files {
                let path = resolver_fixtures.join(file);
                if !path.exists() {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                    std::fs::write(&path, b"").ok();
                }
            }
        });
    }

    fn fixture_path(name: &str) -> Utf8PathBuf {
        setup_fixtures();
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir).join("tests/fixtures/resolver").join(name)
    }

    /// Normalize a path to use forward slashes for cross-platform test assertions.
    /// This allows tests to compare paths consistently on both Unix and Windows.
    /// Also strips Windows drive letters (e.g., "C:" or "D:") from absolute paths.
    #[cfg(test)]
    fn normalize_slashes(path: &Utf8Path) -> String {
        let s = path.as_str().replace('\\', "/");
        // Strip Windows drive letter prefix (e.g., "C:" or "D:")
        if s.len() >= 2 && s.chars().nth(1) == Some(':') {
            s[2..].to_string()
        } else {
            s
        }
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

        // Normalize both paths to forward slashes for cross-platform comparison
        assert!(result.is_some());
        let normalized = normalize_slashes(&result.unwrap());
        assert_eq!(normalized, "/path/to");
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
        // Normalize to forward slashes for cross-platform comparison
        let normalized = normalize_slashes(&resolved);
        assert!(normalized.contains("/config/"));
        assert!(normalized.ends_with(".yaml"));
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
        // Normalize to forward slashes for cross-platform comparison
        let normalized = normalize_slashes(&resolved);
        assert!(normalized.contains("/modules/"));
    }

    // ============== ValuesRef tests ==============

    fn make_values_object(entries: &[(&str, hcl::Value)]) -> hcl::Value {
        let mut map = hcl::Map::new();
        for (k, v) in entries {
            map.insert(k.to_string(), v.clone());
        }
        hcl::Value::Object(map)
    }

    #[test]
    fn test_resolve_values_ref_top_level_string() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir)
            .with_values(make_values_object(&[("dep", hcl::Value::String("../sg".to_string()))]));

        let result = ctx.resolve(&PathExpr::ValuesRef(vec!["dep".to_string()]));

        assert_eq!(result, Some(fixture_path("repo/live/prod/sg")));
    }

    #[test]
    fn test_resolve_values_ref_nested_keys() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let inner = make_values_object(&[("dep", hcl::Value::String("../sg".to_string()))]);
        let ctx = ResolveContext::new(project_dir).with_values(make_values_object(&[("svc", inner)]));

        let result = ctx.resolve(&PathExpr::ValuesRef(vec!["svc".to_string(), "dep".to_string()]));

        assert_eq!(result, Some(fixture_path("repo/live/prod/sg")));
    }

    #[test]
    fn test_resolve_values_ref_missing_key_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir)
            .with_values(make_values_object(&[("other", hcl::Value::String("x".to_string()))]));

        let result = ctx.resolve(&PathExpr::ValuesRef(vec!["missing".to_string()]));

        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_values_ref_non_string_terminal_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir)
            .with_values(make_values_object(&[("n", hcl::Value::Number(hcl::Number::from(42)))]));

        let result = ctx.resolve(&PathExpr::ValuesRef(vec!["n".to_string()]));

        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_values_ref_without_bound_values_returns_none() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        let result = ctx.resolve(&PathExpr::ValuesRef(vec!["dep".to_string()]));

        assert_eq!(result, None);
    }

    // ============== HclExpr resolution ==============

    fn parse_expr(src: &str) -> hcl::Expression {
        src.parse().unwrap_or_else(|e| panic!("failed to parse {:?}: {}", src, e))
    }

    #[test]
    fn test_resolve_hcl_expr_with_get_repo_root() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir);

        // ${get_repo_root()}/modules/vpc as a raw HCL expression.
        let expr = parse_expr(r#""${get_repo_root()}/modules/vpc""#);
        let path_expr = PathExpr::HclExpr(expr);

        let result = ctx.resolve(&path_expr);
        assert_eq!(result, Some(fixture_path("repo/modules/vpc")));
    }

    #[test]
    fn test_resolve_hcl_expr_with_values_ref_via_eval() {
        // `values.dep` evaluated through the HCL evaluator (not via the
        // dedicated ValuesRef variant) still resolves correctly.
        let project_dir = fixture_path("repo/live/prod/vpc");
        let ctx = ResolveContext::new(project_dir)
            .with_values(make_values_object(&[("dep", hcl::Value::String("../sg".to_string()))]));

        let expr = parse_expr("values.dep");
        let path_expr = PathExpr::HclExpr(expr);

        let result = ctx.resolve(&path_expr);
        assert_eq!(result, Some(fixture_path("repo/live/prod/sg")));
    }

    #[test]
    fn test_resolve_hcl_expr_strict_propagates_eval_error() {
        let project_dir = fixture_path("repo/live/prod/vpc");
        let report = std::rc::Rc::new(std::cell::RefCell::new(crate::stack::EvalReport::default()));
        let ctx = ResolveContext::new(project_dir)
            .with_options(ResolveOptions {
                strict: true,
                ..Default::default()
            })
            .with_eval_report(std::rc::Rc::clone(&report));

        // `values.missing` will raise an HCL eval error since `values` has no
        // such key.
        let expr = parse_expr("values.missing");
        let path_expr = PathExpr::HclExpr(expr);

        let result = ctx.resolve(&path_expr);
        assert_eq!(result, None);
        assert_eq!(report.borrow().source_path_failures, 1);
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
