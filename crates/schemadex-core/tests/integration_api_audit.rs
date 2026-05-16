//! Smoke test for the schemadex-api-audit binary: confirms the symbol
//! list parses and includes the headline public types.

use std::process::Command;

#[test]
fn api_audit_lists_core_symbols() {
    let exe = env!("CARGO_BIN_EXE_schemadex-api-audit");
    let output = Command::new(exe)
        .output()
        .expect("run schemadex-api-audit");
    assert!(output.status.success(), "binary exited non-zero");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    for needle in ["SchemaCache", "Database", "Table", "describe_for_agent"] {
        assert!(stdout.contains(needle), "missing symbol: {}", needle);
    }
}
