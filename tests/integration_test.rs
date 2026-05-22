//! End-to-end integration tests.

use std::process::Command;

use camino::Utf8PathBuf;

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
    let azure_count = parsed.projects.iter().filter(|p| p.dir.ends_with("/azure/bdo")).count();
    let saml_count = parsed.projects.iter().filter(|p| p.dir.ends_with("/saml/crelan")).count();
    assert_eq!(
        azure_count,
        1,
        "azure/bdo emitted {} times, expected 1; dirs: {:?}",
        azure_count,
        parsed.projects.iter().map(|p| &p.dir).collect::<Vec<_>>()
    );
    assert_eq!(saml_count, 1, "saml/crelan emitted {} times, expected 1", saml_count);

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
