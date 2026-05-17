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
pub mod examples;
pub mod federation;
pub mod fingerprint;
pub mod hint;
pub mod introspector;
pub mod memo;
pub mod model;
pub mod overlap;
pub mod pii;
pub mod resolve;
pub mod safety;
pub mod sampling;
pub mod synonyms;
pub mod validate;

pub mod backends;

#[cfg(feature = "otel")]
pub mod otel;
#[cfg(feature = "otel")]
pub use crate::otel::init_otel;

pub use crate::agent::{describe_for_agent, DescribeOptions};
pub use crate::cache::{EmbeddingIndex, SchemaCache, INVALIDATED_DDL_HASH};
pub use crate::federation::Federation;
pub use crate::overlap::{find_overlaps, OverlapHint};
pub use crate::pii::{classify_column, PiiKind};
pub use crate::memo::{CachedResult, ResultCache};
pub use crate::error::{Result, SchemadexError};
pub use crate::examples::{generate_examples, generate_examples_for_database};
pub use crate::hint::{hint_for_error, ErrorHint, HintKind};
pub use crate::introspector::{Backend, QueryResult, QueryRunner, SchemaIntrospector};
pub use crate::model::{
    Column, ColumnSample, DataType, Database, ForeignKey, PrimaryKey, SampleStats, Table,
};
pub use crate::resolve::{resolve_column, ResolveResult};
pub use crate::safety::assert_readonly;

/// Pre-execute cost estimate for a SQL statement against a particular
/// backend. Returned by [`QueryRunner::preview_cost`].
///
/// Backends that don't have a meaningful cost-estimation API leave the
/// numeric fields as `None` and surface a brief `warning`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CostEstimate {
    pub bytes_processed: Option<u64>,
    pub rows_estimate: Option<u64>,
    pub warning: Option<String>,
}
pub use crate::synonyms::{resolve_column_with_synonyms, SynonymEntry, SynonymMap};
pub use crate::validate::{validate_sql, IssueKind, ValidationIssue};

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
        safety::assert_readonly(sql)?;
        self.run_sql_unchecked(runner, sql, token_budget).await
    }

    /// Same as [`SchemaCache::run_sql`] but skips the read-only safety check.
    /// Callers must validate the SQL themselves before calling this — it
    /// will happily forward INSERT/UPDATE/DELETE/DROP statements to the
    /// runner. Used by the Python `allow_write=True` escape hatch.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.run_sql_unchecked",
        skip(self, runner, sql),
        fields(
            sql_len = sql.len(),
            token_budget,
        ),
    )]
    pub async fn run_sql_unchecked<R: QueryRunner + ?Sized>(
        &self,
        runner: &R,
        sql: &str,
        token_budget: usize,
    ) -> Result<(String, usize)> {
        // Item 3: optional LRU memoization, keyed by (fingerprint, sql).
        // Only when the cache was constructed with `memoize_results = true`
        // and we have a fingerprint to key against. `token_budget` is *not*
        // part of the key on purpose — a hit returns the cached render and
        // its precomputed token count; callers who need different budgets
        // can re-run uncached.
        let memo_key = self
            .memo()
            .as_ref()
            .and_then(|_| self.database().fingerprint.clone());
        if let (Some(memo), Some(fp)) = (self.memo(), memo_key.as_ref()) {
            if let Some(hit) = memo.get(fp, sql) {
                tracing::debug!(sql_len = sql.len(), "schema_cache.run_sql.memo_hit");
                return Ok((hit.rendered, hit.tokens));
            }
        }

        let result = runner.run_sql(sql, 200).await?;
        tracing::debug!(
            rows = result.rows.len(),
            cols = result.columns.len(),
            truncated = result.truncated,
            "schema_cache.run_sql.fetched"
        );
        let (rendered, tokens) = render_table_for_agent(&result, token_budget)?;

        if let (Some(memo), Some(fp)) = (self.memo(), memo_key.as_ref()) {
            memo.put(
                fp,
                sql,
                crate::memo::CachedResult {
                    rendered: rendered.clone(),
                    tokens,
                },
            );
        }
        Ok((rendered, tokens))
    }

    /// Streaming variant of [`SchemaCache::run_sql`]. The runner consumes
    /// rows one-at-a-time and stops as soon as the estimated token cost
    /// would exceed `token_budget`, so huge result sets never materialise
    /// fully in memory. The final markdown is still re-counted by
    /// [`render_table_for_agent`] for accuracy.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.run_sql_streaming",
        skip(self, runner, sql),
        fields(
            sql_len = sql.len(),
            token_budget,
        ),
    )]
    pub async fn run_sql_streaming<R: QueryRunner + ?Sized>(
        &self,
        runner: &R,
        sql: &str,
        token_budget: usize,
    ) -> Result<(String, usize)> {
        safety::assert_readonly(sql)?;
        let result = runner.run_sql_streaming(sql, token_budget).await?;
        tracing::debug!(
            rows = result.rows.len(),
            cols = result.columns.len(),
            truncated = result.truncated,
            "schema_cache.run_sql_streaming.fetched"
        );
        render_table_for_agent(&result, token_budget)
    }

    /// Like [`SchemaCache::run_sql`] but runs [`validate::validate_sql`]
    /// against the cached schema *before* executing. If the validator finds
    /// any unknown table or column references, returns
    /// [`SchemadexError::Other`] with a structured, agent-readable message
    /// listing every issue and the closest cached identifier.
    ///
    /// This is an *optional* sibling of `run_sql`. The default
    /// [`SchemaCache::run_sql`] retains its current behavior (read-only
    /// guard only) so existing callers don't have to change.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.run_sql_validated",
        skip(self, runner, sql),
        fields(
            sql_len = sql.len(),
            token_budget,
        ),
    )]
    pub async fn run_sql_validated<R: QueryRunner + ?Sized>(
        &self,
        runner: &R,
        sql: &str,
        token_budget: usize,
    ) -> Result<(String, usize)> {
        safety::assert_readonly(sql)?;
        let issues = validate::validate_sql(self.database(), sql);
        if !issues.is_empty() {
            let payload = serde_json::to_string(&issues)
                .unwrap_or_else(|_| "[]".to_string());
            let human = issues
                .iter()
                .map(format_issue_human)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(SchemadexError::Other(format!(
                "schemadex pre-validation flagged {} issue(s): {} (details: {})",
                issues.len(),
                human,
                payload
            )));
        }
        self.run_sql_unchecked(runner, sql, token_budget).await
    }

    /// Pre-execute cost estimate. Delegates to
    /// [`QueryRunner::preview_cost`]; backends that don't support a
    /// dry-run return a `CostEstimate` with `None` numeric fields and a
    /// "not supported" warning.
    #[tracing::instrument(
        level = "info",
        name = "schema_cache.preview_cost",
        skip(self, runner, sql),
        fields(sql_len = sql.len()),
    )]
    pub async fn preview_cost<R: QueryRunner + ?Sized>(
        &self,
        runner: &R,
        sql: &str,
    ) -> Result<CostEstimate> {
        runner.preview_cost(sql).await
    }
}

fn format_issue_human(issue: &validate::ValidationIssue) -> String {
    use validate::IssueKind;
    let suffix = match &issue.suggestion {
        Some(s) => format!(" — did you mean '{s}'?"),
        None => String::new(),
    };
    match &issue.kind {
        IssueKind::UnknownTable => {
            format!("unknown table '{}'{}", issue.identifier, suffix)
        }
        IssueKind::UnknownColumn { table } => {
            format!(
                "unknown column '{}' on table '{}'{}",
                issue.identifier, table, suffix
            )
        }
    }
}
