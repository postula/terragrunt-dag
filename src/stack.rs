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

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use camino::{Utf8Path, Utf8PathBuf};

use crate::hcl_eval::{build_context, evaluate_expression_with_ctx, evaluate_locals};
use crate::parser::{ParseError, PathExpr, extract_path_expr};
use crate::processor::{ParseCache, StackBinding, process_project_with_deps, process_project_with_deps_with_values};
use crate::project::Project;
use crate::resolver::{FileIoSeen, ResolveContext, ResolveOptions};

/// Build a `ResolveContext` configured to record soft eval failures into
/// `eval_report` and honour `opts.strict` inside HCL builtins.
fn resolve_ctx_for(
    config_dir: Utf8PathBuf,
    parent_dir: Option<Utf8PathBuf>,
    opts: ResolveOptions,
    eval_report: Rc<RefCell<EvalReport>>,
    file_io_seen: FileIoSeen,
    stack_file: Option<Utf8PathBuf>,
) -> ResolveContext {
    let mut ctx = ResolveContext::new(config_dir)
        .with_options(opts)
        .with_eval_report(eval_report)
        .with_file_io_seen(file_io_seen);
    if let Some(parent) = parent_dir {
        ctx = ctx.with_parent_dir(parent);
    }
    if let Some(sf) = stack_file {
        ctx = ctx.with_stack_file(sf);
    }
    ctx
}

/// Distinguishes a `unit { ... }` block from a `stack { ... }` block.
///
/// Both share the same shape (`source`, `path`, optional `values`) but only
/// `stack` blocks should be followed recursively into their source's
/// `terragrunt.stack.hcl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Unit,
    Stack,
}

/// A `unit` or `stack` block declared inside a `terragrunt.stack.hcl` file.
#[derive(Debug, Clone)]
pub struct StackUnit {
    /// Relative path under `.terragrunt-stack/` where this unit lives
    pub path: String,
    /// The `source` path expression (typically the shared module dir)
    pub source: PathExpr,
    /// Raw `values = { ... }` expression, parsed but not yet evaluated.
    /// `None` when the unit block does not declare `values`.
    pub values_expr: Option<hcl::Expression>,
    /// Which kind of block produced this entry — controls recursion behavior.
    pub kind: BlockKind,
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
    /// Aggregated soft-failure counters from HCL evaluation.
    /// CLI uses these to escalate under `--strict`.
    pub eval_report: EvalReport,
}

/// Soft-failure counters collected while expanding stack files.
#[derive(Debug, Default, Clone, Copy)]
pub struct EvalReport {
    /// Stack unit `values = { ... }` expressions that failed to evaluate.
    pub values_failures: usize,
    /// `source = ...` path expressions that could not be resolved.
    pub source_path_failures: usize,
    /// Stubbed file-IO style functions (`file()`, `read_terragrunt_config()`,
    /// `templatefile()`, etc.) that were called during evaluation.
    pub file_io_failures: usize,
    /// Recursion into a stack source short-circuited because the same stack
    /// file is already on the current expansion path.
    pub cycle_skipped: usize,
    /// Stack blocks whose `source` resolved to a remote URL — recursion is
    /// skipped and the parent shell entry stays in the output.
    pub remote_sources_skipped: usize,
    /// Stack blocks whose resolved source has no `terragrunt.stack.hcl` on
    /// disk — recursion is skipped and the shell entry stays.
    pub missing_child_stack: usize,
    /// Recursion attempts that would exceed `ResolveOptions::max_recursion_depth`.
    pub recursion_depth_exceeded: usize,
}

