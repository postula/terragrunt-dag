//! Change detection: compare a unit's watch_files and dir against a git diff.

use crate::Project;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `git -C <repo_root> diff --name-only <base_ref>...HEAD` and return the
/// set of changed paths as absolute paths.
///
/// `git diff --name-only` always emits paths relative to the git repo's
/// top-level, regardless of `-C`. Joining those against `repo_root` would
/// double up the prefix whenever `repo_root` is a subdirectory of the git
/// toplevel. We therefore ask git for its toplevel and join against that.
///
/// Paths are not canonicalized — deleted files no longer exist on disk but
/// are still relevant for change detection.
///
/// On any failure (no git, no repo, bad ref), returns an Err. Caller decides
/// whether to fall back to "all changed".
pub fn diff_against(base_ref: &str, repo_root: &Path) -> std::io::Result<HashSet<PathBuf>> {
    let toplevel = git_toplevel(repo_root)?;

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("diff")
        .arg("--name-only")
        .arg(format!("{base_ref}...HEAD"))
        // `--` terminates option parsing so a malicious base_ref (e.g.
        // `--upload-pack=evil`) cannot be interpreted as a git option.
        .arg("--")
        .output()?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!("git diff failed: {}", String::from_utf8_lossy(&output.stderr))));
    }

    let mut set = HashSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() {
            continue;
        }
        // Diff paths are toplevel-relative. Logical join with the toplevel
        // yields an absolute path that lines up with how project dirs are
        // discovered (i.e. children of the canonicalized scan root, which is
        // itself a child of the git toplevel).
        set.insert(toplevel.join(line));
    }
    Ok(set)
}

/// Run `git -C <repo_root> rev-parse --show-toplevel` to find the root of the
/// git working tree that contains `repo_root`.
fn git_toplevel(repo_root: &Path) -> std::io::Result<PathBuf> {
    let output = Command::new("git").arg("-C").arg(repo_root).arg("rev-parse").arg("--show-toplevel").output()?;

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "git rev-parse --show-toplevel failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let toplevel = String::from_utf8_lossy(&output.stdout).trim_end().to_string();
    if toplevel.is_empty() {
        return Err(std::io::Error::other("git rev-parse --show-toplevel returned empty output"));
    }

    Ok(PathBuf::from(toplevel))
}

