//! Integration test for the `schemadex-diff` binary. Writes two cache
//! envelopes — one with `users(id, email)`, one with `users(id, email,
//! region)` — invokes the binary, and asserts the changelog reports the
//! added column.

use std::process::Command;

#[test]
fn diff_reports_added_column() {
    let tmp = tempfile::tempdir().expect("tempdir");

    let old_json = serde_json::json!({
        "saved_at_unix": 0,
        "database": {
            "backend": "test",
            "url_hash": "x",
            "tables": [
                {
                    "schema": null,
                    "name": "users",
                    "comment": null,
                    "columns": [
                        {
                            "name": "id",
                            "data_type": "integer",
                            "native_type": "int",
                            "nullable": false,
                            "default": null,
                            "comment": null,
                            "ordinal": 0,
                            "sample": null
                        },
                        {
                            "name": "email",
                            "data_type": "text",
                            "native_type": "text",
                            "nullable": false,
                            "default": null,
                            "comment": null,
                            "ordinal": 1,
                            "sample": null
                        }
                    ],
                    "primary_key": null,
                    "foreign_keys": [],
                    "row_count_estimate": null,
                    "ddl_hash": null
                }
            ],
            "fingerprint": null
        }
    });

    let new_json = serde_json::json!({
        "saved_at_unix": 1,
        "database": {
            "backend": "test",
            "url_hash": "x",
            "tables": [
                {
                    "schema": null,
                    "name": "users",
                    "comment": null,
                    "columns": [
                        {
                            "name": "id",
                            "data_type": "integer",
                            "native_type": "int",
                            "nullable": false,
                            "default": null,
                            "comment": null,
                            "ordinal": 0,
                            "sample": null
                        },
                        {
                            "name": "email",
                            "data_type": "text",
                            "native_type": "text",
                            "nullable": false,
                            "default": null,
                            "comment": null,
                            "ordinal": 1,
                            "sample": null
                        },
                        {
                            "name": "region",
                            "data_type": "text",
                            "native_type": "text",
                            "nullable": true,
                            "default": null,
                            "comment": null,
                            "ordinal": 2,
                            "sample": null
                        }
                    ],
                    "primary_key": null,
                    "foreign_keys": [],
                    "row_count_estimate": null,
                    "ddl_hash": null
                }
            ],
            "fingerprint": null
        }
    });

    let old_path = tmp.path().join("old.json");
    let new_path = tmp.path().join("new.json");
    std::fs::write(&old_path, serde_json::to_vec_pretty(&old_json).unwrap()).unwrap();
    std::fs::write(&new_path, serde_json::to_vec_pretty(&new_json).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_schemadex-diff"))
        .arg("--from")
        .arg(&old_path)
        .arg("--to")
        .arg(&new_path)
        .output()
        .expect("run schemadex-diff");

    assert!(
        output.status.success(),
        "binary exited with {:?}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(
        stdout.contains("+ users.region"),
        "expected '+ users.region' in stdout, got:\n{stdout}"
    );
}
