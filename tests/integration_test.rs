//! End-to-end integration tests.

use std::path::Path;
use std::process::Command;

use camino::Utf8PathBuf;
use tempfile::TempDir;

/// Copy an in-tree fixture into a fresh tempdir and materialize a `.git`
/// marker at its root so `find_repo_root` lands there at runtime. The
/// tempdir auto-cleans on drop; the in-tree fixture stays pristine
/// (committing `.git/` inside the repo would break outer VCS operations).
fn materialize_fixture_with_git_marker(name: &str) -> (TempDir, Utf8PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let tmp = TempDir::new().expect("create tempdir");
    copy_dir_recursive(&src, tmp.path()).expect("copy fixture");
    std::fs::create_dir_all(tmp.path().join(".git")).expect("create .git marker");
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 tempdir path");
    (tmp, root)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
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

fn cargo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_terragrunt-dag"))
}

fn fixture_path(name: &str) -> Utf8PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Utf8PathBuf::from(manifest_dir).join("tests/fixtures").join(name)
}

#[derive(serde::Deserialize)]
struct JsonOutput {
    projects: Vec<JsonProject>,
}

#[derive(serde::Deserialize)]
struct JsonProject {
    dir: String,
    #[serde(default)]
    watch_files: Vec<String>,
}

fn run_json(root: &Utf8PathBuf) -> JsonOutput {
    let output = cargo_bin().args(["--format", "json", root.as_str()]).output().expect("Failed to execute binary");

    assert!(output.status.success(), "binary failed: stderr={}", String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {}: {}", e, stdout))
}

#[test]
fn stack_with_shared_module_excludes_source_and_emits_units() {
    let root = fixture_path("stack/with_shared_module");
    let parsed = run_json(&root);

    // Shared module dir should NOT appear in output.
    assert!(
        !parsed.projects.iter().any(|p| p.dir.contains("/_modules/")),
        "shared module should be excluded; got dirs: {:?}",
        parsed.projects.iter().map(|p| &p.dir).collect::<Vec<_>>()
    );

    // Two synthetic units expected.
    let synthetic: Vec<&JsonProject> = parsed.projects.iter().filter(|p| p.dir.contains(".terragrunt-stack")).collect();
    assert_eq!(
        synthetic.len(),
        2,
        "expected 2 synthetic units, got: {:?}",
        parsed.projects.iter().map(|p| &p.dir).collect::<Vec<_>>()
    );

    // Each synthetic unit should watch the parent stack file.
    for unit in &synthetic {
        assert!(
            unit.watch_files.iter().any(|f| f.ends_with("terragrunt.stack.hcl")),
            "unit {} should watch the parent stack file, got watch_files={:?}",
            unit.dir,
            unit.watch_files
        );
    }
}

#[test]
fn stack_with_generated_dir_does_not_duplicate() {
    let root = fixture_path("stack/with_generated_dir");
    let parsed = run_json(&root);

    // Shared module dir excluded.
    assert!(
        !parsed.projects.iter().any(|p| p.dir.contains("/_modules/")),
        "shared module should be excluded; got dirs: {:?}",
        parsed.projects.iter().map(|p| &p.dir).collect::<Vec<_>>()
    );

    // Generated units are discovered AND synthesised — but should appear only once each.
    let azure_count = parsed.projects.iter().filter(|p| p.dir.ends_with("/azure/alpha")).count();
    let saml_count = parsed.projects.iter().filter(|p| p.dir.ends_with("/saml/beta")).count();
    assert_eq!(
        azure_count,
        1,
        "azure/alpha emitted {} times, expected 1; dirs: {:?}",
        azure_count,
        parsed.projects.iter().map(|p| &p.dir).collect::<Vec<_>>()
    );
    assert_eq!(saml_count, 1, "saml/beta emitted {} times, expected 1", saml_count);

    // Each unit should watch the stack file.
    for unit in parsed.projects.iter().filter(|p| p.dir.contains(".terragrunt-stack")) {
        assert!(
            unit.watch_files.iter().any(|f| f.ends_with("terragrunt.stack.hcl")),
            "unit {} should watch the parent stack file, got watch_files={:?}",
            unit.dir,
            unit.watch_files
        );
    }
}

#[derive(serde::Deserialize)]
struct GhaOutput {
    include: Vec<GhaProject>,
}

#[derive(serde::Deserialize)]
struct GhaProject {
    name: String,
    #[serde(rename = "working-directory")]
    working_directory: String,
}

#[test]
fn expand_resolves_source_with_get_repo_root_and_local() {
    let (_tmp, root) = materialize_fixture_with_git_marker("stack/source_with_get_repo_root");
    let output = cargo_bin().args(["--format", "gha", root.as_str()]).output().expect("Failed to execute binary");

    assert!(output.status.success(), "binary failed: stderr={}", String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: GhaOutput =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {}: {}", e, stdout));

    let unseal = parsed
        .include
        .iter()
        .find(|p| p.name.contains("unseal"))
        .unwrap_or_else(|| panic!("expected synthetic `unseal` unit; rows: {:?}", parsed.include.iter().map(|p| &p.name).collect::<Vec<_>>()));

    // Source resolves via `${get_repo_root()}/modules/vault/unseal/${local.unseal_type}`;
    // the synthetic unit lives under the stack's `.terragrunt-stack/unseal`.
    assert!(
        unseal.working_directory.ends_with("/.terragrunt-stack/unseal"),
        "unexpected working-directory: {}",
        unseal.working_directory
    );
}

#[test]
fn recursive_stack_sources_emits_leaves_not_shells() {
    let (_tmp, root) = materialize_fixture_with_git_marker("stack/recursive_stack_sources");
    let live = root.join("live");

    // GHA view: assert the right leaves appear and no shell entries leak.
    let gha = cargo_bin().args(["--format", "gha", live.as_str()]).output().expect("Failed to execute binary");
    assert!(gha.status.success(), "gha binary failed: stderr={}", String::from_utf8_lossy(&gha.stderr));
    let gha_stdout = String::from_utf8_lossy(&gha.stdout);
    let gha_parsed: GhaOutput =
        serde_json::from_str(&gha_stdout).unwrap_or_else(|e| panic!("invalid GHA JSON: {}: {}", e, gha_stdout));
    let names: Vec<&String> = gha_parsed.include.iter().map(|p| &p.name).collect();

    assert!(
        !gha_parsed.include.iter().any(|p| p.working_directory.ends_with("/.terragrunt-stack/consul")),
        "consul shell leaked into output: {:?}",
        names
    );
    assert!(
        !gha_parsed.include.iter().any(|p| p.working_directory.ends_with("/.terragrunt-stack/vault")),
        "vault shell leaked into output: {:?}",
        names
    );

    let leaf_suffixes =
        ["consul/.terragrunt-stack/agent", "vault/.terragrunt-stack/core", "vault/.terragrunt-stack/unseal"];
    for suffix in leaf_suffixes {
        let count = gha_parsed.include.iter().filter(|p| p.working_directory.ends_with(suffix)).count();
        assert_eq!(count, 1, "expected exactly one entry ending with {}; got: {:?}", suffix, names);
    }
    assert_eq!(gha_parsed.include.len(), 3, "expected 3 leaf entries, got: {:?}", names);

    // JSON view: assert `unseal`'s source module path contains `shamir`,
    // proving Path B's `${local.unseal_type}` evaluation works inside Path D.
    let json = cargo_bin().args(["--format", "json", live.as_str()]).output().expect("Failed to execute binary");
    assert!(json.status.success(), "json binary failed: stderr={}", String::from_utf8_lossy(&json.stderr));
    let json_stdout = String::from_utf8_lossy(&json.stdout);
    let json_parsed: JsonOutput =
        serde_json::from_str(&json_stdout).unwrap_or_else(|e| panic!("invalid JSON: {}: {}", e, json_stdout));

    let unseal = json_parsed
        .projects
        .iter()
        .find(|p| p.dir.ends_with("vault/.terragrunt-stack/unseal"))
        .expect("unseal leaf present in json output");
    assert!(
        unseal.watch_files.iter().any(|f| f.contains("modules/vault/unseal/shamir")),
        "unseal watch_files must reference the shamir-resolved source; got: {:?}",
        unseal.watch_files
    );
}
