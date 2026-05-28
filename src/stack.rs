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

use crate::hcl_eval::{build_context, evaluate_expression, evaluate_locals};
use crate::parser::{ParseError, PathExpr, extract_path_expr};
use crate::processor::{ParseCache, StackBinding, process_project_with_deps, process_project_with_deps_with_values};
use crate::project::Project;
use crate::resolver::ResolveContext;

/// A `unit` block declared inside a `terragrunt.stack.hcl` file.
#[derive(Debug, Clone)]
pub struct StackUnit {
    /// Relative path under `.terragrunt-stack/` where this unit lives
    pub path: String,
    /// The `source` path expression (typically the shared module dir)
    pub source: PathExpr,
    /// Raw `values = { ... }` expression, parsed but not yet evaluated.
    /// `None` when the unit block does not declare `values`.
    pub values_expr: Option<hcl::Expression>,
}

/// A parsed `terragrunt.stack.hcl` file.
#[derive(Debug, Clone)]
pub struct ParsedStack {
    pub stack_file: Utf8PathBuf,
    pub units: Vec<StackUnit>,
    /// Full HCL body (kept for downstream `locals { ... }` evaluation).
    pub body: hcl::Body,
}

/// Result of expanding all stack files: filtered unit dirs and synthetic projects.
pub struct ExpandedDiscovery {
    /// Real unit directories with shared-module sources removed
    pub unit_dirs: Vec<Utf8PathBuf>,
    /// Synthetic projects representing stack-generated units
    pub synthetic_projects: Vec<Project>,
    /// Number of stack units whose `values` could not be evaluated.
    /// CLI uses this to escalate failures under `--strict`.
    pub unresolved_values_count: usize,
}

