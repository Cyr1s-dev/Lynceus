//! `knowledge-import` command-line regression tests.

use std::process::Command;

#[test]
fn dry_run_does_not_create_or_initialize_target_database() {
    let root = tempfile::TempDir::with_prefix("knowledge_import_cli_")
        .unwrap_or_else(|error| panic!("temporary directory must be created: {error}"));
    let knowledge_dir = root.path().join("knowledge");
    std::fs::create_dir(&knowledge_dir)
        .unwrap_or_else(|error| panic!("fixture directory must be created: {error}"));
    let target = root.path().join("must-not-exist.sqlite3");

    let output = Command::new(env!("CARGO_BIN_EXE_knowledge-import"))
        .args(["--db"])
        .arg(&target)
        .args(["--dir"])
        .arg(&knowledge_dir)
        .arg("--dry-run")
        .output()
        .unwrap_or_else(|error| panic!("knowledge-import must run: {error}"));

    assert!(
        output.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !target.exists(),
        "dry-run must not touch the target database"
    );
}
