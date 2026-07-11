//! CLI-surface integration tests: the binary is an MCP stdio server, so the
//! only accepted invocations are the bare launch plus `--help`/`--version`.
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
        stdout.contains("STEMMA_MCP_DOC_TTL_SECS") && stdout.contains("STEMMA_MCP_MAX_DOC_BYTES"),
        "usage documents the lifecycle env vars: {stdout}"
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
