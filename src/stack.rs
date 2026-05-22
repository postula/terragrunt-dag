//! Parse `terragrunt.stack.hcl` files and expand them into synthetic units.
//!
//! A terragrunt stack file declares multiple `unit "name" { ... }` blocks that
//! reference a shared module via `source`. Terragrunt would generate concrete
//! units under `<stack_dir>/.terragrunt-stack/<unit.path>/terragrunt.hcl`.
//!
//! Two problems we solve here:
//! 1. The shared module's `terragrunt.hcl` (the `source`) is NOT a planable
//!    unit. It must be excluded from discovery output.
//! 2. The stack's units must still appear in the output even when
//!    `.terragrunt-stack/` hasn't been generated on disk yet.

use std::collections::HashSet;

use camino::{Utf8Path, Utf8PathBuf};

use crate::parser::{ParseError, PathExpr, extract_path_expr};
use crate::processor::{ParseCache, process_project_with_deps};
use crate::project::Project;
use crate::resolver::ResolveContext;

/// A `unit` block declared inside a `terragrunt.stack.hcl` file.
#[derive(Debug, Clone)]
pub struct StackUnit {
    /// Relative path under `.terragrunt-stack/` where this unit lives
    pub path: String,
    /// The `source` path expression (typically the shared module dir)
    pub source: PathExpr,
}

/// A parsed `terragrunt.stack.hcl` file.
#[derive(Debug, Clone)]
pub struct ParsedStack {
    pub stack_file: Utf8PathBuf,
    pub units: Vec<StackUnit>,
}

/// Result of expanding all stack files: filtered unit dirs and synthetic projects.
pub struct ExpandedDiscovery {
    /// Real unit directories with shared-module sources removed
    pub unit_dirs: Vec<Utf8PathBuf>,
    /// Synthetic projects representing stack-generated units
    pub synthetic_projects: Vec<Project>,
}

/// Parse a `terragrunt.stack.hcl` file and extract its unit declarations.
pub fn parse_stack_file(path: &Utf8Path) -> Result<ParsedStack, ParseError> {
    let content = std::fs::read_to_string(path)?;
    let body: hcl::Body = hcl::from_str(&content).map_err(|e| ParseError::HclError(e.to_string()))?;

    let mut units = Vec::new();
    for block in body.blocks() {
        if block.identifier() != "unit" {
            continue;
        }

        let source =
            block.body().attributes().find(|attr| attr.key() == "source").map(|attr| extract_path_expr(attr.expr()));

        let path_str = block.body().attributes().find(|attr| attr.key() == "path").and_then(|attr| match attr.expr() {
            hcl::Expression::String(s) => Some(s.clone()),
            _ => None,
        });

        if let (Some(source), Some(path)) = (source, path_str) {
            units.push(StackUnit {
                path,
                source,
            });
        }
    }

    Ok(ParsedStack {
        stack_file: path.to_path_buf(),
        units,
    })
}

