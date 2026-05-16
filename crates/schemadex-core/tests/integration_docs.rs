//! Integration test for the `schemadex-docs` binary. Seeds a SQLite db
//! with two related tables, runs the binary with `--url sqlite://...
//! --output tmp.md`, and asserts the generated markdown contains the
//! expected per-table section, the mermaid block, and the FK edge.

#![cfg(feature = "sqlite")]

use std::process::Command;

use sqlx::sqlite::SqlitePool;

async fn seed(pool: &SqlitePool) {
    sqlx::query(
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customers(id))",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn renders_markdown_with_mermaid_and_fk_edge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("demo.sqlite");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());

    let pool = SqlitePool::connect(&url).await.unwrap();
    seed(&pool).await;
    pool.close().await;

    let out_path = tmp.path().join("schema.md");

    let output = Command::new(env!("CARGO_BIN_EXE_schemadex-docs"))
        .arg("--url")
        .arg(&url)
        .arg("--output")
        .arg(&out_path)
        .output()
        .expect("run schemadex-docs");

    assert!(
        output.status.success(),
        "binary exited with {:?}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let body = std::fs::read_to_string(&out_path).expect("read output md");

    assert!(
        body.contains("## main.customers"),
        "expected '## main.customers' heading in output, got:\n{body}"
    );
    assert!(
        body.contains("## main.orders"),
        "expected '## main.orders' heading in output, got:\n{body}"
    );
    assert!(
        body.contains("mermaid"),
        "expected mermaid fence in output, got:\n{body}"
    );
    assert!(
        body.contains("erDiagram"),
        "expected 'erDiagram' in output, got:\n{body}"
    );
    assert!(
        body.contains("customers ||"),
        "expected an FK edge starting with 'customers ||' in output, got:\n{body}"
    );
}
