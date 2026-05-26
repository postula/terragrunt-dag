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
            "tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/bdo/terragrunt.hcl";

        let unit = project_with(
            "/repo/tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/bdo",
            vec![],
        );
        let mut changed = HashSet::new();
        changed.insert(toplevel.join(diff_line));

        assert!(unit_is_changed(&unit, &changed));
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
            "tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/bdo/terragrunt.hcl";

        let unit = project_with(
            "/repo/tests/fixtures/mixed_live_and_stack/live/staging/auth/.terragrunt-stack/azure/bdo",
            vec![],
        );
        let mut changed = HashSet::new();
        changed.insert(scan_root.join(diff_line));

        assert!(!unit_is_changed(&unit, &changed));
    }
}