/// Expand stack files into synthetic projects and filter out shared-module units.
///
/// - Discovered unit dirs that match a stack's `source` are dropped (they are
///   shared modules, not planable units).
/// - For each `unit` block, a synthetic `Project` is produced at
///   `<stack_dir>/.terragrunt-stack/<unit.path>`.
pub fn expand(unit_dirs: Vec<Utf8PathBuf>, stack_files: &[Utf8PathBuf], cache: &ParseCache) -> ExpandedDiscovery {
    let mut excluded_sources: HashSet<Utf8PathBuf> = HashSet::new();
    let mut excluded_unit_dirs: HashSet<Utf8PathBuf> = HashSet::new();
    let mut synthetic_projects = Vec::new();

    for stack_file in stack_files {
        let parsed = match parse_stack_file(stack_file) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Warning: failed to parse stack file {}: {}", stack_file, e);
                continue;
            }
        };

        let stack_dir = match stack_file.parent() {
            Some(p) => p.to_path_buf(),
            None => {
                eprintln!("Warning: stack file has no parent directory: {}", stack_file);
                continue;
            }
        };

        let resolve_ctx = ResolveContext::new(stack_dir.clone());

        for unit in parsed.units {
            let Some(resolved_source) = resolve_ctx.resolve(&unit.source) else {
                eprintln!("Warning: could not resolve source for unit '{}' in {}; skipping", unit.path, stack_file);
                continue;
            };

            // Canonicalize when possible so symlinks compare correctly.
            let canonical_source = resolved_source.canonicalize_utf8().unwrap_or(resolved_source.clone());
            excluded_sources.insert(canonical_source.clone());

            let unit_dir = stack_dir.join(".terragrunt-stack").join(&unit.path);
            let canonical_unit_dir = unit_dir.canonicalize_utf8().unwrap_or_else(|_| unit_dir.clone());
            excluded_unit_dirs.insert(canonical_unit_dir);

            let project = build_synthetic_project(&unit_dir, &canonical_source, stack_file, cache);
            synthetic_projects.push(project);
        }
    }

    // Filter real unit dirs: drop shared-module sources and any unit dirs we
    // already synthesised from a stack file.
    let filtered = unit_dirs
        .into_iter()
        .filter(|dir| {
            let canonical = dir.canonicalize_utf8().unwrap_or_else(|_| dir.clone());
            !excluded_sources.contains(&canonical) && !excluded_unit_dirs.contains(&canonical)
        })
        .collect();

    ExpandedDiscovery {
        unit_dirs: filtered,
        synthetic_projects,
    }
}

/// Build a `Project` for a synthetic unit at `unit_dir`.
///
/// If the generated `terragrunt.hcl` exists on disk, it is parsed via the
/// normal pipeline to capture real dependencies. Otherwise a minimal project
/// is synthesised from the source module and stack file alone.
fn build_synthetic_project(
    unit_dir: &Utf8Path,
    source_module: &Utf8Path,
    stack_file: &Utf8Path,
    cache: &ParseCache,
) -> Project {
    let unit_hcl = unit_dir.join("terragrunt.hcl");

    // Watch files always include the parent stack file and the shared module sources.
    let stack_watch = stack_file.to_path_buf();
    let module_hcl_glob = source_module.join("**/*.hcl");
    let module_tf_glob = source_module.join("**/*.tf*");

    if unit_hcl.exists() {
        // Reuse the normal pipeline so real dependencies/includes are captured.
        // Cascade is enabled to mirror default CLI behaviour.
        match process_project_with_deps(unit_dir, cache, true) {
            Ok(mut project) => {
                project.has_terraform_source = true;
                for extra in [stack_watch, module_hcl_glob, module_tf_glob] {
                    if !project.watch_files.contains(&extra) {
                        project.watch_files.push(extra);
                    }
                }
                project.watch_files.sort();
                project.watch_files.dedup();
                return project;
            }
            Err(e) => {
                eprintln!("Warning: failed to process generated unit {}: {}; using synthetic fallback", unit_dir, e);
            }
        }
    }

    Project {
        name: derive_project_name(unit_dir),
        dir: unit_dir.to_path_buf(),
        project_dependencies: Vec::new(),
        watch_files: vec![stack_watch, module_hcl_glob, module_tf_glob],
        has_terraform_source: true,
    }
}

/// Mirror of `processor::derive_project_name`.
///
/// Kept local to avoid widening that function's visibility.
fn derive_project_name(path: &Utf8Path) -> String {
    let components: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            camino::Utf8Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    let skip_prefixes = ["live", "terragrunt", "infrastructure", "infra", "repo", "projects"];
    let meaningful: Vec<&str> = components.iter().filter(|c| !skip_prefixes.contains(c)).copied().collect();

    if meaningful.is_empty() {
        "unknown".to_string()
    } else {
        meaningful.join("_")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(name: &str) -> Utf8PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        Utf8PathBuf::from(manifest_dir).join("tests/fixtures/stack").join(name)
    }

    #[test]
    fn test_parse_stack_file_basic() {
        let path = fixture_path("with_shared_module/live/staging/auth/terragrunt.stack.hcl");
        let parsed = parse_stack_file(&path).expect("should parse");
        assert_eq!(parsed.units.len(), 2);
        assert!(parsed.units.iter().any(|u| u.path == "azure/bdo"));
        assert!(parsed.units.iter().any(|u| u.path == "saml/crelan"));
    }
}