impl EvalReport {
    pub fn total(&self) -> usize {
        self.values_failures
            + self.source_path_failures
            + self.file_io_failures
            + self.cycle_skipped
            + self.remote_sources_skipped
            + self.missing_child_stack
            + self.recursion_depth_exceeded
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Parse a stack file via the shared cache when possible. Falls back to
/// uncached parsing if the path cannot be canonicalized.
fn parse_stack_file_cached(path: &Utf8Path, cache: &ParseCache) -> Result<ParsedStack, ParseError> {
    let key = path.canonicalize_utf8().unwrap_or_else(|_| path.to_path_buf());
    if let Some(parsed) = cache.stack_parses.read().expect("stack parses poisoned").get(&key) {
        return Ok(parsed.clone());
    }
    let parsed = parse_stack_file(path)?;
    cache.stack_parses.write().expect("stack parses poisoned").insert(key, parsed.clone());
    Ok(parsed)
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
        let kind = match block.identifier() {
            "unit" => BlockKind::Unit,
            "stack" => BlockKind::Stack,
            _ => continue,
        };

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
                kind,
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
    expand_with_options(unit_dirs, stack_files, cache, ResolveOptions::default())
}

/// Like [`expand`] but threads caller-facing `ResolveOptions` through to the
/// HCL evaluator so file-IO stubs honour `--strict`.
pub fn expand_with_options(
    unit_dirs: Vec<Utf8PathBuf>,
    stack_files: &[Utf8PathBuf],
    cache: &ParseCache,
    opts: ResolveOptions,
) -> ExpandedDiscovery {
    let mut state = ExpandState::new();

    for stack_file in stack_files {
        expand_stack_file(stack_file, None, 0, &opts, cache, &mut state);
    }

    // Filter real unit dirs: drop shared-module sources and any unit dirs we
    // already synthesised from a stack file.
    let filtered = unit_dirs
        .into_iter()
        .filter(|dir| {
            let canonical = dir.canonicalize_utf8().unwrap_or_else(|_| dir.clone());
            !state.excluded_sources.contains(&canonical) && !state.excluded_unit_dirs.contains(&canonical)
        })
        .collect();

    let report = *state.eval_report.borrow();
    ExpandedDiscovery {
        unit_dirs: filtered,
        synthetic_projects: state.synthetic_projects,
        eval_report: report,
    }
}

/// Shared mutable state threaded through every recursive `expand_stack_file`
/// call. Bundles accumulators so the helper signature stays manageable.
struct ExpandState {
    excluded_sources: HashSet<Utf8PathBuf>,
    excluded_unit_dirs: HashSet<Utf8PathBuf>,
    synthetic_projects: Vec<Project>,
    eval_report: Rc<RefCell<EvalReport>>,
    file_io_seen: FileIoSeen,
    /// Canonical stack-file paths currently on the expansion path (cycle guard).
    visited: HashSet<Utf8PathBuf>,
    /// Dedup key per emitted leaf: (canonical_source_module, unit.path).
    /// Prevents duplicates when discovery surfaces both the parent stack and
    /// its materialized `.terragrunt-stack/<x>/terragrunt.stack.hcl`.
    emitted_leaves: HashSet<(Utf8PathBuf, String)>,
}

impl ExpandState {
    fn new() -> Self {
        Self {
            excluded_sources: HashSet::new(),
            excluded_unit_dirs: HashSet::new(),
            synthetic_projects: Vec::new(),
            eval_report: Rc::new(RefCell::new(EvalReport::default())),
            file_io_seen: Rc::new(RefCell::new(HashSet::new())),
            visited: HashSet::new(),
            emitted_leaves: HashSet::new(),
        }
    }
}

/// Expand a single `terragrunt.stack.hcl` file into synthetic projects.
///
/// When `inherited_values` is `None` (top-level call from `expand_with_options`),
/// outer values are folded from any materialized `.terragrunt-stack/` ancestors.
/// When `Some`, recursion supplies the parent stack's evaluated `values = {...}`
/// directly so child `unit` / `stack` blocks see `values.X`.
fn expand_stack_file(
    stack_file: &Utf8Path,
    inherited_values: Option<hcl::Value>,
    depth: usize,
    opts: &ResolveOptions,
    cache: &ParseCache,
    state: &mut ExpandState,
) {
    // Cycle guard: if we are already mid-expansion of this stack file on the
    // current call chain, skip it. Also serves as the dedup point when both
    // discovery and recursion surface the same stack file.
    let canonical_stack_file = stack_file.canonicalize_utf8().unwrap_or_else(|_| stack_file.to_path_buf());
    if !state.visited.insert(canonical_stack_file.clone()) {
        state.eval_report.borrow_mut().cycle_skipped += 1;
        return;
    }

    let parsed = match parse_stack_file_cached(stack_file, cache) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Warning: failed to parse stack file {}: {}", stack_file, e);
            state.visited.remove(&canonical_stack_file);
            return;
        }
    };