/// Parse a `terragrunt.stack.hcl` file and extract its unit declarations.
pub fn parse_stack_file(path: &Utf8Path) -> Result<ParsedStack, ParseError> {
    let content = std::fs::read_to_string(path)?;
    let body: hcl::Body = hcl::from_str(&content).map_err(|e| ParseError::HclError(e.to_string()))?;

    let mut units = Vec::new();
    for block in body.blocks() {
        // Both `unit` and `stack` blocks declare a target path with `source`
        // and optional `values`. For the values-chain resolver they're
        // equivalent: only the destination kind differs.
        if !matches!(block.identifier(), "unit" | "stack") {
            continue;
        }

        let source =
            block.body().attributes().find(|attr| attr.key() == "source").map(|attr| extract_path_expr(attr.expr()));

        let path_str = block.body().attributes().find(|attr| attr.key() == "path").and_then(|attr| match attr.expr() {
            hcl::Expression::String(s) => Some(s.clone()),
            _ => None,
        });

        let values_expr = block.body().attributes().find(|attr| attr.key() == "values").map(|attr| attr.expr().clone());

        if let (Some(source), Some(path)) = (source, path_str) {
            units.push(StackUnit {
                path,
                source,
                values_expr,
            });
        }
    }

    Ok(ParsedStack {
        stack_file: path.to_path_buf(),
        units,
        body,
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
    let mut unresolved_values_count = 0usize;

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

        for unit in &parsed.units {
            let Some(resolved_source) = resolve_ctx.resolve(&unit.source) else {
                eprintln!("Warning: could not resolve source for unit '{}' in {}; skipping", unit.path, stack_file);
                continue;
            };

            // Canonicalize when possible so symlinks compare correctly.
            let canonical_source = resolved_source.canonicalize_utf8().unwrap_or(resolved_source.clone());
            excluded_sources.insert(canonical_source.clone());

            let unit_dir = stack_dir.join(".terragrunt-stack").join(&unit.path);
            let canonical_unit_dir = unit_dir.canonicalize_utf8().unwrap_or_else(|_| unit_dir.clone());
            excluded_unit_dirs.insert(canonical_unit_dir.clone());

            // Walk every parent `.terragrunt-stack/` layer so nested stacks
            // see the outer layer's values when computing their own.
            let bound_values = fold_frames(&collect_stack_frames(&unit_dir));

            if let Some(values) = bound_values.clone() {
                cache.stack_bindings.write().expect("stack bindings poisoned").insert(
                    canonical_unit_dir,
                    StackBinding {
                        values,
                        stack_file: stack_file.clone(),
                        parent_unit_path: unit.path.clone(),
                    },
                );
            } else if unit.values_expr.is_some() {
                unresolved_values_count += 1;
            }

            let project = build_synthetic_project(&unit_dir, &canonical_source, stack_file, cache, bound_values);
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
        unresolved_values_count,
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
    bound_values: Option<hcl::Value>,
) -> Project {
    let unit_hcl = unit_dir.join("terragrunt.hcl");

    // Watch files always include the parent stack file and the shared module sources.
    let stack_watch = stack_file.to_path_buf();
    let module_hcl_glob = source_module.join("**/*.hcl");
    let module_tf_glob = source_module.join("**/*.tf*");

    let process_result = match bound_values.clone() {
        Some(values) if unit_hcl.exists() => process_project_with_deps_with_values(unit_dir, cache, true, values),
        _ if unit_hcl.exists() => process_project_with_deps(unit_dir, cache, true),
        _ => {
            // The shared module's terragrunt.hcl carries the templated config.
            // Resolve dependencies from it but report the synthetic unit's dir.
            let module_hcl = source_module.join("terragrunt.hcl");
            if module_hcl.exists() {
                let result = match bound_values.clone() {
                    Some(values) => process_project_with_deps_with_values(source_module, cache, true, values),
                    None => process_project_with_deps(source_module, cache, true),
                };
                result.map(|mut p| {
                    p.dir = unit_dir.to_path_buf();
                    p.name = derive_project_name(unit_dir);
                    p
                })
            } else {
                return Project {
                    name: derive_project_name(unit_dir),
                    dir: unit_dir.to_path_buf(),
                    project_dependencies: Vec::new(),
                    watch_files: vec![stack_watch, module_hcl_glob, module_tf_glob],
                    has_terraform_source: true,
                };
            }
        }
    };

    match process_result {
        Ok(mut project) => {
            project.has_terraform_source = true;
            for extra in [stack_watch, module_hcl_glob, module_tf_glob] {
                if !project.watch_files.contains(&extra) {
                    project.watch_files.push(extra);
                }
            }
            project.watch_files.sort();
            project.watch_files.dedup();
            project
        }
        Err(e) => {
            eprintln!("Warning: failed to process generated unit {}: {}; using synthetic fallback", unit_dir, e);
            Project {
                name: derive_project_name(unit_dir),
                dir: unit_dir.to_path_buf(),
                project_dependencies: Vec::new(),
                watch_files: vec![stack_watch, module_hcl_glob, module_tf_glob],
                has_terraform_source: true,
            }
        }
    }
}

fn empty_object() -> hcl::Value {
    hcl::Value::Object(hcl::Map::new())
}

/// Evaluate a unit's `values = { ... }` expression against the supplied
/// outer values and locals. Logs and returns `None` on failure so the
/// caller falls back to unbound resolution.
fn evaluate_unit_values(
    unit: &StackUnit,
    outer_values: &hcl::Value,
    locals: &hcl::Value,
    stack_file: &Utf8Path,
) -> Option<hcl::Value> {
    let expr = unit.values_expr.as_ref()?;
    let ctx = build_context(outer_values, locals);
    match evaluate_expression(expr, &ctx) {
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!(
                "Warning: could not evaluate values for unit '{}' in {}: {}; dependency edges skipped",
                unit.path, stack_file, err
            );
            None
        }
    }
}

/// One frame in the stack chain leading from an outer stack to an inner unit.
struct StackFrame {
    stack_file: Utf8PathBuf,
    parsed: ParsedStack,
    unit_index: usize,
}

/// Walk outward from `unit_dir` collecting every parent `terragrunt.stack.hcl`
/// frame whose declared unit covers `unit_dir`.
///
/// Frames are returned outermost-first so callers can fold `values` from the
/// outermost stack down to `unit_dir`.
fn collect_stack_frames(unit_dir: &Utf8Path) -> Vec<StackFrame> {
    let mut frames = Vec::new();
    let mut cursor = unit_dir.to_path_buf();

    while let Some((stack_dir, rel)) = nearest_stack_ancestor(&cursor) {
        let stack_file = stack_dir.join("terragrunt.stack.hcl");
        let parsed = match parse_stack_file(&stack_file) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Warning: failed to parse stack file {}: {}", stack_file, e);
                break;
            }
        };

        // Several stacks can declare overlapping paths
        // (e.g. `on_premise/.../platform` and `on_premise/.../platform/apps/cd`).
        // Pick the longest matching path so we land on the deepest stack
        // covering this rel.
        let Some(unit_index) = parsed
            .units
            .iter()
            .enumerate()
            .filter(|(_, u)| rel == u.path || rel.starts_with(&format!("{}/", u.path)))
            .max_by_key(|(_, u)| u.path.len())
            .map(|(i, _)| i)
        else {
            // No matching unit at this layer — stop walking, stale `.terragrunt-stack` content.
            break;
        };

        frames.push(StackFrame {
            stack_file,
            parsed,
            unit_index,
        });

        cursor = stack_dir;
    }

    frames.reverse();
    frames
}

