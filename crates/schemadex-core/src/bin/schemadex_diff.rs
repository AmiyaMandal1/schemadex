//! `schemadex-diff` — diff two cached [`schemadex_core::model::Database`]
//! snapshots and print a human-readable changelog.
//!
//! Usage:
//!
//! ```text
//! schemadex-diff --from path/to/old.json --to path/to/new.json
//! ```
//!
//! The inputs are the on-disk cache envelopes that schemadex writes under
//! `~/.cache/schemadex/<url_hash>/database.json`. Each file is a JSON object
//! of the form `{"saved_at_unix": <int>, "database": <Database>}`. This
//! binary reads either that envelope or a bare `Database` JSON object — if
//! the top-level object has a `database` field, we pick it out; otherwise we
//! parse the whole document as a `Database` directly.
//!
//! Output is plain markdown bullets covering: tables added/removed, columns
//! added/removed per surviving table, and column type changes. If both
//! snapshots are equivalent, the binary prints `no changes`. It always
//! exits 0 — including on missing files or parse errors, which surface as
//! a diagnostic on stderr.

use schemadex_core::model::{Column, Database, Table};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Deserialize)]
struct CacheEnvelope {
    #[allow(dead_code)]
    saved_at_unix: Option<u64>,
    database: Database,
}

fn parse_args() -> Result<(PathBuf, PathBuf), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut from: Option<PathBuf> = None;
    let mut to: Option<PathBuf> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" => {
                from = Some(PathBuf::from(iter.next().ok_or("--from requires a path")?));
            }
            "--to" => {
                to = Some(PathBuf::from(iter.next().ok_or("--to requires a path")?));
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok((
        from.ok_or("missing --from <path>")?,
        to.ok_or("missing --to <path>")?,
    ))
}

fn print_help() {
    println!("schemadex-diff — diff two cached schemadex Database snapshots");
    println!();
    println!("USAGE:");
    println!("    schemadex-diff --from <OLD.json> --to <NEW.json>");
    println!();
    println!("Each input is expected to be a schemadex cache envelope, e.g.");
    println!("    ~/.cache/schemadex/<url_hash>/database.json");
    println!("A bare Database JSON object is also accepted.");
}

fn load_database(path: &Path) -> Result<Database, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let reader = BufReader::new(file);
    // First parse as a generic Value so we can accept either an envelope
    // (with a "database" field) or a bare Database object.
    let value: serde_json::Value = serde_json::from_reader(reader)
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

fn diff(old: &Database, new: &Database) -> String {
    // Key tables by qualified name for stable, case-insensitive matching.
    let old_tables: BTreeMap<String, &Table> = old
        .tables
        .iter()
        .map(|t| (t.qualified_name().to_lowercase(), t))
        .collect();
    let new_tables: BTreeMap<String, &Table> = new
        .tables
        .iter()
        .map(|t| (t.qualified_name().to_lowercase(), t))
        .collect();

    let mut added_tables: Vec<&Table> = Vec::new();
    let mut removed_tables: Vec<&Table> = Vec::new();
    for (k, t) in &new_tables {
        if !old_tables.contains_key(k) {
            added_tables.push(t);
        }
    }
    for (k, t) in &old_tables {
        if !new_tables.contains_key(k) {
            removed_tables.push(t);
        }
    }

    // Surviving tables: collect (qualified_name, old_table, new_table) for
    // every key present in both maps.
    let mut survivors: Vec<(String, &Table, &Table)> = old_tables
        .iter()
        .filter_map(|(k, ot)| new_tables.get(k).map(|nt| (k.clone(), *ot, *nt)))
        .collect();
    survivors.sort_by(|a, b| a.0.cmp(&b.0));
    added_tables.sort_by_key(|t| t.qualified_name());
    removed_tables.sort_by_key(|t| t.qualified_name());

    let mut out = String::new();
    let mut wrote_anything = false;

    if !added_tables.is_empty() {
        out.push_str("## Tables added\n");
        for t in &added_tables {
            out.push_str(&format!("- + {}\n", t.qualified_name()));
        }
        out.push('\n');
        wrote_anything = true;
    }
    if !removed_tables.is_empty() {
        out.push_str("## Tables removed\n");
        for t in &removed_tables {
            out.push_str(&format!("- - {}\n", t.qualified_name()));
        }
        out.push('\n');
        wrote_anything = true;
    }

    let mut column_section = String::new();
    for (_, ot, nt) in &survivors {
        let table_diff = diff_columns(ot, nt);
        if !table_diff.is_empty() {
            column_section.push_str(&format!("### {}\n", nt.qualified_name()));
            column_section.push_str(&table_diff);
            column_section.push('\n');
        }
    }
    if !column_section.is_empty() {
        out.push_str("## Column changes\n");
        out.push_str(&column_section);
        wrote_anything = true;
    }

    if !wrote_anything {
        out.push_str("no changes\n");
    }

    out
}

fn diff_columns(old: &Table, new: &Table) -> String {
    let old_cols: BTreeMap<String, &Column> = old
        .columns
        .iter()
        .map(|c| (c.name.to_lowercase(), c))
        .collect();
    let new_cols: BTreeMap<String, &Column> = new
        .columns
        .iter()
        .map(|c| (c.name.to_lowercase(), c))
        .collect();

    let mut lines = String::new();
    for (k, c) in &new_cols {
        if !old_cols.contains_key(k) {
            lines.push_str(&format!(
                "- + {}.{}: {}\n",
                new.qualified_name(),
                c.name,
                c.native_type
            ));
        }
    }
    for (k, c) in &old_cols {
        if !new_cols.contains_key(k) {
            lines.push_str(&format!(
                "- - {}.{}: {}\n",
                old.qualified_name(),
                c.name,
                c.native_type
            ));
        }
    }
    for (k, oc) in &old_cols {
        if let Some(nc) = new_cols.get(k) {
            if oc.native_type != nc.native_type {
                lines.push_str(&format!(
                    "- ~ {}.{}: {} -> {}\n",
                    new.qualified_name(),
                    nc.name,
                    oc.native_type,
                    nc.native_type
                ));
            }
        }
    }
    lines
}

fn main() -> ExitCode {
    let (from, to) = match parse_args() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("schemadex-diff: {e}");
            eprintln!("run with --help for usage");
            return ExitCode::SUCCESS;
        }
    };
    let old = match load_database(&from) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("schemadex-diff: {e}");
            return ExitCode::SUCCESS;
        }
    };
    let new = match load_database(&to) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("schemadex-diff: {e}");
            return ExitCode::SUCCESS;
        }
    };
    let out = diff(&old, &new);
    print!("{}", out);
    ExitCode::SUCCESS
}