    let stack_dir = match stack_file.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            eprintln!("Warning: stack file has no parent directory: {}", stack_file);
            state.visited.remove(&canonical_stack_file);
            return;
        }
    };

    // Top-level expansions infer outer values from the on-disk `.terragrunt-stack/`
    // ancestry; recursive expansions are given parent values explicitly.
    let outer_values_for_stack = match inherited_values {
        Some(v) => v,
        None => fold_frames(
            &collect_stack_frames(&stack_dir),
            *opts,
            Rc::clone(&state.eval_report),
            Rc::clone(&state.file_io_seen),
        )
        .unwrap_or_else(empty_object),
    };

    // Evaluate this stack's own `locals { ... }` once so `${local.*}`
    // references in unit sources resolve. We pass the outer values so
    // locals that depend on them can evaluate too.
    let stack_resolve_ctx_for_locals = resolve_ctx_for(
        stack_dir.clone(),
        Some(stack_dir.clone()),
        *opts,
        Rc::clone(&state.eval_report),
        Rc::clone(&state.file_io_seen),
        Some(stack_file.to_path_buf()),
    );
    let stack_locals = evaluate_locals(&parsed.body, &outer_values_for_stack, &stack_resolve_ctx_for_locals)
        .unwrap_or_else(|_| empty_object());

    let resolve_ctx = resolve_ctx_for(
        stack_dir.clone(),
        Some(stack_dir.clone()),
        *opts,
        Rc::clone(&state.eval_report),
        Rc::clone(&state.file_io_seen),
        Some(stack_file.to_path_buf()),
    )
    .with_values(outer_values_for_stack.clone())
    .with_locals(stack_locals.clone());

    for unit in &parsed.units {
        // Catch literal remote sources before path-join collapses `git::ssh://`
        // into `git::ssh:/`, which would defeat the post-resolve check below.
        if let PathExpr::Literal(s) = &unit.source
            && crate::parser::is_remote_path(camino::Utf8Path::new(s))
        {
            if unit.kind == BlockKind::Stack {
                state.eval_report.borrow_mut().remote_sources_skipped += 1;
            }
            continue;
        }

        let failures_before = state.eval_report.borrow().source_path_failures;
        let resolved_sources = resolve_ctx.resolve_all(&unit.source);
        if resolved_sources.is_empty() {
            eprintln!("Warning: could not resolve source for unit '{}' in {}; skipping", unit.path, stack_file);
            if state.eval_report.borrow().source_path_failures == failures_before {
                state.eval_report.borrow_mut().source_path_failures += 1;
            }
            continue;
        }

        let resolved_source = resolved_sources.into_iter().next().expect("non-empty");
        let canonical_source = resolved_source.canonicalize_utf8().unwrap_or(resolved_source.clone());

        if crate::parser::is_remote_path(&canonical_source) {
            if unit.kind == BlockKind::Stack {
                state.eval_report.borrow_mut().remote_sources_skipped += 1;
            }
            continue;
        }

        state.excluded_sources.insert(canonical_source.clone());

        let unit_dir = stack_dir.join(".terragrunt-stack").join(&unit.path);
        let canonical_unit_dir = unit_dir.canonicalize_utf8().unwrap_or_else(|_| unit_dir.clone());
        state.excluded_unit_dirs.insert(canonical_unit_dir.clone());

        // Bound values either come from the on-disk parent stack chain (top-level)
        // or from the inherited values already supplied to this call (recursion).
        let bound_values = if inherited_values_for_unit_is_inline(depth) {
            fold_frames(
                &collect_stack_frames(&unit_dir),
                *opts,
                Rc::clone(&state.eval_report),
                Rc::clone(&state.file_io_seen),
            )
        } else {
            evaluate_unit_values(unit, &outer_values_for_stack, &stack_locals, stack_file, &resolve_ctx)
                .or_else(|| Some(outer_values_for_stack.clone()))
        };

        if let Some(values) = bound_values.clone() {
            cache.stack_bindings.write().expect("stack bindings poisoned").insert(
                canonical_unit_dir.clone(),
                StackBinding {
                    values,
                    stack_file: stack_file.to_path_buf(),
                    parent_unit_path: unit.path.clone(),
                },
            );
        } else if unit.values_expr.is_some() {
            state.eval_report.borrow_mut().values_failures += 1;
        }

        // For `stack` blocks, try to recurse into the source's own
        // `terragrunt.stack.hcl`. On success, the parent shell entry is
        // dropped (the recursed leaves replace it). On any skip reason
        // (cycle, depth, remote, missing child) the shell is kept so the
        // user still sees something for this stack.
        if unit.kind == BlockKind::Stack {
            let child_stack_file = canonical_source.join("terragrunt.stack.hcl");
            let recurse_outcome = try_recurse_into_child_stack(
                &child_stack_file,
                unit,
                &outer_values_for_stack,
                &stack_locals,
                stack_file,
                &resolve_ctx,
                depth,
                opts,
                cache,
                state,
            );
            if recurse_outcome == RecurseOutcome::ChildrenEmitted {
                // Parent shell dropped: leaves take over for this stack.
                continue;
            }
        }

        let leaf_key = (canonical_source.clone(), unit.path.clone());
        if !state.emitted_leaves.insert(leaf_key) {
            // Already emitted via another visit (e.g. discovery and recursion
            // both surfaced this leaf).
            continue;
        }

        let project = build_synthetic_project(&unit_dir, &canonical_source, stack_file, cache, bound_values);
        state.synthetic_projects.push(project);
    }

    state.visited.remove(&canonical_stack_file);
}