/// Locate the nearest `.terragrunt-stack/<rel>` segment in `path`.
///
/// Returns `(stack_dir, rel)` where `stack_dir` is the directory containing
/// the `.terragrunt-stack/` folder and `rel` is the remainder under it.
fn nearest_stack_ancestor(path: &Utf8Path) -> Option<(Utf8PathBuf, String)> {
    use camino::Utf8Component;

    let components: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Utf8Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    // Find the last occurrence of `.terragrunt-stack` to handle nesting.
    let marker_idx = components.iter().rposition(|c| *c == ".terragrunt-stack")?;
    if marker_idx + 1 >= components.len() {
        return None;
    }

    let rel = components[marker_idx + 1..].join("/");
    let stack_dir_components = &components[..marker_idx];

    // Rebuild stack_dir preserving the path's prefix (e.g. leading `/`).
    let mut stack_dir = Utf8PathBuf::new();
    let mut normal_seen = 0;
    for comp in path.components() {
        match comp {
            Utf8Component::Normal(_) if normal_seen >= stack_dir_components.len() => break,
            Utf8Component::Normal(_) => {
                normal_seen += 1;
                stack_dir.push(comp.as_str());
            }
            _ => stack_dir.push(comp.as_str()),
        }
    }

    Some((stack_dir, rel))
}

