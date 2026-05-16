//! `schemadex-docs` — render a [`schemadex_core::model::Database`] snapshot
//! as a single human-readable markdown document.
//!
//! The output contains:
//! - a header with the database's url hash and table count
//! - a generated-at ISO-8601 timestamp
//! - a table-of-contents linking to per-table sections
//! - a mermaid `erDiagram` of every foreign-key edge
//! - one section per table with the table comment, a columns/types/nullable/
//!   defaults table, sample top-K values or sentinel flags, the primary key,
//!   and a foreign-key list
//!
//! Usage:
//!
//! ```text
//! schemadex-docs --cache path/to/database.json.zst --output schema.md
//! schemadex-docs --url postgres://... --output schema.md
//! ```
//!
//! With `--url`, the cache is rebuilt on the fly via
//! [`schemadex_core::backends::connect_with_sampling`] and persisted to a
//! temp directory so the binary doesn't pollute the user's `~/.cache`.
//!
//! With `--cache`, the file is read directly: both the zstd-compressed
//! `database.json.zst` envelope and a bare uncompressed `database.json`
//! (legacy or hand-crafted) are accepted.

use schemadex_core::model::{Column, Database, ForeignKey, Table};
use serde::Deserialize;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Deserialize)]
struct CacheEnvelope {
    #[allow(dead_code)]
    saved_at_unix: Option<u64>,
    database: Database,
}

enum Input {
    Cache(PathBuf),
    Url(String),
}

struct Args {
    input: Input,
    output: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut cache: Option<PathBuf> = None;
    let mut url: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut iter = raw.into_iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--cache" => {
                cache = Some(PathBuf::from(
                    iter.next().ok_or("--cache requires a path")?,
                ));
            }
            "--url" => {
                url = Some(iter.next().ok_or("--url requires a connection string")?);
            }
            "--output" => {
                output = Some(PathBuf::from(
                    iter.next().ok_or("--output requires a path")?,
                ));
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let output = output.ok_or("missing --output <path>")?;
    let input = match (cache, url) {
        (Some(p), None) => Input::Cache(p),
        (None, Some(u)) => Input::Url(u),
        (Some(_), Some(_)) => return Err("pass either --cache or --url, not both".into()),
        (None, None) => return Err("missing input: pass either --cache or --url".into()),
    };
    Ok(Args { input, output })
}

fn print_help() {
    println!("schemadex-docs — render a SchemaCache as a markdown schema document");
    println!();
    println!("USAGE:");
    println!("    schemadex-docs --cache <PATH> --output <MD>");
    println!("    schemadex-docs --url <URL>    --output <MD>");
    println!();
    println!("The cache input accepts both `.json` and `.json.zst` envelopes.");
}

/// Read a cache file, accepting either the zstd-compressed envelope shape
/// (`database.json.zst`) or a plain JSON envelope/database object.
fn load_cache(path: &Path) -> Result<Database, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {}: {e}", path.display()))?;

    // Try zstd decode first. If the magic doesn't match we fall through and
    // attempt plain JSON — this keeps the binary friendly to hand-written
    // fixtures the way `schemadex-diff` already is.
    let decoded: Vec<u8> = match zstd::decode_all(&bytes[..]) {
        Ok(d) => d,
        Err(_) => bytes,
    };

    let value: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if value.get("database").is_some() {
        let env: CacheEnvelope = serde_json::from_value(value)
            .map_err(|e| format!("envelope decode {}: {e}", path.display()))?;
        Ok(env.database)
    } else {
        let db: Database = serde_json::from_value(value)
            .map_err(|e| format!("database decode {}: {e}", path.display()))?;
        Ok(db)
    }
}