/// Compute the set of projects (by `dir` string) that are changed.
///
/// A project is always included if its own `watch_files`/`dir` overlap the
/// changed paths (per [`unit_is_changed`]). When `cascade == true`, the set is
/// then extended via transitive propagation across dependency edges: a project
/// is also included if any of its `project_dependencies` is in the set. The
/// propagation iterates to a fixed point so it is independent of project order.
///
/// When `cascade == false`, only the directly-changed seed set is returned.
///
/// Invariant: keys are absolute path strings taken from `Project.dir`. Entries
/// of `project_dependencies` must use the same canonical form (they are
/// populated as absolute paths in `crate::processor`). A mismatch would
/// silently drop propagation, hence the debug assertion below.
pub fn compute_changed_units(
    projects: &[Project],
    changed: &HashSet<PathBuf>,
    cascade: bool,
) -> HashSet<String> {
    // Catch silent identity drift between `Project.dir` keys and the strings
    // stored in `project_dependencies`. Both must be absolute paths.
    debug_assert!(
        projects.iter().flat_map(|p| p.project_dependencies.iter()).all(|dep| Path::new(dep).is_absolute()),
        "compute_changed_units: project_dependencies must be absolute paths to match `dir` keys",
    );

    // Seed with units whose own files changed.
    let mut changed_units: HashSet<String> =
        projects.iter().filter(|p| unit_is_changed(p, changed)).map(|p| p.dir.to_string()).collect();

    if !cascade {
        return changed_units;
    }

    // Propagate: a unit is changed if any of its deps is changed. Fixed-point
    // pass because deps may reference units we haven't visited yet on this pass.
    loop {
        let mut added = false;
        for p in projects {
            let key = p.dir.to_string();
            if changed_units.contains(&key) {
                continue;
            }
            if p.project_dependencies.iter().any(|dep| changed_units.contains(dep)) {
                changed_units.insert(key);
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    changed_units
}

/// True if any changed path is inside `unit.dir`, matches a watch_file exactly,
/// or lives under a directory-style watch entry (terraform module source).
pub fn unit_is_changed(unit: &Project, changed: &HashSet<PathBuf>) -> bool {
    let unit_dir = Path::new(unit.dir.as_str());
    for f in changed {
        if f.starts_with(unit_dir) {
            return true;
        }
        for w in &unit.watch_files {
            let w_path = Path::new(w.as_str());
            if crate::output::looks_like_directory(w) {
                if f.starts_with(w_path) {
                    return true;
                }
            } else if f == w_path {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn project_with(dir: &str, watch_files: Vec<&str>) -> Project {
        Project {
            name: "p".to_string(),
            dir: Utf8PathBuf::from(dir),
            project_dependencies: vec![],
            watch_files: watch_files.into_iter().map(Utf8PathBuf::from).collect(),
            has_terraform_source: true,
        }
    }

    #[test]
    fn test_unit_is_changed_file_inside_dir() {
        let unit = project_with("/repo/live/prod/vpc", vec![]);
        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/repo/live/prod/vpc/terragrunt.hcl"));

        assert!(unit_is_changed(&unit, &changed));
    }

    #[test]
    fn test_unit_is_changed_exact_watch_file() {
        let unit = project_with("/repo/live/prod/vpc", vec!["/repo/root.hcl"]);
        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/repo/root.hcl"));

        assert!(unit_is_changed(&unit, &changed));
    }

    #[test]
    fn test_unit_is_changed_module_source_dir() {
        // No extension on /repo/modules/vpc → treated as a directory-style
        // watch entry. Any file under it should mark the unit as changed.
        let unit = project_with("/repo/live/prod/vpc", vec!["/repo/modules/vpc"]);
        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/repo/modules/vpc/main.tf"));

        assert!(unit_is_changed(&unit, &changed));
    }

    #[test]
    fn test_unit_not_changed_when_no_overlap() {
        let unit = project_with("/repo/live/prod/vpc", vec!["/repo/root.hcl"]);
        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/repo/live/dev/app/main.tf"));
        changed.insert(PathBuf::from("/repo/docs/README.md"));

        assert!(!unit_is_changed(&unit, &changed));
    }

    /// Regression: when the user scans a subdirectory of the git repo (e.g.
    /// `tests/fixtures/foo`), `git diff --name-only` returns paths relative
    /// to the git toplevel, not to the scan dir. `diff_against` must join
    /// against the toplevel so the resulting absolute paths line up with the
    /// canonical (absolute) `Project.dir` produced by discovery.
    #[test]
    fn test_unit_is_changed_when_scan_root_is_subdir_of_repo() {
        // Simulate the post-fix shape: toplevel-relative diff entries joined
        // against the git toplevel, compared with an absolute unit dir that
        // lives several levels below the toplevel.
        let toplevel = Path::new("/repo");
        let diff_line =
            "tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/alpha/terragrunt.hcl";

        let unit = project_with(
            "/repo/tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/alpha",
            vec![],
        );
        let mut changed = HashSet::new();
        changed.insert(toplevel.join(diff_line));

        assert!(unit_is_changed(&unit, &changed));
    }

    fn project(dir: &str, watch_files: Vec<&str>, deps: Vec<&str>) -> Project {
        Project {
            name: dir.trim_start_matches('/').replace('/', "_"),
            dir: Utf8PathBuf::from(dir),
            project_dependencies: deps.into_iter().map(String::from).collect(),
            watch_files: watch_files.into_iter().map(Utf8PathBuf::from).collect(),
            has_terraform_source: true,
        }
    }

    /// Regression for v0.5.0: a shared file (e.g. `live/terragrunt.stack.hcl`)
    /// referenced by some units must NOT flag unrelated units as changed via
    /// inherited watch_files. Units whose own watch_files do not reference the
    /// shared file and whose dep chain does not reach a changed unit stay clean.
    #[test]
    fn test_compute_changed_units_shared_file_does_not_flag_unrelated() {
        let shared = "/repo/live/terragrunt.stack.hcl";

        // Five units, all in disjoint directories. Only `a` references the
        // shared file in its own watch_files; the others don't.
        let projects = vec![
            project("/repo/live/a", vec![shared], vec![]),
            project("/repo/live/b", vec![], vec![]),
            project("/repo/live/c", vec![], vec![]),
            project("/repo/live/d", vec![], vec![]),
            project("/repo/live/e", vec![], vec![]),
        ];

        let mut changed = HashSet::new();
        changed.insert(PathBuf::from(shared));

        let result = compute_changed_units(&projects, &changed, true);

        assert!(result.contains("/repo/live/a"), "a directly watches the shared file");
        for unrelated in ["/repo/live/b", "/repo/live/c", "/repo/live/d", "/repo/live/e"] {
            assert!(!result.contains(unrelated), "{} must not be flagged; got: {:?}", unrelated, result);
        }
    }

    /// Propagation: if A depends on B and B's own files changed, A must also
    /// be flagged changed — even though A's own watch_files don't mention the
    /// changed path.
    #[test]
    fn test_compute_changed_units_propagates_via_deps() {
        let projects = vec![
            project("/repo/live/b", vec![], vec![]),
            project("/repo/live/a", vec![], vec!["/repo/live/b"]),
        ];

        let mut changed = HashSet::new();
        // Change a file inside B's dir.
        changed.insert(PathBuf::from("/repo/live/b/main.tf"));

        let result = compute_changed_units(&projects, &changed, true);

        assert!(result.contains("/repo/live/b"), "B's own file changed");
        assert!(result.contains("/repo/live/a"), "A depends on B and must propagate");
    }

    /// Propagation fixed-point: A → B → C, change in C must propagate to both
    /// B and A regardless of iteration order.
    #[test]
    fn test_compute_changed_units_propagates_transitively() {
        let projects = vec![
            project("/repo/live/a", vec![], vec!["/repo/live/b"]),
            project("/repo/live/b", vec![], vec!["/repo/live/c"]),
            project("/repo/live/c", vec![], vec![]),
        ];

        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/repo/live/c/main.tf"));

        let result = compute_changed_units(&projects, &changed, true);

        assert_eq!(result.len(), 3, "all of A, B, C must be flagged; got: {:?}", result);
    }

    /// `cascade=false` returns only directly-changed units. Chain A → B → C
    /// with C seeded: A and B must NOT be flagged.
    #[test]
    fn test_compute_changed_units_no_cascade_returns_only_seed() {
        let projects = vec![
            project("/repo/live/a", vec![], vec!["/repo/live/b"]),
            project("/repo/live/b", vec![], vec!["/repo/live/c"]),
            project("/repo/live/c", vec![], vec![]),
        ];

        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/repo/live/c/main.tf"));

        let result = compute_changed_units(&projects, &changed, false);

        assert_eq!(result.len(), 1, "only C should be flagged when cascade=false; got: {:?}", result);
        assert!(result.contains("/repo/live/c"), "C is the seed");
    }

    /// Cycle regression: A → B → C → A. Fixed-point propagation must terminate
    /// (no infinite loop) and flag the entire cycle when one node is seeded.
    #[test]
    fn test_compute_changed_units_cycle_terminates_and_flags_all() {
        let projects = vec![
            project("/repo/live/a", vec![], vec!["/repo/live/b"]),
            project("/repo/live/b", vec![], vec!["/repo/live/c"]),
            project("/repo/live/c", vec![], vec!["/repo/live/a"]),
        ];

        let mut changed = HashSet::new();
        changed.insert(PathBuf::from("/repo/live/a/main.tf"));

        let result = compute_changed_units(&projects, &changed, true);

        assert_eq!(result.len(), 3, "every node in the cycle must be flagged; got: {:?}", result);
        for node in ["/repo/live/a", "/repo/live/b", "/repo/live/c"] {
            assert!(result.contains(node), "{} missing from cycle propagation; got: {:?}", node, result);
        }
    }

    /// Regression: confirm the pre-fix bug shape (joining diff lines against
    /// a scan dir that is a *deeper* directory than the git toplevel)
    /// would have failed — guards against accidentally re-introducing that
    /// join target.
    #[test]
    fn test_unit_not_changed_with_doubled_prefix_path() {
        // What the buggy code produced: scan_root.join(repo_relative_line)
        // duplicates the `tests/fixtures/...` segment.
        let scan_root = Path::new("/repo/tests/fixtures/mixed_live_and_stack");
        let diff_line =
            "tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/alpha/terragrunt.hcl";

        let unit = project_with(
            "/repo/tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/alpha",
            vec![],
        );
        let mut changed = HashSet::new();
        changed.insert(scan_root.join(diff_line));

        assert!(!unit_is_changed(&unit, &changed));
    }
}
