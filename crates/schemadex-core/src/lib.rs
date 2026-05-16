//! schemadex-core: schema introspection and resolution toolkit for SQL agents.
//!
//! See the workspace README for the full pitch. This crate exposes:
//! - [`SchemaIntrospector`] trait for live database introspection
//! - Backend implementations behind feature flags (`postgres`, `sqlite`, `duckdb_backend`)
//! - [`SchemaCache`] for on-disk caching with DDL fingerprinting
//! - Fuzzy column resolution and agent-facing describe API

pub mod agent;
pub mod cache;
pub mod error;
pub mod fingerprint;
pub mod introspector;
pub mod model;
pub mod resolve;
pub mod sampling;

pub mod backends;

pub use crate::agent::{describe_for_agent, DescribeOptions};
pub use crate::cache::SchemaCache;
pub use crate::error::{Result, SchemadexError};
pub use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
pub use crate::model::{
    Column, ColumnSample, DataType, Database, ForeignKey, PrimaryKey, SampleStats, Table,
};
pub use crate::resolve::{resolve_column, ResolveResult};

/// Render a markdown-style table from a [`QueryResult`] and trim it to fit
/// `token_budget`. Returns the rendered string and the final token count.
///
/// Rows are dropped from the bottom until the table fits. If anything was
/// dropped (or the runner already flagged `truncated`), a trailing
/// `_(truncated to N rows)_` marker is appended so the agent knows it didn't
/// see everything.
pub fn render_table_for_agent(
    result: &QueryResult,
    token_budget: usize,
) -> Result<(String, usize)> {
    let tokenizer = tiktoken_rs::cl100k_base()
        .map_err(|e| SchemadexError::Other(format!("tiktoken init failed: {e}")))?;

    // Empty result set: emit just the header (or a one-line "no rows" marker
    // if there isn't even a header) and call it done.
    if result.columns.is_empty() {
        let text = "_(no rows)_\n".to_string();
        let tokens = tokenizer.encode_with_special_tokens(&text).len();
        if tokens > token_budget {
            return Err(SchemadexError::TokenBudget {
                needed: tokens,
                budget: token_budget,
            });
        }
        return Ok((text, tokens));
    }

    let header = render_header(&result.columns);
    let dashes = render_dashes(result.columns.len());

    // Render full body once, then peel rows off the bottom until it fits.
    let mut body_rows: Vec<String> = result
        .rows
        .iter()
        .map(|row| render_body_row(&result.columns, row))
        .collect();
    let mut dropped = 0usize;

    loop {
        let total = result.rows.len() - dropped;
        let truncated = result.truncated || dropped > 0;
        let rendered = assemble_table(&header, &dashes, &body_rows, truncated, total);
        let tokens = tokenizer.encode_with_special_tokens(&rendered).len();
        if tokens <= token_budget {
            return Ok((rendered, tokens));
        }
        if body_rows.pop().is_none() {
            // Even the header + truncation marker don't fit. Surface the
            // budget error rather than silently returning an empty string.
            return Err(SchemadexError::TokenBudget {
                needed: tokens,
                budget: token_budget,
            });
        }
        dropped += 1;
    }
}

fn render_header(columns: &[String]) -> String {
    let mut s = String::from("| ");
    s.push_str(&columns.join(" | "));
    s.push_str(" |\n");
    s
}

fn render_dashes(n: usize) -> String {
    let mut s = String::from("|");
    for _ in 0..n {
        s.push_str(" --- |");
    }
    s.push('\n');
    s
}

fn render_body_row(columns: &[String], row: &[String]) -> String {
    let mut s = String::from("| ");
    for i in 0..columns.len() {
        let cell = row.get(i).map(String::as_str).unwrap_or("");
        // Escape pipe characters so the markdown stays valid.
        let escaped = cell.replace('|', "\\|").replace('\n', " ");
        s.push_str(&escaped);
        if i + 1 < columns.len() {
            s.push_str(" | ");
        } else {
            s.push_str(" |\n");
        }
    }
    s
}

fn assemble_table(
    header: &str,
    dashes: &str,
    body_rows: &[String],
    truncated: bool,
    kept: usize,
) -> String {
    let mut out = String::with_capacity(header.len() + dashes.len() + body_rows.len() * 16);
    out.push_str(header);
    out.push_str(dashes);
    for row in body_rows {
        out.push_str(row);
    }
    if truncated {
        out.push_str(&format!("_(truncated to {} rows)_\n", kept));
    }
    out
}

impl SchemaCache {
    /// Convenience wrapper: run `sql` through `runner` and return a
    /// markdown-rendered result that fits inside `token_budget`. The internal
    /// row limit is a fixed heuristic (200) — `render_table_for_agent` then
    /// trims further if the response still doesn't fit.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.run_sql",
        skip(self, runner, sql),
        fields(
            sql_len = sql.len(),
            token_budget,
        ),
    )]
    pub async fn run_sql<R: QueryRunner + ?Sized>(
        &self,
        runner: &R,
        sql: &str,
        token_budget: usize,
    ) -> Result<(String, usize)> {
        let result = runner.run_sql(sql, 200).await?;
        tracing::debug!(
            rows = result.rows.len(),
            cols = result.columns.len(),
            truncated = result.truncated,
            "schema_cache.run_sql.fetched"
        );
        render_table_for_agent(&result, token_budget)
    }
}