async fn load_from_url(url: &str) -> Result<Database, String> {
    // Persist the rebuilt cache to a temp dir so we don't write into the
    // user's `~/.cache/schemadex`. The temp dir is dropped on return.
    let tmp = tempfile::tempdir()
        .map_err(|e| format!("temp dir: {e}"))?;
    let opts = schemadex_core::cache::CacheOptions {
        ttl: std::time::Duration::from_secs(24 * 3600),
        cache_dir: Some(tmp.path().to_path_buf()),
        parallel: true,
        ..Default::default()
    };
    let introspector = schemadex_core::backends::connect_with_sampling(
        url,
        Some(schemadex_core::sampling::SamplingPolicy::default_policy()),
    )
    .await
    .map_err(|e| format!("connect {url}: {e}"))?;
    let cache = schemadex_core::SchemaCache::from_introspector(&*introspector, url, &opts)
        .await
        .map_err(|e| format!("introspect: {e}"))?;
    Ok(cache.into_database())
}

/// Render the full document.
fn render(db: &Database) -> String {
    let mut tables: Vec<&Table> = db.tables.iter().collect();
    tables.sort_by(|a, b| display_name(a, db).cmp(&display_name(b, db)));

    let mut out = String::new();

    out.push_str(&format!(
        "# Schema: {} ({} tables)\n\n",
        db.url_hash,
        db.tables.len()
    ));
    out.push_str(&format!("Generated: {}\n\n", iso_now()));

    out.push_str("## Tables of contents\n");
    for t in &tables {
        let name = display_name(t, db);
        out.push_str(&format!("- [{}](#{})\n", name, anchor(&name)));
    }
    out.push('\n');

    out.push_str("## ER diagram\n\n");
    out.push_str("```mermaid\n");
    out.push_str("erDiagram\n");
    let mut wrote_edge = false;
    for t in &tables {
        for fk in &t.foreign_keys {
            out.push_str(&render_edge(t, fk));
            wrote_edge = true;
        }
    }
    if !wrote_edge {
        // Mermaid renders an empty `erDiagram` fine, but a comment makes it
        // obvious to a human reader that we didn't forget to populate it.
        out.push_str("    %% no foreign-key relationships\n");
    }
    out.push_str("```\n\n");

    for t in &tables {
        render_table(&mut out, t, db);
    }

    out
}

/// Render a single mermaid `erDiagram` edge. Labels with `from_cols ->
/// ref_cols` so a reader can see exactly which columns join.
///
/// We render edges parent-first (`parent ||--o{ child : "id -> child_id"`)
/// because the customary mermaid one-to-many notation reads naturally that
/// way: one parent, many children.
fn render_edge(t: &Table, fk: &ForeignKey) -> String {
    let child = mermaid_ident(&t.name);
    let parent = mermaid_ident(&fk.referenced_table);
    let child_cols = fk.columns.join(",");
    let parent_cols = fk.referenced_columns.join(",");
    format!(
        "    {parent} ||--o{{ {child} : \"{parent_cols} -> {child_cols}\"\n"
    )
}

fn render_table(out: &mut String, t: &Table, db: &Database) {
    let name = display_name(t, db);
    out.push_str(&format!("## {}\n", name));
    if let Some(c) = &t.comment {
        if !c.trim().is_empty() {
            out.push_str(&format!("> {}\n\n", c.trim()));
        } else {
            out.push('\n');
        }
    } else {
        out.push('\n');
    }

    let mut cols: Vec<&Column> = t.columns.iter().collect();
    cols.sort_by_key(|c| c.ordinal);

    out.push_str("| Column | Type | Null | Default | Sample / sentinel |\n");
    out.push_str("|--------|------|------|---------|-------------------|\n");
    for c in &cols {
        let null = if c.nullable { "nullable" } else { "NOT NULL" };
        let default = c.default.as_deref().unwrap_or("").replace('|', "\\|");
        let sample = render_sample(c);
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            escape_cell(&c.name),
            escape_cell(&c.native_type),
            null,
            default,
            sample,
        ));
    }
    out.push('\n');

    match &t.primary_key {
        Some(pk) if !pk.columns.is_empty() => {
            out.push_str(&format!(
                "**Primary key:** ({})\n",
                pk.columns.join(", ")
            ));
        }
        _ => {
            out.push_str("**Primary key:** none\n");
        }
    }

    if t.foreign_keys.is_empty() {
        out.push_str("**Foreign keys:** none\n\n");
    } else {
        out.push_str("**Foreign keys:**\n");
        for fk in &t.foreign_keys {
            out.push_str(&format!(
                "- ({}) -> {}({})\n",
                fk.columns.join(", "),
                fk.referenced_table,
                fk.referenced_columns.join(", "),
            ));
        }
        out.push('\n');
    }
}

