//! `schemadex-diff` — diff two cached [`schemadex_core::model::Database`]
//! snapshots and print a human-readable changelog.
//!
//! Usage:
//!
//! ```text
//! schemadex-diff --from path/to/old.json --to path/to/new.json
//! schemadex-diff --cache-dir <DIR> --snapshot N      # compare current vs Nth-most-recent
//! ```
//!
//! The inputs are the on-disk cache envelopes that schemadex writes under
//! `~/.cache/schemadex/<url_hash>/database.json`. Each file is a JSON object
//! of the form `{"saved_at_unix": <int>, "database": <Database>}`. This
//! binary reads either that envelope or a bare `Database` JSON object — if
//! the top-level object has a `database` field, we pick it out; otherwise we
//! parse the whole document as a `Database` directly.
//!
//! When `--snapshot N` is supplied with `--cache-dir`, the current
//! `database.json.zst` is compared against the Nth-most-recent file in
//! `<cache-dir>/history/`. Both files may be zstd-compressed (`.json.zst`)
//! or plain JSON; the loader detects the extension.
//!
//! Output is plain markdown bullets covering: tables added/removed, columns
//! added/removed per surviving table, and column type changes. If both
//! snapshots are equivalent, the binary prints `no changes`. It always
//! exits 0 — including on missing files or parse errors, which surface as
//! a diagnostic on stderr.

use schemadex_core::model::{Column, Database, Table};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Deserialize)]
struct CacheEnvelope {
    #[allow(dead_code)]
    saved_at_unix: Option<u64>,
    database: Database,
}

enum DiffArgs {
    /// Explicit paths supplied via `--from` / `--to`.
    Pair(PathBuf, PathBuf),
    /// `--cache-dir DIR --snapshot N` — resolve the Nth-most-recent history
    /// file inside `DIR/history/` and compare it against `DIR/database.json.zst`.
    Snapshot { cache_dir: PathBuf, n: usize },
}

fn parse_args() -> Result<DiffArgs, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut from: Option<PathBuf> = None;
    let mut to: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;
    let mut snapshot: Option<usize> = None;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--from" => {
                from = Some(PathBuf::from(iter.next().ok_or("--from requires a path")?));
            }
            "--to" => {
                to = Some(PathBuf::from(iter.next().ok_or("--to requires a path")?));
            }
            "--cache-dir" => {
                cache_dir = Some(PathBuf::from(
                    iter.next().ok_or("--cache-dir requires a path")?,
                ));
            }
            "--snapshot" => {
                let n = iter.next().ok_or("--snapshot requires an integer")?;
                snapshot = Some(
                    n.parse::<usize>()
                        .map_err(|e| format!("--snapshot expects an integer: {e}"))?,
                );
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if let (Some(dir), Some(n)) = (cache_dir.clone(), snapshot) {
        return Ok(DiffArgs::Snapshot { cache_dir: dir, n });
    }
    if from.is_some() && to.is_some() {
        return Ok(DiffArgs::Pair(from.unwrap(), to.unwrap()));
    }
    Err("expected --from/--to OR --cache-dir/--snapshot".to_string())
}

fn print_help() {
    println!("schemadex-diff — diff two cached schemadex Database snapshots");
    println!();
    println!("USAGE:");
    println!("    schemadex-diff --from <OLD.json> --to <NEW.json>");
    println!("    schemadex-diff --cache-dir <DIR> --snapshot <N>");
    println!();
    println!("With --from/--to, both inputs are schemadex cache envelopes (or");
    println!("bare Database JSON objects).");
    println!();
    println!("With --cache-dir/--snapshot, the current database.json.zst is");
    println!("compared against the Nth-most-recent history file in DIR/history/.");
    println!("N=1 is the most recent snapshot, N=2 the one before, etc.");
}

fn load_database(path: &Path) -> Result<Database, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let value: serde_json::Value = if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s == "zst")
        .unwrap_or(false)
    {
        let decoded = zstd::decode_all(&bytes[..])
            .map_err(|e| format!("zstd decode {}: {e}", path.display()))?;
        serde_json::from_slice(&decoded)
            .map_err(|e| format!("parse {}: {e}", path.display()))?
    } else {
        let reader = BufReader::new(std::io::Cursor::new(bytes));
        serde_json::from_reader(reader).map_err(|e| format!("parse {}: {e}", path.display()))?
    };
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

fn list_history(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries: Vec<(u64, PathBuf)> = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for e in rd {
        let e = e.map_err(|e| format!("read_dir entry: {e}"))?;
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let stem = stem.trim_end_matches(".json");
        let ts_part = stem.split('-').next().unwrap_or(stem);
        let Ok(ts) = ts_part.parse::<u64>() else {
            continue;
        };
        entries.push((ts, path));
    }
    // Sort descending — newest first, so index 0 is "most recent snapshot".
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, p)| p).collect())
}

fn resolve_snapshot(cache_dir: &Path, n: usize) -> Result<(PathBuf, PathBuf), String> {
    if n == 0 {
        return Err("--snapshot N requires N >= 1".to_string());
    }
    let current = cache_dir.join("database.json.zst");
    if !current.exists() {
        return Err(format!(
            "current cache file not found at {}",
            current.display()
        ));
    }
    let history = cache_dir.join("history");
    let snapshots = list_history(&history)?;
    let idx = n - 1;
    if idx >= snapshots.len() {
        return Err(format!(
            "only {} snapshots in {}, but --snapshot {} requested",
            snapshots.len(),
            history.display(),
            n
        ));
    }
    Ok((snapshots[idx].clone(), current))
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
    let parsed = match parse_args() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("schemadex-diff: {e}");
            eprintln!("run with --help for usage");
            return ExitCode::SUCCESS;
        }
    };
    let (from, to) = match parsed {
        DiffArgs::Pair(f, t) => (f, t),
        DiffArgs::Snapshot { cache_dir, n } => match resolve_snapshot(&cache_dir, n) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("schemadex-diff: {e}");
                return ExitCode::SUCCESS;
            }
        },
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
