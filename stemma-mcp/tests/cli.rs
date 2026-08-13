//! CLI-surface integration tests: the binary is an MCP stdio server, so the
//! accepted invocations are the server launch (with an optional workspace
//! root) plus `--help`/`--version`.
//! These spawn the real binary and assert it handles arguments before ever
//! touching the stdio transport (the pre-fix behavior started the server and
//! died with a confusing "connection closed").

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_stemma-mcp"))
}

#[test]
fn help_prints_usage_and_exits_zero() {
    let out = bin().arg("--help").output().expect("spawn --help");
    assert!(out.status.success(), "--help exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("stdio"),
        "usage explains it speaks MCP over stdio: {stdout}"
    );
    assert!(
        stdout.contains("STEMMA_MCP_PROFILE")
            && stdout.contains("STEMMA_MCP_DOC_TTL_SECS")
            && stdout.contains("STEMMA_MCP_MAX_DOC_BYTES")
            && stdout.contains("STEMMA_MCP_MAX_IMAGE_BYTES")
            && stdout.contains("STEMMA_MCP_MAX_IMAGE_TOTAL_BYTES")
            && stdout.contains("STEMMA_MCP_WORKSPACE_ROOT"),
        "usage documents the lifecycle env vars: {stdout}"
    );
    assert!(
        stdout.contains("--workspace-root") && stdout.contains("CLAUDE_PROJECT_DIR"),
        "usage documents both discoverable and Claude Code root selection: {stdout}"
    );
    assert!(
        stdout.contains("README.md"),
        "usage points at the README: {stdout}"
    );
}

#[test]
fn version_prints_and_exits_zero() {
    let out = bin().arg("--version").output().expect("spawn --version");
    assert!(out.status.success(), "--version exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.trim().is_empty(), "prints a version string");
}

#[test]
fn unrecognized_argument_fails_loudly() {
    let out = bin().arg("--nope").output().expect("spawn bad arg");
    assert!(!out.status.success(), "an unrecognized arg exits nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unrecognized argument") && stderr.contains("--nope"),
        "stderr names the offending argument: {stderr}"
    );
    assert!(
        stderr.contains("USAGE"),
        "usage is shown on the error path: {stderr}"
    );
}

#[test]
fn workspace_root_without_a_path_fails_loudly() {
    let out = bin()
        .arg("--workspace-root")
        .output()
        .expect("spawn missing workspace root");
    assert!(!out.status.success(), "missing path exits nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--workspace-root requires a path"),
        "stderr explains the missing value: {stderr}"
    );
}

#[test]
fn invalid_cli_workspace_root_wins_over_environment() {
    let valid_environment_root = tempfile::tempdir().expect("valid environment root");
    let missing_cli_root = valid_environment_root.path().join("missing");
    let out = bin()
        .env("STEMMA_MCP_WORKSPACE_ROOT", valid_environment_root.path())
        .env("CLAUDE_PROJECT_DIR", valid_environment_root.path())
        .arg("--workspace-root")
        .arg(&missing_cli_root)
        .output()
        .expect("spawn with invalid CLI root");
    assert!(
        !out.status.success(),
        "invalid higher-priority root exits nonzero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--workspace-root") && stderr.contains("missing"),
        "CLI root is selected and fails without falling through: {stderr}"
    );
}

#[test]
fn invalid_stemma_environment_root_wins_over_claude_project() {
    let valid_claude_project = tempfile::tempdir().expect("valid Claude project");
    let missing_stemma_root = valid_claude_project.path().join("missing");
    let out = bin()
        .env("STEMMA_MCP_WORKSPACE_ROOT", &missing_stemma_root)
        .env("CLAUDE_PROJECT_DIR", valid_claude_project.path())
        .output()
        .expect("spawn with invalid Stemma environment root");
    assert!(!out.status.success(), "invalid explicit root exits nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("STEMMA_MCP_WORKSPACE_ROOT") && stderr.contains("missing"),
        "explicit environment root fails without falling through: {stderr}"
    );
}

#[test]
fn claude_project_root_is_selected_without_logging_its_path() {
    let claude_project = tempfile::tempdir().expect("Claude project");
    let out = bin()
        .env_remove("STEMMA_MCP_WORKSPACE_ROOT")
        .env("CLAUDE_PROJECT_DIR", claude_project.path())
        .output()
        .expect("spawn with Claude project root");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("workspace_root_source=\"claude_project_dir\""),
        "ordinary startup log identifies the selected source: {stderr}"
    );
    assert!(
        !stderr.contains(&claude_project.path().display().to_string()),
        "ordinary startup log does not expose the absolute root: {stderr}"
    );
}

#[test]
fn startup_directory_fallback_is_observable() {
    let startup = tempfile::tempdir().expect("startup directory");
    let out = bin()
        .current_dir(startup.path())
        .env_remove("STEMMA_MCP_WORKSPACE_ROOT")
        .env_remove("CLAUDE_PROJECT_DIR")
        .output()
        .expect("spawn without a configured root");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("using the canonical server startup directory")
            && stderr.contains("backward compatibility"),
        "compatibility fallback emits a visible warning: {stderr}"
    );
    assert!(
        !stderr.contains(&startup.path().display().to_string()),
        "fallback warning does not expose the absolute root: {stderr}"
    );
}