fn render_sample(c: &Column) -> String {
    let Some(sample) = &c.sample else {
        return String::new();
    };
    if let Some((value, frac)) = &sample.sentinel {
        let pct = (*frac * 100.0).round() as i64;
        return format!(
            "**sentinel: {} {}%**",
            escape_cell(&format!("{value:?}")),
            pct
        );
    }
    if sample.top_values.is_empty() {
        return String::new();
    }
    sample
        .top_values
        .iter()
        .take(3)
        .map(|(v, f)| {
            let pct = (*f * 100.0).round() as i64;
            format!("{} ({pct}%)", escape_cell(&format!("{v:?}")))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Display the table name as `schema.name`. Backends like SQLite leave
/// `schema` as `None` even though there's a conceptual default schema
/// (`main`); for those we fall back to a per-backend default so the
/// document's headings and anchors are still qualified.
fn display_name(t: &Table, db: &Database) -> String {
    if let Some(schema) = &t.schema {
        return format!("{schema}.{}", t.name);
    }
    match db.backend.as_str() {
        "sqlite" => format!("main.{}", t.name),
        "mysql" | "mariadb" => t.name.clone(),
        "duckdb" => format!("main.{}", t.name),
        _ => t.name.clone(),
    }
}

/// Lowercase the qualified name and strip non-alnum chars — matches the
/// auto-generated GitHub-flavoured-markdown anchor.
fn anchor(qualified: &str) -> String {
    qualified
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c == '-' || c == '_' {
                Some(c)
            } else {
                None
            }
        })
        .collect()
}

/// Make a string safe for use as a mermaid identifier. Mermaid identifiers
/// may contain letters, digits, and underscores; everything else gets
/// replaced.
fn mermaid_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Minimal ISO-8601 UTC timestamp from the current `SystemTime`. Avoids
/// pulling in `chrono` just for the header line.
fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso_utc(secs as i64)
}

/// Convert a unix timestamp (seconds since epoch, UTC) into an ISO-8601
/// string like `2024-05-16T17:42:09Z`. Uses Howard Hinnant's
/// days_from_civil algorithm so we don't need chrono.
fn format_iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // civil_from_days, adapted to i64. Day 0 == 1970-01-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, m, d, hour, minute, second
    )
}

fn write_output(path: &Path, body: &str) -> Result<(), String> {
    let mut f = File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(body.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let db = match args.input {
        Input::Cache(p) => load_cache(&p)?,
        Input::Url(u) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            rt.block_on(load_from_url(&u))?
        }
    };
    let body = render(&db);
    write_output(&args.output, &body)?;
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("schemadex-docs: {e}");
            eprintln!("run with --help for usage");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_known_value() {
        // 2024-05-16T17:42:09Z == 1715881329 unix seconds.
        assert_eq!(format_iso_utc(1_715_881_329), "2024-05-16T17:42:09Z");
        // Epoch.
        assert_eq!(format_iso_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn anchor_strips_punctuation() {
        assert_eq!(anchor("public.customers"), "publiccustomers");
        assert_eq!(anchor("main.orders"), "mainorders");
    }

    #[test]
    fn mermaid_ident_replaces_dots() {
        assert_eq!(mermaid_ident("public.customers"), "public_customers");
    }
}
