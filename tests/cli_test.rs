//! Integration tests for the CLI

use std::process::Command;

fn cargo_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_terragrunt-dag"))
}

#[test]
fn test_cli_help() {
    let output = cargo_bin().arg("--help").output().expect("Failed to execute");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("terragrunt-dag"));
    assert!(stdout.contains("--format"));
}

#[test]
fn test_cli_version() {
    let output = cargo_bin().arg("--version").output().expect("Failed to execute");

    assert!(output.status.success());
}

#[test]
fn test_cli_nonexistent_dir() {
    let output = cargo_bin().arg("/nonexistent/path").output().expect("Failed to execute");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not exist"));
}

#[test]
fn test_cli_json_output() {
    let fixture_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/processor/with_read_terragrunt_config");

    let output = cargo_bin().args(["--format", "json", fixture_dir]).output().expect("Failed to execute");

    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Output should be valid JSON");

    assert!(parsed["projects"].is_array());
}

#[test]
fn test_cli_atlantis_output() {
    let fixture_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/processor/with_read_terragrunt_config");

    let output = cargo_bin().args(["--format", "atlantis", fixture_dir]).output().expect("Failed to execute");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("version: 3"));
    assert!(stdout.contains("execution_order_group"));
}

#[test]
fn test_cli_digger_output() {
    let fixture_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/processor/with_read_terragrunt_config");

    let output = cargo_bin().args(["--format", "digger", fixture_dir]).output().expect("Failed to execute");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("terragrunt: true"));
    assert!(stdout.contains("layer:"));
}

#[test]
fn test_cli_verbose_output() {
    let fixture_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/processor/with_read_terragrunt_config");

    let output = cargo_bin().args(["--verbose", "--format", "json", fixture_dir]).output().expect("Failed to execute");

    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Scanning directory"));
    assert!(stderr.contains("Found"));
    assert!(stderr.contains("Cache stats"));
}

#[test]
fn test_cli_empty_directory() {
    let fixture_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/parser/no_projects");

    // Create empty dir if it doesn't exist
    std::fs::create_dir_all(fixture_dir).ok();

    let output = cargo_bin().args(["--format", "json", fixture_dir]).output().expect("Failed to execute");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(parsed["projects"].as_array().unwrap().is_empty());
}