/// Fold every frame's `values` from outermost to innermost, producing the
/// bound values for the innermost unit. Returns `None` if any layer fails
/// to evaluate its unit values expression.
fn fold_frames(frames: &[StackFrame]) -> Option<hcl::Value> {
    let mut outer = empty_object();
    for frame in frames {
        let locals = evaluate_locals(&frame.parsed.body, &outer).unwrap_or_else(|_| empty_object());
        let unit = &frame.parsed.units[frame.unit_index];
        let evaluated = evaluate_unit_values(unit, &outer, &locals, &frame.stack_file)?;
        outer = evaluated;
    }
    Some(outer)
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
        assert!(parsed.units.iter().any(|u| u.path == "azure/alpha"));
        assert!(parsed.units.iter().any(|u| u.path == "saml/beta"));
    }

    #[test]
    fn test_parse_stack_file_captures_values_expr() {
        let path = fixture_path("with_shared_module/live/staging/auth/terragrunt.stack.hcl");
        let parsed = parse_stack_file(&path).expect("should parse");
        assert!(parsed.units.iter().all(|u| u.values_expr.is_some()), "every unit should carry its values expr");
    }

    #[test]
    fn test_expand_with_merge_format_locals_resolves_deps() {
        let stack_file =
            fixture_path("values_merge_format/live/staging/svc/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        assert_eq!(expanded.synthetic_projects.len(), 1);
        let project = &expanded.synthetic_projects[0];

        let base = fixture_path("values_merge_format/live/staging/svc/base").canonicalize_utf8().unwrap();
        let svc_eu = fixture_path("values_merge_format/live/staging/svc/svc-eu").canonicalize_utf8().unwrap();

        assert!(
            project.project_dependencies.iter().any(|d| d == base.as_str()),
            "expected dependency on base ({}), got {:?}",
            base,
            project.project_dependencies
        );
        assert!(
            project.project_dependencies.iter().any(|d| d == svc_eu.as_str()),
            "expected dependency on svc-eu ({}), got {:?}",
            svc_eu,
            project.project_dependencies
        );

        // Bound values should include the merged `env` key from locals as well.
        let unit_dir =
            fixture_path("values_merge_format/live/staging/svc/.terragrunt-stack/main").canonicalize_utf8().unwrap();
        let bindings = cache.stack_bindings.read().expect("stack bindings poisoned");
        let binding = bindings.get(&unit_dir).expect("stack binding should be cached");
        let hcl::Value::Object(values) = &binding.values else {
            panic!("expected object values, got {:?}", binding.values);
        };
        assert_eq!(values.get("env"), Some(&hcl::Value::String("prod".to_string())));
    }

    #[test]
    fn test_nearest_stack_ancestor_simple() {
        let path = Utf8PathBuf::from("/root/live/svc/.terragrunt-stack/main");
        let (dir, rel) = nearest_stack_ancestor(&path).unwrap();
        assert_eq!(dir, Utf8PathBuf::from("/root/live/svc"));
        assert_eq!(rel, "main");
    }

    #[test]
    fn test_nearest_stack_ancestor_nested_picks_innermost() {
        let path = Utf8PathBuf::from("/root/live/svc/.terragrunt-stack/azure/.terragrunt-stack/svc");
        let (dir, rel) = nearest_stack_ancestor(&path).unwrap();
        assert_eq!(dir, Utf8PathBuf::from("/root/live/svc/.terragrunt-stack/azure"));
        assert_eq!(rel, "svc");
    }

    #[test]
    fn test_expand_with_nested_stacks_resolves_chain() {
        let outer = fixture_path("values_nested_stacks/live/svc/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let inner = fixture_path("values_nested_stacks/live/svc/.terragrunt-stack/azure/terragrunt.stack.hcl")
            .canonicalize_utf8()
            .unwrap();

        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[outer, inner], &cache);

        let azure_base =
            fixture_path("values_nested_stacks/live/svc/.terragrunt-stack/azure/.terragrunt-stack/azure_base")
                .canonicalize_utf8()
                .unwrap();

        let svc_unit = expanded
            .synthetic_projects
            .iter()
            .find(|p| p.dir.as_str().ends_with(".terragrunt-stack/azure/.terragrunt-stack/svc"))
            .expect("inner svc unit must be in synthetic projects");

        assert!(
            svc_unit.project_dependencies.iter().any(|d| d == azure_base.as_str()),
            "expected inner svc unit to depend on azure_base ({}), got {:?}",
            azure_base,
            svc_unit.project_dependencies
        );
    }

    #[test]
    fn test_expand_with_three_level_chain_resolves_dep() {
        // Three nested stack files; the inner unit's `values.dep` is computed
        // from a value introduced two layers up (`values.cloud`/`values.env`
        // at the outermost stack, then `values.cluster` at the middle layer).
        let outer = fixture_path("values_three_levels/live/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let mid = fixture_path("values_three_levels/live/.terragrunt-stack/azure/east-us/terragrunt.stack.hcl")
            .canonicalize_utf8()
            .unwrap();
        let inner = fixture_path(
            "values_three_levels/live/.terragrunt-stack/azure/east-us/.terragrunt-stack/k8s/terragrunt.stack.hcl",
        )
        .canonicalize_utf8()
        .unwrap();

        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[outer, mid, inner], &cache);

        let target = fixture_path(
            "values_three_levels/live/.terragrunt-stack/azure/east-us/.terragrunt-stack/k8s/.terragrunt-stack/azure-prod_base",
        )
        .canonicalize_utf8()
        .unwrap();

        let svc = expanded
            .synthetic_projects
            .iter()
            .find(|p| p.dir.as_str().ends_with("/svc"))
            .expect("inner svc unit must be present");

        assert!(
            svc.project_dependencies.iter().any(|d| d == target.as_str()),
            "expected svc to depend on azure-prod_base ({}), got {:?}",
            target,
            svc.project_dependencies
        );
    }

    #[test]
    fn test_expand_with_unresolvable_values_warns_and_skips() {
        let stack_file = fixture_path("values_unresolvable/live/svc/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        assert_eq!(expanded.synthetic_projects.len(), 1);
        let project = &expanded.synthetic_projects[0];
        assert!(
            project.project_dependencies.is_empty(),
            "unresolvable values should yield no dependency edges, got {:?}",
            project.project_dependencies
        );
    }

    #[test]
    fn test_expand_with_lazy_try_resolves_dep() {
        // `try(values.optional_extras, ["fallback"])` must not abort
        // evaluation when `values.optional_extras` is undefined.
        let stack_file = fixture_path("values_lazy_try/live/svc/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        assert_eq!(expanded.synthetic_projects.len(), 1);
        let project = &expanded.synthetic_projects[0];

        let base = fixture_path("values_lazy_try/live/svc/.terragrunt-stack/base").canonicalize_utf8().unwrap();
        assert!(
            project.project_dependencies.iter().any(|d| d == base.as_str()),
            "expected dependency on base ({}), got {:?}",
            base,
            project.project_dependencies
        );
    }

    #[test]
    fn test_expand_with_bare_local_resolves_dep() {
        // `all_locals = local` should bind the full locals object so
        // downstream `values.all_locals.shared_dep` traversal works.
        let stack_file = fixture_path("values_bare_local/live/svc/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        assert_eq!(expanded.synthetic_projects.len(), 1);
        let project = &expanded.synthetic_projects[0];

        let base = fixture_path("values_bare_local/live/svc/.terragrunt-stack/base").canonicalize_utf8().unwrap();
        assert!(
            project.project_dependencies.iter().any(|d| d == base.as_str()),
            "expected dependency on base ({}), got {:?}",
            base,
            project.project_dependencies
        );
    }

    #[test]
    fn test_expand_with_for_expression_resolves_dep() {
        // `local.groups = { for name, _ in local.customers : ... => name }`
        // should evaluate so the unit's `values = { groups = local.groups }`
        // binds successfully and the `dep` edge resolves.
        let stack_file =
            fixture_path("values_for_expression/live/svc/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        assert_eq!(expanded.synthetic_projects.len(), 1);
        let project = &expanded.synthetic_projects[0];

        let base = fixture_path("values_for_expression/live/svc/.terragrunt-stack/base").canonicalize_utf8().unwrap();
        assert!(
            project.project_dependencies.iter().any(|d| d == base.as_str()),
            "expected dependency on base ({}), got {:?}",
            base,
            project.project_dependencies
        );

        // The synthesized for-object should land in the cached binding.
        let unit_dir =
            fixture_path("values_for_expression/live/svc/.terragrunt-stack/svc").canonicalize_utf8().unwrap();
        let bindings = cache.stack_bindings.read().expect("stack bindings poisoned");
        let binding = bindings.get(&unit_dir).expect("stack binding should be cached");
        let hcl::Value::Object(values) = &binding.values else {
            panic!("expected object values, got {:?}", binding.values);
        };
        let hcl::Value::Object(groups) = values.get("groups").expect("groups bound") else {
            panic!("expected groups to be an object");
        };
        assert_eq!(groups.get("team/alice"), Some(&hcl::Value::String("alice".into())));
        assert_eq!(groups.get("team/bob"), Some(&hcl::Value::String("bob".into())));
    }

    #[test]
    fn test_expand_with_literal_values_resolves_dep() {
        let stack_file =
            fixture_path("values_simple_dep/live/staging/svc/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        assert_eq!(expanded.synthetic_projects.len(), 1);
        let project = &expanded.synthetic_projects[0];

        // The dependency should resolve to the sibling directory via values.dep.
        let sibling = fixture_path("values_simple_dep/live/staging/svc/sibling").canonicalize_utf8().unwrap();
        assert!(
            project.project_dependencies.iter().any(|d| d == sibling.as_str()),
            "expected dependency on {}, got {:?}",
            sibling,
            project.project_dependencies
        );

        // Stack binding should be cached for the synthetic unit.
        let unit_dir =
            fixture_path("values_simple_dep/live/staging/svc/.terragrunt-stack/main").canonicalize_utf8().unwrap();
        assert!(
            cache.stack_bindings.read().expect("stack bindings poisoned").contains_key(&unit_dir),
            "stack binding should be cached for {}",
            unit_dir
        );
    }
}
