//! Integration test for the MSSQL backend.
//!
//! Boots its own SQL Server container via `docker run` when
//! `DATABASE_URL_MSSQL` is unset, exercises the introspector against a
//! seeded schema, and tears the container down on exit. The container is
//! best-effort cleaned up even on panic via a guard.
//!
//! ## Container choice
//!
//! Picks the image via `SCHEMADEX_MSSQL_IMAGE` (default
//! `mcr.microsoft.com/azure-sql-edge:latest`). The Azure SQL Edge image is
//! the only first-party Microsoft SQL Server engine that runs on ARM64 hosts
//! (Apple Silicon under colima); the full `mssql/server` image is x86-only
//! and crashes in QEMU emulation. On x86 hosts you can override the env var
//! to use `mcr.microsoft.com/mssql/server:2022-latest`.
//!
//! Skipping is intentionally narrow: if `SCHEMADEX_SKIP_MSSQL_DOCKER` is set
//! and `DATABASE_URL_MSSQL` is empty, the test eprintln-skips. Otherwise we
//! either use the supplied URL or stand up our own container.

#![cfg(feature = "mssql")]

use schemadex_core::backends::mssql::MssqlIntrospector;
use schemadex_core::introspector::SchemaIntrospector;
use schemadex_core::QueryRunner;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CONTAINER_NAME: &str = "schemadex-mssql-it";
const TEST_PASSWORD: &str = "SchemaDexTest!1";
const TEST_PORT: u16 = 11433;
const DEFAULT_IMAGE: &str = "mcr.microsoft.com/azure-sql-edge:latest";

/// RAII guard that runs `docker rm -f` when dropped. Stored as `Some(name)`
/// when we created the container, `None` when we used a caller-provided URL.
struct DockerGuard(Option<String>);

impl Drop for DockerGuard {
    fn drop(&mut self) {
        if let Some(name) = self.0.take() {
            let _ = Command::new("docker")
                .args(["rm", "-f", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Best-effort docker availability check. Returns true when the daemon
/// responds to `docker version --format` within a few seconds.
fn docker_available() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    }

/// Start a SQL Server container with the test password. Returns a guard
/// that removes it on drop. Image defaults to Azure SQL Edge for ARM64
/// compatibility; override via `SCHEMADEX_MSSQL_IMAGE`.
fn start_mssql_container() -> Result<DockerGuard, String> {
    // Best-effort cleanup of any previous run before starting fresh.
    let _ = Command::new("docker")
        .args(["rm", "-f", CONTAINER_NAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let image = std::env::var("SCHEMADEX_MSSQL_IMAGE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_IMAGE.to_string());

    let status = Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            CONTAINER_NAME,
            "-e",
            "ACCEPT_EULA=Y",
            "-e",
            &format!("MSSQL_SA_PASSWORD={TEST_PASSWORD}"),
            "-p",
            &format!("{TEST_PORT}:1433"),
            &image,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("docker run failed to spawn: {e}"))?;
    if !status.status.success() {
        return Err(format!(
            "docker run failed: {}",
            String::from_utf8_lossy(&status.stderr)
        ));
    }
    Ok(DockerGuard(Some(CONTAINER_NAME.to_string())))
}

/// Poll the SA login until it succeeds. SQL Server typically needs 10–25s to
/// finish initial setup on first boot.
async fn wait_for_mssql(url: &str, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    let mut last_err = String::new();
    while started.elapsed() < timeout {
        match MssqlIntrospector::connect(url).await {
            Ok(_) => return Ok(()),
            Err(e) => last_err = e.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(format!("mssql never became reachable: {last_err}"))
}

#[tokio::test]
async fn mssql_smoke() {
    // Env-supplied URL takes precedence so users with a running instance can
    // skip the docker dance entirely.
    let (url, _guard) = match std::env::var("DATABASE_URL_MSSQL") {
        Ok(u) if !u.is_empty() => (u, DockerGuard(None)),
        _ => {
            if std::env::var("SCHEMADEX_SKIP_MSSQL_DOCKER").is_ok() {
                eprintln!(
                    "skipping mssql tests: DATABASE_URL_MSSQL unset and \
                     SCHEMADEX_SKIP_MSSQL_DOCKER set"
                );
                return;
            }
            if !docker_available() {
                eprintln!("skipping mssql tests: docker not available");
                return;
            }
            let guard = match start_mssql_container() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("skipping mssql tests: {e}");
                    return;
                }
            };
            // `master` is always present; we'll use it for the test schema.
            let url = format!(
                "mssql://sa:{TEST_PASSWORD}@localhost:{TEST_PORT}/master"
            );
            (url, guard)
        }
    };

    // Wait for SQL Server to finish initializing. Azure SQL Edge usually
    // boots in ~20s; the full mssql/server image needs more.
    if let Err(e) = wait_for_mssql(&url, Duration::from_secs(120)).await {
        panic!("mssql failed to come up: {e}");
    }

    let introspector = MssqlIntrospector::connect(&url)
        .await
        .expect("connect to mssql");

    // Seed a fresh schema. Drop in dependency order so reruns are clean.
    for stmt in [
        "IF OBJECT_ID('dbo.orders', 'U') IS NOT NULL DROP TABLE dbo.orders",
        "IF OBJECT_ID('dbo.customers', 'U') IS NOT NULL DROP TABLE dbo.customers",
        "CREATE TABLE dbo.customers (id INT NOT NULL PRIMARY KEY, email NVARCHAR(255) NOT NULL)",
        "CREATE TABLE dbo.orders (\
            id INT NOT NULL PRIMARY KEY, \
            customer_id INT NOT NULL, \
            CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES dbo.customers(id))",
    ] {
        introspector
            .run_sql(stmt, 1)
            .await
            .unwrap_or_else(|e| panic!("seed stmt failed: {stmt}\n{e}"));
    }

    // tables() should surface both seeded tables.
    let tables = introspector.tables().await.expect("list tables");
    let names: Vec<&str> = tables.iter().map(|(_, n)| n.as_str()).collect();
    assert!(
        names.contains(&"customers"),
        "expected customers in {:?}",
        names
    );
    assert!(
        names.contains(&"orders"),
        "expected orders in {:?}",
        names
    );

    // columns() should list both columns of `customers` in order.
    let cols = introspector
        .columns(Some("dbo"), "customers")
        .await
        .expect("columns");
    let col_names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(col_names, vec!["id", "email"]);

    // primary_key() should report (`customers`, [id]).
    let pk = introspector
        .primary_key(Some("dbo"), "customers")
        .await
        .expect("pk");
    let pk = pk.expect("customers should have a PK");
    assert_eq!(pk.columns, vec!["id".to_string()]);

    // foreign_keys() on `orders` should find the FK pointing at `customers`.
    let fks = introspector
        .foreign_keys(Some("dbo"), "orders")
        .await
        .expect("fks");
    let fk = fks
        .iter()
        .find(|fk| fk.referenced_table.eq_ignore_ascii_case("customers"))
        .expect("orders.customer_id FK to customers");
    assert_eq!(fk.columns, vec!["customer_id".to_string()]);
    assert_eq!(fk.referenced_columns, vec!["id".to_string()]);

    // run_sql sanity check.
    let result = introspector
        .run_sql("SELECT 1 AS v", 10)
        .await
        .expect("run SELECT 1");
    assert_eq!(result.columns, vec!["v".to_string()]);
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], "1");
}