/// For top-level calls (depth 0) we infer `values` from any on-disk
/// `.terragrunt-stack/` chain. Recursive calls always carry inherited values
/// directly, so this helper just gates the legacy fold-from-disk path.
fn inherited_values_for_unit_is_inline(depth: usize) -> bool {
    depth == 0
}

/// Outcome of a child-stack recursion attempt.
#[derive(Debug, PartialEq, Eq)]
enum RecurseOutcome {
    /// Recursion ran and emitted at least one synthetic leaf — caller drops
    /// the parent shell entry.
    ChildrenEmitted,
    /// Recursion was skipped (remote, missing, cycle, depth). Caller keeps
    /// the parent shell entry.
    Skipped,
}

#[allow(clippy::too_many_arguments)]
fn try_recurse_into_child_stack(
    child_stack_file: &Utf8Path,
    parent_unit: &StackUnit,
    outer_values: &hcl::Value,
    parent_locals: &hcl::Value,
    parent_stack_file: &Utf8Path,
    parent_resolve_ctx: &ResolveContext,
    depth: usize,
    opts: &ResolveOptions,
    cache: &ParseCache,
    state: &mut ExpandState,
) -> RecurseOutcome {
    if !child_stack_file.exists() {
        state.eval_report.borrow_mut().missing_child_stack += 1;
        return RecurseOutcome::Skipped;
    }

    let canonical_child = child_stack_file.canonicalize_utf8().unwrap_or_else(|_| child_stack_file.to_path_buf());
    if state.visited.contains(&canonical_child) {
        state.eval_report.borrow_mut().cycle_skipped += 1;
        return RecurseOutcome::Skipped;
    }

    if depth + 1 > opts.max_recursion_depth {
        state.eval_report.borrow_mut().recursion_depth_exceeded += 1;
        return RecurseOutcome::Skipped;
    }

    // Evaluate the parent's own `values = {...}` so the child sees them as
    // `values.X`. When the parent has no values expression, fall back to the
    // outer values currently in scope.
    let evaluated_values =
        evaluate_unit_values(parent_unit, outer_values, parent_locals, parent_stack_file, parent_resolve_ctx)
            .unwrap_or_else(|| outer_values.clone());

    let leaves_before = state.synthetic_projects.len();
    expand_stack_file(&canonical_child, Some(evaluated_values), depth + 1, opts, cache, state);
    if state.synthetic_projects.len() > leaves_before {
        RecurseOutcome::ChildrenEmitted
    } else {
        RecurseOutcome::Skipped
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
    resolve_ctx: &ResolveContext,
) -> Option<hcl::Value> {
    let expr = unit.values_expr.as_ref()?;
    let ctx = build_context(outer_values, locals, resolve_ctx);
    match evaluate_expression_with_ctx(expr, &ctx, resolve_ctx) {
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
fn fold_frames(
    frames: &[StackFrame],
    opts: ResolveOptions,
    eval_report: Rc<RefCell<EvalReport>>,
    file_io_seen: FileIoSeen,
) -> Option<hcl::Value> {
    let mut outer = empty_object();
    for frame in frames {
        let frame_dir = frame.stack_file.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| Utf8PathBuf::from("."));
        let resolve_ctx = resolve_ctx_for(
            frame_dir.clone(),
            Some(frame_dir),
            opts,
            Rc::clone(&eval_report),
            Rc::clone(&file_io_seen),
            Some(frame.stack_file.clone()),
        );
        let locals =
            evaluate_locals(&frame.parsed.body, &outer, &resolve_ctx).unwrap_or_else(|_| empty_object());
        let unit = &frame.parsed.units[frame.unit_index];
        let evaluated = evaluate_unit_values(unit, &outer, &locals, &frame.stack_file, &resolve_ctx)?;
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

    /// Copy an in-tree fixture into a tempdir and create a `.git` marker
    /// at its root so `find_repo_root` lands there. Returns the live
    /// `TempDir` guard (drop = cleanup) and its utf8 root path.
    fn materialize_fixture_with_git_marker(name: &str) -> (tempfile::TempDir, Utf8PathBuf) {
        let src = fixture_path(name);
        let tmp = tempfile::tempdir().expect("create tempdir");
        copy_dir_recursive(src.as_std_path(), tmp.path()).expect("copy fixture");
        std::fs::create_dir_all(tmp.path().join(".git")).expect("create .git marker");
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 tempdir path");
        (tmp, root)
    }

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let dst_path = dst.join(entry.file_name());
            if file_type.is_dir() {
                copy_dir_recursive(&entry.path(), &dst_path)?;
            } else {
                std::fs::copy(entry.path(), &dst_path)?;
            }
        }
        Ok(())
    }

    #[test]
    fn test_expand_source_with_local_resolves() {
        let (_tmp, fixture_root) = materialize_fixture_with_git_marker("source_with_get_repo_root");

        let stack_file = fixture_root.join("live/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        assert_eq!(
            expanded.synthetic_projects.len(),
            1,
            "expected 1 synthetic unit, got: {:?}",
            expanded.synthetic_projects.iter().map(|p| &p.dir).collect::<Vec<_>>()
        );
        let unit = &expanded.synthetic_projects[0];
        assert!(
            unit.dir.as_str().ends_with("/.terragrunt-stack/unseal"),
            "synthetic unit dir mismatch: {}",
            unit.dir
        );
        // Soft failures must not have fired for this fully resolved fixture.
        assert!(
            expanded.eval_report.is_empty(),
            "expected no eval failures, got: {:?}",
            expanded.eval_report
        );
    }

    #[test]
    fn test_expand_nested_stack_source_uses_outer_values() {
        // An outer stack passes `values = { module = "gcp" }` into its inner
        // stack, whose `source` then interpolates `${values.module}`. The
        // resolver must thread the outer values through to the inner source.
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        std::fs::create_dir_all(root.join("modules/gcp").as_std_path()).unwrap();
        std::fs::write(
            root.join("terragrunt.stack.hcl").as_std_path(),
            r#"
            stack "inner" {
              source = "./inner"
              path   = "inner"
              values = { module = "gcp" }
            }
            "#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".terragrunt-stack/inner").as_std_path()).unwrap();
        std::fs::write(
            root.join(".terragrunt-stack/inner/terragrunt.stack.hcl").as_std_path(),
            r#"
            unit "leaf" {
              source = "../../../modules/${values.module}"
              path   = "leaf"
            }
            "#,
        )
        .unwrap();

        let outer = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let inner = root.join(".terragrunt-stack/inner/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[outer, inner], &cache);

        let leaf = expanded
            .synthetic_projects
            .iter()
            .find(|p| p.dir.as_str().ends_with("/leaf"))
            .expect("leaf unit must exist");
        assert!(
            leaf.dir.as_str().contains(".terragrunt-stack/inner/.terragrunt-stack/leaf"),
            "unexpected leaf dir: {}",
            leaf.dir
        );
    }

    #[test]
    fn test_expand_source_with_local_referencing_unresolvable_skipped() {
        // Build a stack file whose `source = local.missing` cannot resolve.
        let dir = tempfile::tempdir().unwrap();
        let stack_dir = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        let stack_file = stack_dir.join("terragrunt.stack.hcl");
        std::fs::write(
            stack_file.as_std_path(),
            r#"
            unit "broken" {
              source = local.missing
              path   = "broken"
            }
            "#,
        )
        .unwrap();

        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[stack_file], &cache);

        // The unit must be skipped (no synthetic project) and the failure
        // must be recorded exactly once (the resolver helper records it; the
        // unit loop must not double-count).
        assert!(expanded.synthetic_projects.is_empty());
        assert_eq!(
            expanded.eval_report.source_path_failures, 1,
            "expected exactly one source_path_failure, got: {:?}",
            expanded.eval_report
        );
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

    // ============== Path D: recursive stack-source expansion ==============

    /// Write a `terragrunt.stack.hcl` at `dir/terragrunt.stack.hcl`.
    fn write_stack(dir: &Utf8Path, body: &str) -> Utf8PathBuf {
        std::fs::create_dir_all(dir.as_std_path()).unwrap();
        let path = dir.join("terragrunt.stack.hcl");
        std::fs::write(path.as_std_path(), body).unwrap();
        path
    }

    /// Write a stub `terragrunt.hcl` so a synthetic unit looks like a real module.
    fn write_terragrunt(dir: &Utf8Path) {
        std::fs::create_dir_all(dir.as_std_path()).unwrap();
        std::fs::write(dir.join("terragrunt.hcl").as_std_path(), "terraform { source = \".\" }\n").unwrap();
    }

    #[test]
    fn test_recurse_into_stack_source_emits_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root,
            r#"
            stack "x" {
              source = "./inner"
              path   = "x"
            }
            "#,
        );
        let inner_dir = root.join("inner");
        write_stack(
            &inner_dir,
            r#"
            unit "leaf" {
              source = "../../module"
              path   = "leaf"
            }
            "#,
        );
        write_terragrunt(&root.join("module"));

        let top = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top], &cache);

        assert!(expanded.synthetic_projects.iter().any(|p| p.dir.as_str().ends_with("/.terragrunt-stack/leaf")));
        assert!(
            !expanded.synthetic_projects.iter().any(|p| p.dir.as_str().ends_with("/.terragrunt-stack/x")),
            "parent shell should have been dropped after successful recursion"
        );
    }

    #[test]
    fn test_recurse_drops_shell_when_children_emitted() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root,
            r#"
            stack "outer" {
              source = "./outer"
              path   = "outer"
            }
            "#,
        );
        write_stack(
            &root.join("outer"),
            r#"
            unit "a" {
              source = "../../mod_a"
              path   = "a"
            }
            unit "b" {
              source = "../../mod_b"
              path   = "b"
            }
            "#,
        );
        write_terragrunt(&root.join("mod_a"));
        write_terragrunt(&root.join("mod_b"));

        let top = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top], &cache);

        let dirs: Vec<&str> = expanded.synthetic_projects.iter().map(|p| p.dir.as_str()).collect();
        assert!(!dirs.iter().any(|d| d.ends_with("/.terragrunt-stack/outer")), "outer shell must be dropped: {:?}", dirs);
        assert_eq!(expanded.synthetic_projects.len(), 2, "exactly the two leaves should remain: {:?}", dirs);
    }

    #[test]
    fn test_recurse_keeps_shell_when_source_missing_stack_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root,
            r#"
            stack "only_tg" {
              source = "./only_tg"
              path   = "only_tg"
            }
            "#,
        );
        // Source dir exists with terragrunt.hcl but NO terragrunt.stack.hcl.
        write_terragrunt(&root.join("only_tg"));

        let top = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top], &cache);

        assert_eq!(expanded.eval_report.missing_child_stack, 1, "missing child stack should be reported");
        assert!(
            expanded.synthetic_projects.iter().any(|p| p.dir.as_str().ends_with("/.terragrunt-stack/only_tg")),
            "parent shell must be kept when recursion can't find a child stack file"
        );
    }

    #[test]
    fn test_recurse_skips_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        // a.source = b, b.source = a — recursion should short-circuit on the
        // second visit. Each stack file also declares its own leaf so that the
        // first arm of the cycle still emits something.
        write_stack(
            &root.join("a"),
            r#"
            stack "b" {
              source = "../b"
              path   = "b"
            }
            unit "leaf_a" {
              source = "../../mod_a"
              path   = "leaf_a"
            }
            "#,
        );
        write_stack(
            &root.join("b"),
            r#"
            stack "a" {
              source = "../a"
              path   = "a"
            }
            unit "leaf_b" {
              source = "../../mod_b"
              path   = "leaf_b"
            }
            "#,
        );
        write_terragrunt(&root.join("mod_a"));
        write_terragrunt(&root.join("mod_b"));

        let top = root.join("a/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top], &cache);

        assert!(expanded.eval_report.cycle_skipped >= 1, "expected at least one cycle_skipped, got: {:?}", expanded.eval_report);
    }

    #[test]
    fn test_recurse_skips_remote_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root,
            r#"
            stack "remote" {
              source = "git::ssh://example.com/foo.git"
              path   = "remote"
            }
            "#,
        );

        let top = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top], &cache);

        assert_eq!(expanded.eval_report.remote_sources_skipped, 1, "remote sources should be counted");
        assert!(expanded.synthetic_projects.is_empty(), "no leaves expected for a remote stack source");
    }

    #[test]
    fn test_recurse_inherits_parent_values() {
        // Parent passes `values = { module = "../modules/eu" }` into the child
        // stack; the child unit's `source = values.module` then resolves
        // against the child stack's config_dir. The bound values must arrive
        // intact through the recursion path.
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root,
            r#"
            stack "child" {
              source = "./child"
              path   = "child"
              values = { module = "../modules/eu" }
            }
            "#,
        );
        write_stack(
            &root.join("child"),
            r#"
            unit "x" {
              source = values.module
              path   = "x"
            }
            "#,
        );
        write_terragrunt(&root.join("modules/eu"));

        let top = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top], &cache);

        let leaf = expanded
            .synthetic_projects
            .iter()
            .find(|p| p.dir.as_str().ends_with("/.terragrunt-stack/x"))
            .unwrap_or_else(|| panic!("expected leaf 'x'; got: {:?}", expanded.synthetic_projects.iter().map(|p| &p.dir).collect::<Vec<_>>()));
        // The bound source module (resolved relative to the child stack dir)
        // should appear in the unit's watch_files glob.
        assert!(
            leaf.watch_files.iter().any(|f| f.as_str().contains("/modules/eu")),
            "expected source to resolve under modules/eu; watch_files: {:?}",
            leaf.watch_files
        );
    }

    #[test]
    fn test_recurse_dedup_with_materialized_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root.join("live"),
            r#"
            stack "consul" {
              source = "../consul_src"
              path   = "consul"
            }
            "#,
        );
        write_stack(
            &root.join("consul_src"),
            r#"
            unit "agent" {
              source = "../modules/agent"
              path   = "agent"
            }
            "#,
        );
        write_terragrunt(&root.join("modules/agent"));

        // Also materialize the inner stack file under .terragrunt-stack/ —
        // this is what `terragrunt stack generate` produces.
        write_stack(
            &root.join("live/.terragrunt-stack/consul"),
            r#"
            unit "agent" {
              source = "../../../modules/agent"
              path   = "agent"
            }
            "#,
        );

        let top = root.join("live/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let materialized =
            root.join("live/.terragrunt-stack/consul/terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top, materialized], &cache);

        let agent_count = expanded.synthetic_projects.iter().filter(|p| p.dir.as_str().ends_with("/agent")).count();
        assert_eq!(agent_count, 1, "leaf 'agent' should appear exactly once; got: {:?}", expanded.synthetic_projects.iter().map(|p| &p.dir).collect::<Vec<_>>());
    }

    #[test]
    fn test_mixed_inline_unit_and_stack_at_top() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root,
            r#"
            unit "inline" {
              source = "./mod_inline"
              path   = "inline"
            }
            stack "outer" {
              source = "./outer"
              path   = "outer"
            }
            "#,
        );
        write_stack(
            &root.join("outer"),
            r#"
            unit "child" {
              source = "../../mod_child"
              path   = "child"
            }
            "#,
        );
        write_terragrunt(&root.join("mod_inline"));
        write_terragrunt(&root.join("mod_child"));

        let top = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand(Vec::new(), &[top], &cache);

        let dirs: Vec<&str> = expanded.synthetic_projects.iter().map(|p| p.dir.as_str()).collect();
        assert!(dirs.iter().any(|d| d.ends_with("/.terragrunt-stack/inline")), "inline unit must be emitted: {:?}", dirs);
        assert!(dirs.iter().any(|d| d.ends_with("/.terragrunt-stack/child")), "recursed child must be emitted: {:?}", dirs);
        assert!(!dirs.iter().any(|d| d.ends_with("/.terragrunt-stack/outer")), "outer shell must be dropped: {:?}", dirs);
    }

    #[test]
    fn test_recurse_strict_escalates_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        write_stack(
            &root,
            r#"
            stack "nope" {
              source = "./nope"
              path   = "nope"
            }
            "#,
        );
        // Note: no `./nope` dir created — the resolver returns the (still
        // normalized) path so canonicalization is best-effort; the
        // child stack file does not exist, increment `missing_child_stack`.

        let top = root.join("terragrunt.stack.hcl").canonicalize_utf8().unwrap();
        let cache = ParseCache::new();
        let expanded = expand_with_options(
            Vec::new(),
            &[top],
            &cache,
            ResolveOptions {
                strict: true,
                ..Default::default()
            },
        );

        assert!(expanded.eval_report.total() > 0, "strict should record an eval failure; got: {:?}", expanded.eval_report);
    }
}
