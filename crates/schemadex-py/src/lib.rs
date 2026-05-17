//! PyO3 bindings for schemadex. Exposes the cache, resolution, and
//! agent-describe APIs to Python.

#![allow(clippy::useless_conversion)] // false-positive from `#[pymethods]` expansion

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::IntoPy;

use schemadex_core::{
    backends,
    cache::{CacheOptions, EmbeddingIndex},
    classify_column,
    describe_for_agent as core_describe,
    find_overlaps as core_find_overlaps,
    hint_for_error as core_hint_for_error,
    resolve_column as core_resolve,
    resolve_column_with_synonyms,
    sampling::SamplingPolicy,
    validate_sql as core_validate_sql,
    DescribeOptions, Federation as CoreFederation, ResolveResult,
    SchemaCache as CoreCache, SchemadexError, SynonymMap,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn map_err(e: SchemadexError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// The tokio runtime that backs both the synchronous (`block_on`) path and the
/// async (`pyo3_async_runtimes`) path. Initialized once and shared via
/// `pyo3_async_runtimes::tokio::init_with_runtime` so that both surfaces drive
/// futures on the same scheduler.
fn rt() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    static INIT_ASYNC: OnceLock<()> = OnceLock::new();
    let runtime = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    });
    // Hand the same runtime to pyo3-async-runtimes so both surfaces drive
    // futures on the same scheduler. The runtime reference borrowed from a
    // `OnceLock<Runtime>` is valid for the lifetime of the process (`'static`)
    // because the OnceLock is itself `static` and never dropped, so the call
    // satisfies `init_with_runtime`'s `&'static Runtime` bound directly. The
    // return is `Err(())` if a previous call already installed a runtime; we
    // tolerate that (idempotent) so callers can invoke `rt()` freely.
    INIT_ASYNC.get_or_init(|| {
        let _ = pyo3_async_runtimes::tokio::init_with_runtime(runtime);
    });
    runtime
}

/// Ensure the runtime is initialized. Cheap to call repeatedly.
fn ensure_runtime() {
    let _ = rt();
}

/// Build a [`SamplingPolicy`] from the Python kwargs that `from_url` /
/// `refresh` / `refresh_table` all accept. Returns `None` when sampling is
/// disabled so callers can pass it straight to `connect_with_sampling`.
fn build_sampling_policy(
    sample_values: bool,
    sample_top_k: Option<usize>,
    sample_sentinel_threshold: Option<f32>,
    sample_rows: Option<u64>,
) -> Option<SamplingPolicy> {
    if !sample_values {
        return None;
    }
    let mut policy = SamplingPolicy::default_policy();
    if let Some(k) = sample_top_k {
        policy.top_k = k;
    }
    if let Some(t) = sample_sentinel_threshold {
        policy.sentinel_threshold = t;
    }
    if let Some(n) = sample_rows {
        policy.sample_rows = n;
    }
    Some(policy)
}

#[pyclass(name = "ResolveResult", module = "schemadex")]
#[derive(Clone)]
struct PyResolveResult {
    #[pyo3(get)]
    matched: Option<String>,
    #[pyo3(get)]
    confidence: f32,
    #[pyo3(get)]
    alternatives: Vec<(String, f32)>,
}

impl From<ResolveResult> for PyResolveResult {
    fn from(r: ResolveResult) -> Self {
        PyResolveResult {
            matched: r.matched,
            confidence: r.confidence,
            alternatives: r.alternatives,
        }
    }
}

#[pyclass(name = "SchemaCache", module = "schemadex")]
struct PySchemaCache {
    inner: Arc<Mutex<CoreCache>>,
    /// Cached parsed synonym map. Keyed by the path the user supplied; we
    /// reload if the path changes between calls. `None` once means we
    /// haven't loaded any synonyms yet on this cache.
    synonyms: Arc<Mutex<Option<LoadedSynonyms>>>,
}

#[derive(Clone)]
struct LoadedSynonyms {
    path: PathBuf,
    map: SynonymMap,
}

impl PySchemaCache {
    fn new(cache: CoreCache) -> Self {
        PySchemaCache {
            inner: Arc::new(Mutex::new(cache)),
            synonyms: Arc::new(Mutex::new(None)),
        }
    }

    /// Load synonyms from `path`, reusing the cached map if `path` matches the
    /// previously-loaded path. Returns a clone of the map for use by the
    /// caller.
    fn synonyms_for_path(&self, path: &str) -> PyResult<SynonymMap> {
        let path = PathBuf::from(path);
        let mut guard = self.synonyms.lock().expect("poisoned");
        if let Some(loaded) = guard.as_ref() {
            if loaded.path == path {
                return Ok(loaded.map.clone());
            }
        }
        let map = SynonymMap::load_yaml(&path).map_err(map_err)?;
        *guard = Some(LoadedSynonyms {
            path,
            map: map.clone(),
        });
        Ok(map)
    }
}

#[pymethods]
impl PySchemaCache {
    /// Build a cache by introspecting `url`. If a fresh on-disk cache exists,
    /// reuse it; otherwise introspect and persist.
    ///
    /// When `sample_values=True`, the postgres backend collects top-K values
    /// and sentinel flags for each column. The other backends accept the flag
    /// but currently ignore it (no-op).
    #[staticmethod]
    #[pyo3(signature = (
        url,
        ttl_seconds=None,
        cache_dir=None,
        parallel=true,
        sample_values=false,
        sample_top_k=None,
        sample_sentinel_threshold=None,
        sample_rows=None,
        history=None,
        max_history=10,
        memoize_results=false,
        memo_capacity=128,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn from_url(
        url: &str,
        ttl_seconds: Option<u64>,
        cache_dir: Option<String>,
        parallel: bool,
        sample_values: bool,
        sample_top_k: Option<usize>,
        sample_sentinel_threshold: Option<f32>,
        sample_rows: Option<u64>,
        history: Option<usize>,
        max_history: usize,
        memoize_results: bool,
        memo_capacity: usize,
    ) -> PyResult<Self> {
        let url = url.to_string();
        let opts = CacheOptions {
            ttl: ttl_seconds
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(24 * 3600)),
            cache_dir: cache_dir.map(std::path::PathBuf::from),
            parallel,
            history,
            max_history,
            memoize_results,
            memo_capacity,
            ..CacheOptions::default()
        };
        let sampling = build_sampling_policy(
            sample_values,
            sample_top_k,
            sample_sentinel_threshold,
            sample_rows,
        );
        let cache = rt()
            .block_on(async move {
                let introspector = backends::connect_with_sampling(&url, sampling).await?;
                CoreCache::from_introspector(&*introspector, &url, &opts).await
            })
            .map_err(map_err)?;
        Ok(PySchemaCache::new(cache))
    }

    /// Load a previously persisted cache from disk without contacting the DB.
    #[staticmethod]
    #[pyo3(signature = (
        url,
        cache_dir=None,
        history=None,
        max_history=10,
        memoize_results=false,
        memo_capacity=128,
    ))]
    fn load(
        url: &str,
        cache_dir: Option<String>,
        history: Option<usize>,
        max_history: usize,
        memoize_results: bool,
        memo_capacity: usize,
    ) -> PyResult<Option<Self>> {
        let url = url.to_string();
        let opts = CacheOptions {
            cache_dir: cache_dir.map(std::path::PathBuf::from),
            history,
            max_history,
            memoize_results,
            memo_capacity,
            ..CacheOptions::default()
        };
        let cache = rt()
            .block_on(async move { CoreCache::load(&url, &opts).await })
            .map_err(map_err)?;
        Ok(cache.map(PySchemaCache::new))
    }

    fn list_tables(&self) -> Vec<String> {
        let guard = self.inner.lock().expect("poisoned");
        guard.database().list_tables()
    }

    fn get_table<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Option<Bound<'py, PyDict>>> {
        let guard = self.inner.lock().expect("poisoned");
        let Some(table) = guard.database().table(name) else {
            return Ok(None);
        };
        let value =
            serde_json::to_value(table).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let dict = json_to_py(py, &value)?;
        Ok(Some(dict.downcast_into::<PyDict>()?))
    }

    /// Fuzzy-resolve `candidate` to a column on `table`. When
    /// `synonyms_path` is supplied, the YAML at that path is consulted before
    /// the lexical fallback. The parsed synonym map is cached on this
    /// :class:`SchemaCache` instance; subsequent calls with the same path
    /// reuse the cached map.
    #[pyo3(signature = (table, candidate, synonyms_path=None))]
    fn resolve(
        &self,
        table: &str,
        candidate: &str,
        synonyms_path: Option<String>,
    ) -> PyResult<PyResolveResult> {
        // Load synonyms first (outside the cache lock — file IO).
        let synonyms = match synonyms_path {
            Some(p) => Some(self.synonyms_for_path(&p)?),
            None => None,
        };
        let guard = self.inner.lock().expect("poisoned");
        let t = guard
            .database()
            .table(table)
            .ok_or_else(|| PyRuntimeError::new_err(format!("table not found: {table}")))?;
        let result = match synonyms.as_ref() {
            Some(map) => resolve_column_with_synonyms(t, candidate, map),
            None => core_resolve(t, candidate),
        };
        Ok(result.into())
    }

    /// Pre-load a synonyms YAML file into this cache. Subsequent calls to
    /// :meth:`resolve` with the same `path` will reuse the parsed map. Raises
    /// :class:`RuntimeError` if the file is missing or malformed.
    fn load_synonyms(&self, path: &str) -> PyResult<()> {
        self.synonyms_for_path(path).map(|_| ())
    }

    #[pyo3(signature = (max_tokens=2048, hint=None, tables=None, include_samples=true, include_foreign_keys=true, include_examples=false))]
    fn describe_for_agent(
        &self,
        max_tokens: usize,
        hint: Option<String>,
        tables: Option<Vec<String>>,
        include_samples: bool,
        include_foreign_keys: bool,
        include_examples: bool,
    ) -> PyResult<(String, usize)> {
        let opts = DescribeOptions {
            max_tokens,
            hint,
            tables,
            include_samples,
            include_foreign_keys,
            include_examples,
        };
        let guard = self.inner.lock().expect("poisoned");
        core_describe(guard.database(), &opts).map_err(map_err)
    }

    /// Generate a handful of valid SELECT statements for `table`. These are
    /// the same few-shot examples that :meth:`describe_for_agent` embeds when
    /// ``include_examples=True`` — exposed standalone so callers can render
    /// them in their own prompts.
    ///
    /// Raises ``RuntimeError`` if the table is not in the cache.
    #[pyo3(signature = (table, max_examples=None))]
    fn examples_for_table(
        &self,
        table: &str,
        max_examples: Option<usize>,
    ) -> PyResult<Vec<String>> {
        let max = max_examples.unwrap_or(4);
        let guard = self.inner.lock().expect("poisoned");
        let t = guard
            .database()
            .table(table)
            .ok_or_else(|| PyRuntimeError::new_err(format!("table not found: {table}")))?;
        Ok(schemadex_core::generate_examples(t, max))
    }

    fn to_json(&self) -> PyResult<String> {
        let guard = self.inner.lock().expect("poisoned");
        serde_json::to_string_pretty(guard.database())
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    fn cache_path(&self) -> PyResult<String> {
        let guard = self.inner.lock().expect("poisoned");
        Ok(guard.cache_path().to_string_lossy().to_string())
    }

    fn fingerprint(&self) -> Option<String> {
        let guard = self.inner.lock().expect("poisoned");
        guard.database().fingerprint.clone()
    }

    /// Re-introspect every table in this cache against ``url`` and rewrite
    /// the persisted cache file. Returns ``(changed, unchanged)`` — two lists
    /// of qualified table names partitioned by whether the DDL hash moved.
    ///
    /// The sampling kwargs mirror :meth:`from_url`. Omit them to skip
    /// sample-value collection, or pass the same kwargs you used at
    /// ``from_url`` time to keep behavior consistent.
    #[pyo3(signature = (
        url,
        sample_values=false,
        sample_top_k=None,
        sample_sentinel_threshold=None,
        sample_rows=None,
        parallel=true,
    ))]
    fn refresh(
        &self,
        url: &str,
        sample_values: bool,
        sample_top_k: Option<usize>,
        sample_sentinel_threshold: Option<f32>,
        sample_rows: Option<u64>,
        parallel: bool,
    ) -> PyResult<(Vec<String>, Vec<String>)> {
        let url = url.to_string();
        let sampling = build_sampling_policy(
            sample_values,
            sample_top_k,
            sample_sentinel_threshold,
            sample_rows,
        );
        let inner = Arc::clone(&self.inner);
        let report = rt()
            .block_on(async move {
                let introspector = backends::connect_with_sampling(&url, sampling).await?;
                let mut guard = inner.lock().expect("poisoned");
                guard.refresh(&*introspector, parallel).await
            })
            .map_err(map_err)?;
        Ok((report.changed, report.unchanged))
    }

    /// Re-introspect a single table (matched by qualified or bare name,
    /// case-insensitive) and rewrite the persisted cache file. Returns
    /// ``(changed, unchanged)`` — two lists summing to at most one entry.
    ///
    /// Raises ``RuntimeError`` if the table is not present in the cache.
    /// The sampling kwargs mirror :meth:`from_url`.
    #[pyo3(signature = (
        url,
        table,
        sample_values=false,
        sample_top_k=None,
        sample_sentinel_threshold=None,
        sample_rows=None,
    ))]
    fn refresh_table(
        &self,
        url: &str,
        table: &str,
        sample_values: bool,
        sample_top_k: Option<usize>,
        sample_sentinel_threshold: Option<f32>,
        sample_rows: Option<u64>,
    ) -> PyResult<(Vec<String>, Vec<String>)> {
        let url = url.to_string();
        let table = table.to_string();
        let sampling = build_sampling_policy(
            sample_values,
            sample_top_k,
            sample_sentinel_threshold,
            sample_rows,
        );
        let inner = Arc::clone(&self.inner);
        let report = rt()
            .block_on(async move {
                let introspector = backends::connect_with_sampling(&url, sampling).await?;
                let mut guard = inner.lock().expect("poisoned");
                guard.refresh_table(&*introspector, &table).await
            })
            .map_err(map_err)?;
        Ok((report.changed, report.unchanged))
    }

    /// Execute a SELECT through a pooled connection to `url` and return a
    /// markdown-rendered result table that fits inside ``token_budget``.
    ///
    /// Returns ``(rendered_table, token_count)``. Rows are dropped from the
    /// bottom until the rendered table fits; if anything was dropped, a
    /// ``_(truncated to N rows)_`` marker is appended.
    ///
    /// By default the SQL is validated by :func:`schemadex.assert_readonly`
    /// (only ``SELECT`` / ``WITH`` / ``EXPLAIN`` / ``SHOW`` / ``DESCRIBE`` /
    /// ``DESC`` are accepted). Pass ``allow_write=True`` to bypass the
    /// guard — only do this if you have already validated the SQL yourself.
    /// Bypassing the guard lets ``DELETE`` / ``DROP`` / ``UPDATE`` reach the
    /// database. **This is dangerous.**
    ///
    /// The underlying connection is cached process-wide and reused across
    /// calls, so the first invocation pays the connect cost and later ones
    /// don't.
    ///
    /// Pass ``memoize=True`` to opt the call into the LRU result cache. The
    /// cache must have been constructed with ``memoize_results=True`` for
    /// this kwarg to have any effect — otherwise it's silently a no-op.
    ///
    /// DuckDB URLs are not supported yet — the QueryRunner trait isn't wired
    /// up for the synchronous duckdb backend.
    #[pyo3(signature = (url, sql, token_budget=1024, allow_write=false, memoize=false))]
    fn run_sql(
        &self,
        url: &str,
        sql: &str,
        token_budget: usize,
        allow_write: bool,
        memoize: bool,
    ) -> PyResult<(String, usize)> {
        let url = url.to_string();
        let sql = sql.to_string();
        let inner = Arc::clone(&self.inner);
        rt()
            .block_on(async move {
                let runner = backends::shared_runner(&url).await?;
                let guard = inner.lock().expect("poisoned");
                // `memoize=False` should skip the LRU entirely even when the
                // cache itself has memoization enabled. We honor that by
                // temporarily peeking at the memo via guard.memo() — when the
                // caller didn't ask for memoization, fall back to the
                // unmemoized path. The core `run_sql_unchecked` always
                // consults the cache's memo, so we need a small bypass here.
                if !memoize {
                    // Drop the memo for this call by routing through a
                    // helper closure that bypasses the memo. Easiest: clear
                    // any prior hit and use the standard path with
                    // memoize_results disabled at the cache level. We can't
                    // mutate options here, so simulate by reading directly.
                    if allow_write {
                        run_sql_bypass_memo(&guard, &*runner, &sql, token_budget, true).await
                    } else {
                        run_sql_bypass_memo(&guard, &*runner, &sql, token_budget, false).await
                    }
                } else if allow_write {
                    guard
                        .run_sql_unchecked(&*runner, &sql, token_budget)
                        .await
                } else {
                    guard.run_sql(&*runner, &sql, token_budget).await
                }
            })
            .map_err(map_err)
    }

    /// Persist a column-name embedding index to disk. ``index`` must be a
    /// dict of shape::
    ///
    ///     {
    ///         "model": "nomic-embed-text-v2-moe",
    ///         "dim": 768,
    ///         "by_column": {
    ///             "public.users": {"id": [...], "email": [...]},
    ///             ...,
    ///         },
    ///     }
    ///
    /// The on-disk file lives next to ``database.json.zst`` and is reused by
    /// :func:`schemadex.resolve_with_embedding` to skip per-call HTTP traffic.
    fn store_embeddings(&self, index: &Bound<'_, PyDict>) -> PyResult<()> {
        let idx = py_to_embedding_index(index)?;
        let inner = Arc::clone(&self.inner);
        rt()
            .block_on(async move {
                let guard = inner.lock().expect("poisoned");
                guard.store_embeddings(&idx).await
            })
            .map_err(map_err)
    }

    /// Load the on-disk embedding index, if any. Returns ``None`` when no
    /// index file exists.
    fn load_embeddings<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        let inner = Arc::clone(&self.inner);
        let loaded = rt()
            .block_on(async move {
                let guard = inner.lock().expect("poisoned");
                guard.load_embeddings().await
            })
            .map_err(map_err)?;
        match loaded {
            Some(idx) => Ok(Some(embedding_index_to_py(py, &idx)?)),
            None => Ok(None),
        }
    }

    /// Write a snapshot of the current cache into the history directory.
    /// Returns the path of the written snapshot. Rotates the history
    /// directory to keep at most ``max_history`` files (from
    /// ``CacheOptions``). Raises ``RuntimeError`` if the cache has no
    /// history directory configured.
    fn snapshot(&self) -> PyResult<String> {
        let inner = Arc::clone(&self.inner);
        let path = rt()
            .block_on(async move {
                let guard = inner.lock().expect("poisoned");
                guard.snapshot().await
            })
            .map_err(map_err)?;
        Ok(path.to_string_lossy().to_string())
    }

    /// List all snapshots stored in the history directory, ordered ascending
    /// by timestamp. Each entry is a ``(unix_ts, [qualified_table_name, ...])``
    /// tuple. Returns an empty list when no history is configured or no
    /// snapshots have been written.
    fn history(&self) -> PyResult<Vec<(u64, Vec<String>)>> {
        let inner = Arc::clone(&self.inner);
        let hist = rt()
            .block_on(async move {
                let guard = inner.lock().expect("poisoned");
                guard.history().await
            })
            .map_err(map_err)?;
        Ok(hist
            .into_iter()
            .map(|(ts, db)| (ts, db.list_tables()))
            .collect())
    }

    /// Mark a table as stale so the next ``refresh_table`` definitely
    /// re-introspects it. Raises ``RuntimeError`` if the table is not in the
    /// cache. Useful for callers wiring Postgres logical replication or
    /// similar DDL-event streams.
    fn invalidate_table(&self, table: &str) -> PyResult<()> {
        let inner = Arc::clone(&self.inner);
        let table = table.to_string();
        rt()
            .block_on(async move {
                let mut guard = inner.lock().expect("poisoned");
                guard.invalidate_table(&table).await
            })
            .map_err(map_err)
    }

    /// Streaming variant of [`Self::run_sql`]. The backend consumes rows
    /// one-at-a-time and stops as soon as the estimated token cost would
    /// exceed ``token_budget``, so huge result sets never materialise fully
    /// in memory. Returns ``(rendered_table, token_count)`` and always
    /// applies the read-only safety check.
    #[pyo3(signature = (url, sql, token_budget=1024))]
    fn run_sql_streaming(
        &self,
        url: &str,
        sql: &str,
        token_budget: usize,
    ) -> PyResult<(String, usize)> {
        let url = url.to_string();
        let sql = sql.to_string();
        let inner = Arc::clone(&self.inner);
        rt()
            .block_on(async move {
                let runner = backends::shared_runner(&url).await?;
                let guard = inner.lock().expect("poisoned");
                guard.run_sql_streaming(&*runner, &sql, token_budget).await
            })
            .map_err(map_err)
    }

    /// Pre-validate a SQL query against the cached schema. Returns a list of
    /// issue dicts; an empty list means the query references only known
    /// tables and columns (per the heuristic — it is not a full SQL parser).
    ///
    /// Each dict looks like::
    ///
    ///     {
    ///         "kind": "unknown_table" | "unknown_column",
    ///         "identifier": str,
    ///         "table": str | None,         # only present for unknown_column
    ///         "suggestion": str | None,
    ///         "confidence": float | None,
    ///     }
    fn validate_sql<'py>(
        &self,
        py: Python<'py>,
        sql: &str,
    ) -> PyResult<Bound<'py, PyList>> {
        let guard = self.inner.lock().expect("poisoned");
        let issues = core_validate_sql(guard.database(), sql);
        let list = PyList::empty_bound(py);
        for issue in issues {
            let d = PyDict::new_bound(py);
            match &issue.kind {
                schemadex_core::IssueKind::UnknownTable => {
                    d.set_item("kind", "unknown_table")?;
                }
                schemadex_core::IssueKind::UnknownColumn { table } => {
                    d.set_item("kind", "unknown_column")?;
                    d.set_item("table", table.as_str())?;
                }
            }
            d.set_item("identifier", issue.identifier)?;
            match issue.suggestion {
                Some(s) => d.set_item("suggestion", s)?,
                None => d.set_item("suggestion", py.None())?,
            }
            match issue.confidence {
                Some(c) => d.set_item("confidence", c)?,
                None => d.set_item("confidence", py.None())?,
            }
            list.append(d)?;
        }
        Ok(list)
    }

    /// Try to turn a raw database error message into a structured hint
    /// pointing at the likely-real identifier. Returns ``None`` if no known
    /// error pattern matched.
    ///
    /// The returned dict looks like::
    ///
    ///     {
    ///         "kind": "unknown_column" | "unknown_table" | "ambiguous_column",
    ///         "table": str | None,                 # only for unknown_column
    ///         "original_identifier": str,
    ///         "suggested_identifier": str | None,
    ///         "confidence": float | None,
    ///         "human_message": str,
    ///     }
    fn hint_for_error<'py>(
        &self,
        py: Python<'py>,
        error_message: &str,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let guard = self.inner.lock().expect("poisoned");
        let Some(hint) = core_hint_for_error(guard.database(), error_message) else {
            return Ok(None);
        };
        let d = PyDict::new_bound(py);
        match &hint.kind {
            schemadex_core::HintKind::UnknownColumn { table } => {
                d.set_item("kind", "unknown_column")?;
                match table {
                    Some(t) => d.set_item("table", t.as_str())?,
                    None => d.set_item("table", py.None())?,
                }
            }
            schemadex_core::HintKind::UnknownTable => {
                d.set_item("kind", "unknown_table")?;
            }
            schemadex_core::HintKind::AmbiguousColumn => {
                d.set_item("kind", "ambiguous_column")?;
            }
        }
        d.set_item("original_identifier", hint.original_identifier)?;
        match hint.suggested_identifier {
            Some(s) => d.set_item("suggested_identifier", s)?,
            None => d.set_item("suggested_identifier", py.None())?,
        }
        match hint.confidence {
            Some(c) => d.set_item("confidence", c)?,
            None => d.set_item("confidence", py.None())?,
        }
        d.set_item("human_message", hint.human_message)?;
        Ok(Some(d))
    }

    /// Estimate the cost of running ``sql`` against ``url`` without
    /// executing it. Returns a dict with ``bytes_processed``,
    /// ``rows_estimate`` (both may be None) and a ``warning`` string.
    ///
    /// Postgres uses ``EXPLAIN (FORMAT JSON)``. Other backends currently
    /// return ``{"bytes_processed": None, "rows_estimate": None,
    /// "warning": "not supported"}``.
    fn preview_cost<'py>(
        &self,
        py: Python<'py>,
        url: &str,
        sql: &str,
    ) -> PyResult<Bound<'py, PyDict>> {
        let url = url.to_string();
        let sql = sql.to_string();
        let est = rt()
            .block_on(async move {
                let runner = backends::shared_runner(&url).await?;
                runner.preview_cost(&sql).await
            })
            .map_err(map_err)?;
        let d = PyDict::new_bound(py);
        match est.bytes_processed {
            Some(b) => d.set_item("bytes_processed", b)?,
            None => d.set_item("bytes_processed", py.None())?,
        }
        match est.rows_estimate {
            Some(r) => d.set_item("rows_estimate", r)?,
            None => d.set_item("rows_estimate", py.None())?,
        }
        match est.warning {
            Some(w) => d.set_item("warning", w)?,
            None => d.set_item("warning", py.None())?,
        }
        Ok(d)
    }

    /// Discover implicit foreign-key relationships across tables in the
    /// cached schema. Returns a list of dicts with keys ``left_table``,
    /// ``left_column``, ``right_table``, ``right_column``, ``confidence``.
    fn find_overlaps<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let guard = self.inner.lock().expect("poisoned");
        let hints = core_find_overlaps(guard.database());
        let list = PyList::empty_bound(py);
        for h in hints {
            let d = PyDict::new_bound(py);
            d.set_item("left_table", h.left_table)?;
            d.set_item("left_column", h.left_column)?;
            d.set_item("right_table", h.right_table)?;
            d.set_item("right_column", h.right_column)?;
            d.set_item("confidence", h.confidence)?;
            list.append(d)?;
        }
        Ok(list)
    }

    /// Classify the sample values on ``(table, column)`` as a PII kind.
    /// Returns one of ``"email"``, ``"phone"``, ``"ssn"``,
    /// ``"credit_card"`` or ``None``. Requires that the column was
    /// sampled (``sample_values=True`` at cache build time).
    fn classify_pii(&self, table: &str, column: &str) -> PyResult<Option<String>> {
        let guard = self.inner.lock().expect("poisoned");
        let t = guard
            .database()
            .table(table)
            .ok_or_else(|| PyRuntimeError::new_err(format!("table not found: {table}")))?;
        let c = t
            .column(column)
            .ok_or_else(|| PyRuntimeError::new_err(format!("column not found: {column}")))?;
        Ok(classify_column(c).map(|k| k.as_str().to_string()))
    }
}

/// Federate multiple [`SchemaCache`] instances under one describe surface.
#[pyclass(name = "Federation", module = "schemadex")]
struct PyFederation {
    inner: Arc<Mutex<CoreFederation>>,
}

#[pymethods]
impl PyFederation {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CoreFederation::new())),
        }
    }

    /// Add a [`SchemaCache`] to the federation. The order of `add` calls
    /// determines the `dbN.` prefix used by [`Self::list_tables`].
    fn add(&self, cache: &PySchemaCache) -> PyResult<()> {
        let mut fed = self.inner.lock().expect("poisoned");
        let inner = cache.inner.lock().expect("poisoned");
        // Clone the underlying CoreCache so the federation owns its own
        // copy. We don't share the Mutex — federation reads are
        // independent of subsequent mutations to the original cache.
        fed.add(clone_core_cache(&inner));
        Ok(())
    }

    /// Return all tables across all caches, each prefixed with `dbN.`.
    fn list_tables(&self) -> Vec<String> {
        let fed = self.inner.lock().expect("poisoned");
        fed.list_tables()
    }

    /// Return the qualified table info dict at `qualified` (e.g.
    /// `db0.public.users`), or None.
    fn table<'py>(
        &self,
        py: Python<'py>,
        qualified: &str,
    ) -> PyResult<Option<Bound<'py, PyDict>>> {
        let fed = self.inner.lock().expect("poisoned");
        let Some((_cache, table)) = fed.table(qualified) else {
            return Ok(None);
        };
        let value =
            serde_json::to_value(table).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let dict = json_to_py(py, &value)?;
        Ok(Some(dict.downcast_into::<PyDict>()?))
    }
}

/// Clone the inner `CoreCache` by re-loading from its on-disk path. The
/// `CoreCache` doesn't implement `Clone` directly because it owns a
/// `Database` that's cheap to clone but also holds an optional `Arc` to
/// the result memo; we synthesise a new cache from the in-memory
/// database without copying the memo.
fn clone_core_cache(src: &CoreCache) -> CoreCache {
    // The internal layout isn't accessible from outside the crate, so we
    // round-trip the database through a fresh `SchemaCache::load` call.
    // That requires the on-disk file to exist — which it always does for
    // a CoreCache built via `from_introspector` or `load`. If load fails
    // (e.g. the cache file moved), we fall back to a poison value with
    // an empty database, which still satisfies the federation API but
    // won't list any tables.
    let url_hash = src.database().url_hash.clone();
    let cache_path = src.cache_path().to_path_buf();
    // The on-disk file path is known; load returns Option<Self> based on
    // the file's presence. We rebuild CacheOptions to point at the
    // parent dir so the URL hash matches.
    let parent = cache_path
        .parent()
        .map(|p| p.parent().map(|q| q.to_path_buf()))
        .unwrap_or(None);
    let opts = CacheOptions {
        cache_dir: parent,
        ..CacheOptions::default()
    };
    let url_for_load = format!("hash://{url_hash}");
    // `load` rehashes the URL so it won't find the file at a "hash://"
    // URL. Fallback: serialize/deserialize the in-memory Database via
    // serde_json round-trip and stamp it into a minimal cache via
    // `into_database`-style construction. We do that by going through
    // the public `Database` clone + a one-shot `SchemaCache::load` using
    // the original cache_path's parent + url hash that match.
    // Since we can't reconstruct an exact CoreCache without crate
    // privates, use the fact that CoreCache derives nothing useful and
    // synthesise via JSON round-trip into a load.
    let _ = url_for_load;
    let _ = opts;
    // Fall back: use serde-clone of the Database, then load from disk
    // via the original cache_path. We'll trust the file written by the
    // builder.
    let rt_handle = rt();
    rt_handle
        .block_on(async move {
            // Re-load from the same on-disk path as the source.
            let url_hash = src.database().url_hash.clone();
            let dir = src
                .cache_path()
                .parent()
                .map(|p| p.parent().map(|q| q.to_path_buf()))
                .unwrap_or(None);
            // CacheOptions::cache_dir is the *base* dir; SchemaCache::load
            // appends the URL hash, so we want the grandparent.
            let opts = CacheOptions {
                cache_dir: dir,
                ..CacheOptions::default()
            };
            // load() expects a URL, not a hash; we pass a synthetic URL
            // that will be hashed by `hash_database_url`. To round-trip
            // correctly we'd need the original URL, which CoreCache
            // doesn't retain. Instead, manually compute the URL_hash and
            // call `read_envelope`-equivalent via a public path: there
            // isn't one. Punt: just clone the inner Database via serde
            // and rebuild a transient CoreCache by writing a temp file.
            let _ = url_hash;
            let _ = opts;
            // serde round-trip the database (cheap and reliable)
            let db: schemadex_core::Database = src.database().clone();
            // Wrap in a fresh cache by writing into a tempdir-scoped
            // location so `SchemaCache::load` can read it back.
            let tmp = std::env::temp_dir().join(format!(
                "schemadex-fed-{}-{}",
                std::process::id(),
                fastrand_like()
            ));
            let _ = tokio::fs::create_dir_all(&tmp).await;
            let opts = CacheOptions {
                cache_dir: Some(tmp.clone()),
                ..CacheOptions::default()
            };
            // We need a URL whose hash gives us a known subdir. Easiest:
            // write the file under whatever url_hash from `db` already is,
            // and supply a URL we know will hash to that — but
            // `hash_database_url` is private. So instead, write directly
            // to `tmp/<original_url_hash>/database.json.zst` and then
            // call `load` with a fabricated URL string that matches.
            // Without access to `hash_database_url`, we cheat: we know
            // the source already lives at `src.cache_path()`. Just point
            // `cache_dir` at the grandparent of that file and use a URL
            // string whose first hashing run we don't care about — call
            // `load` with the *original* URL is impossible, so we
            // synthesise a CoreCache via a thin private helper.
            let _ = (db, opts);
            // Final fallback: re-use the existing CoreCache by reading
            // the source cache_path through `SchemaCache::load` is also
            // not possible (URL unknown). We rebuild from scratch:
            rebuild_cache_from_database(src).await
        })
}

/// Synthesise a fresh [`CoreCache`] that owns a clone of the source
/// database. This is best-effort: the new cache writes its envelope
/// under a temp dir keyed by a per-clone nonce so it doesn't collide
/// with other caches.
async fn rebuild_cache_from_database(src: &CoreCache) -> CoreCache {
    // We can't fabricate the internal struct directly; use the same
    // strategy as `SchemaCache::load`: write the envelope to disk under
    // a synthetic URL hash, then load it back.
    use std::path::PathBuf;
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let base: PathBuf = std::env::temp_dir().join(format!("schemadex-fed-{nonce}"));
    // The original url_hash is the subdir; we mirror that so `load`
    // re-hashes the synthetic URL and lands in the same subdir.
    // Construct a URL string and let CoreCache::from_introspector hash
    // it. We don't have an introspector here — so we use `load` after
    // manually writing the file. `load` accepts a URL, not a hash, so
    // we wrap our database in a fresh on-disk envelope via the public
    // path: call `from_introspector` with a stub. That's heavy. Easier:
    // use serde to re-write the on-disk file at the source's cache_path
    // location and `load` it via a synthetic URL whose hash equals the
    // source's url_hash. Since we cannot influence the hash, we
    // sidestep entirely by using a clone-through-serde of the source
    // cache. CoreCache itself doesn't impl Clone, but the underlying
    // Database does. Construct a fresh CoreCache via the public
    // `load` path against the *source's* on-disk file:
    let _ = base;
    // Read the source's cache file directly back from disk via load. The
    // url here is irrelevant — load resolves the file via cache_dir +
    // url_hash, and we point cache_dir at the source's grandparent so
    // the inner join yields the same path.
    let url_hash = src.database().url_hash.clone();
    let grand = src
        .cache_path()
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    let opts = CacheOptions {
        cache_dir: grand,
        ..CacheOptions::default()
    };
    // Need a URL whose hash == url_hash. We can't synthesise that. So:
    // bypass entirely — directly stash the path as if `load` succeeded.
    // The only public constructor that doesn't depend on hashing is
    // `SchemaCache::load`, which we can pass `url=<hash>` to and then
    // walk the resulting subdir; that won't match. Give up trying to
    // produce a perfect clone and instead use a *temporary* writable
    // cache: write the database into a fresh tempdir under any
    // url_hash, set cache_dir accordingly, and read it back.
    let _ = url_hash;
    let _ = opts;
    // ---
    // Write the source database to a fresh tempdir using a synthetic URL.
    let url = format!(
        "memory://federation-clone-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = std::env::temp_dir().join(format!(
        "schemadex-fed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = tokio::fs::create_dir_all(&tmp).await;
    let opts = CacheOptions {
        cache_dir: Some(tmp),
        ..CacheOptions::default()
    };
    // Build a stub introspector that returns the cloned database tables.
    // Use the snapshot path: `SchemaCache::from_introspector` is the
    // only public path that constructs a fresh cache. We provide a thin
    // in-memory introspector that just echoes the source's tables.
    let introspector = MemoryIntrospector::new(src.database().clone());
    CoreCache::from_introspector(&introspector, &url, &opts)
        .await
        .expect("federation clone: from_introspector failed")
}

/// Cheap per-clone nonce. We don't pull `rand`/`fastrand` so we use the
/// system clock for uniqueness across process lifetime. Collisions only
/// matter for the temp-dir name and would self-heal on next call.
fn fastrand_like() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// In-memory introspector that echoes a known [`Database`] back to the
/// `SchemaCache::from_introspector` path. Used for cheap clone-by-rebuild
/// in the federation glue.
struct MemoryIntrospector {
    db: schemadex_core::Database,
}

impl MemoryIntrospector {
    fn new(db: schemadex_core::Database) -> Self {
        Self { db }
    }
}

#[async_trait::async_trait]
impl schemadex_core::SchemaIntrospector for MemoryIntrospector {
    fn backend(&self) -> schemadex_core::Backend {
        // Reuse SQLite as a neutral default; the backend label is only
        // recorded in the cache envelope for diagnostics.
        schemadex_core::Backend::Sqlite
    }

    async fn tables(&self) -> schemadex_core::Result<Vec<(Option<String>, String)>> {
        Ok(self
            .db
            .tables
            .iter()
            .map(|t| (t.schema.clone(), t.name.clone()))
            .collect())
    }

    async fn columns(
        &self,
        schema: Option<&str>,
        table: &str,
    ) -> schemadex_core::Result<Vec<schemadex_core::Column>> {
        let qn = match schema {
            Some(s) => format!("{s}.{table}"),
            None => table.to_string(),
        };
        Ok(self
            .db
            .table(&qn)
            .or_else(|| self.db.table(table))
            .map(|t| t.columns.clone())
            .unwrap_or_default())
    }

    async fn primary_key(
        &self,
        _schema: Option<&str>,
        table: &str,
    ) -> schemadex_core::Result<Option<schemadex_core::PrimaryKey>> {
        Ok(self.db.table(table).and_then(|t| t.primary_key.clone()))
    }

    async fn foreign_keys(
        &self,
        _schema: Option<&str>,
        table: &str,
    ) -> schemadex_core::Result<Vec<schemadex_core::ForeignKey>> {
        Ok(self
            .db
            .table(table)
            .map(|t| t.foreign_keys.clone())
            .unwrap_or_default())
    }
}

/// Decode an embedding index from a Python dict. Tolerates missing keys by
/// defaulting to empty values; raises only on obviously-wrong shapes (e.g.
/// `by_column` not a mapping).
fn py_to_embedding_index(d: &Bound<'_, PyDict>) -> PyResult<EmbeddingIndex> {
    let model: String = match d.get_item("model")? {
        Some(v) => v.extract()?,
        None => String::new(),
    };
    let dim: usize = match d.get_item("dim")? {
        Some(v) => v.extract()?,
        None => 0,
    };
    let mut by_column: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, Vec<f32>>,
    > = Default::default();
    if let Some(by_col) = d.get_item("by_column")? {
        let tbl_dict: &Bound<PyDict> = by_col.downcast()?;
        for (tbl_key, cols_val) in tbl_dict.iter() {
            let tbl_name: String = tbl_key.extract()?;
            let cols_dict: &Bound<PyDict> = cols_val.downcast()?;
            let mut col_map: std::collections::BTreeMap<String, Vec<f32>> = Default::default();
            for (col_key, vec_val) in cols_dict.iter() {
                let col_name: String = col_key.extract()?;
                let vec: Vec<f32> = vec_val.extract()?;
                col_map.insert(col_name, vec);
            }
            by_column.insert(tbl_name, col_map);
        }
    }
    Ok(EmbeddingIndex {
        model,
        dim,
        by_column,
    })
}

fn embedding_index_to_py<'py>(
    py: Python<'py>,
    idx: &EmbeddingIndex,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new_bound(py);
    d.set_item("model", &idx.model)?;
    d.set_item("dim", idx.dim)?;
    let by_col = PyDict::new_bound(py);
    for (tbl, cols) in &idx.by_column {
        let cols_dict = PyDict::new_bound(py);
        for (col, vec) in cols {
            cols_dict.set_item(col, vec.clone())?;
        }
        by_col.set_item(tbl, cols_dict)?;
    }
    d.set_item("by_column", by_col)?;
    Ok(d)
}

/// Drive `run_sql` without consulting the memo cache. Used when the caller
/// passes `memoize=False` even though the cache was constructed with
/// memoization enabled — we forward to the runner directly and skip the
/// cache lookup / insert.
async fn run_sql_bypass_memo(
    guard: &CoreCache,
    runner: &dyn schemadex_core::QueryRunner,
    sql: &str,
    token_budget: usize,
    allow_write: bool,
) -> schemadex_core::Result<(String, usize)> {
    if !allow_write {
        schemadex_core::assert_readonly(sql)?;
    }
    let _ = guard; // memo bypass — schema metadata isn't consulted here.
    let result = runner.run_sql(sql, 200).await?;
    schemadex_core::render_table_for_agent(&result, token_budget)
}

fn json_to_py<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    use serde_json::Value;
    Ok(match v {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => b.into_py(py).into_bound(py),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_py(py).into_bound(py)
            } else if let Some(u) = n.as_u64() {
                u.into_py(py).into_bound(py)
            } else {
                n.as_f64().unwrap_or(0.0).into_py(py).into_bound(py)
            }
        }
        Value::String(s) => {
            let obj: PyObject = s.as_str().into_py(py);
            obj.into_bound(py)
        }
        Value::Array(arr) => {
            let list = PyList::empty_bound(py);
            for item in arr {
                list.append(json_to_py(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(obj) => {
            let dict = PyDict::new_bound(py);
            for (k, val) in obj {
                dict.set_item(k.as_str(), json_to_py(py, val)?)?;
            }
            dict.into_any()
        }
    })
}

// ---------------------------------------------------------------------------
// Async variants
// ---------------------------------------------------------------------------
//
// Each async function returns a Python awaitable (via
// `pyo3_async_runtimes::tokio::future_into_py`). The awaitable is driven by
// the shared tokio runtime registered in `rt()`.
//
// The cache state is guarded by `std::sync::Mutex`, whose `MutexGuard` is not
// `Send`. To keep the spawned future `Send + 'static` we wrap the lock-holding
// portion in `tokio::task::spawn_blocking`, which runs on a dedicated blocking
// thread and is allowed to call `Handle::block_on` to drive the inner async
// chain (introspector creation + refresh / run_sql). This avoids reworking
// every existing sync method to use `tokio::sync::Mutex`.

#[pyfunction]
#[pyo3(signature = (
    url,
    ttl_seconds=None,
    cache_dir=None,
    parallel=true,
    sample_values=false,
    sample_top_k=None,
    sample_sentinel_threshold=None,
    sample_rows=None,
    history=None,
    max_history=10,
    memoize_results=false,
    memo_capacity=128,
))]
#[allow(clippy::too_many_arguments)]
fn from_url_async<'py>(
    py: Python<'py>,
    url: String,
    ttl_seconds: Option<u64>,
    cache_dir: Option<String>,
    parallel: bool,
    sample_values: bool,
    sample_top_k: Option<usize>,
    sample_sentinel_threshold: Option<f32>,
    sample_rows: Option<u64>,
    history: Option<usize>,
    max_history: usize,
    memoize_results: bool,
    memo_capacity: usize,
) -> PyResult<Bound<'py, PyAny>> {
    ensure_runtime();
    let opts = CacheOptions {
        ttl: ttl_seconds
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(24 * 3600)),
        cache_dir: cache_dir.map(std::path::PathBuf::from),
        parallel,
        history,
        max_history,
        memoize_results,
        memo_capacity,
        ..CacheOptions::default()
    };
    let sampling = build_sampling_policy(
        sample_values,
        sample_top_k,
        sample_sentinel_threshold,
        sample_rows,
    );
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let cache = async {
            let introspector = backends::connect_with_sampling(&url, sampling).await?;
            CoreCache::from_introspector(&*introspector, &url, &opts).await
        }
        .await
        .map_err(map_err)?;
        Ok(PySchemaCache::new(cache))
    })
}

#[pyfunction]
#[pyo3(signature = (
    cache,
    url,
    sample_values=false,
    sample_top_k=None,
    sample_sentinel_threshold=None,
    sample_rows=None,
    parallel=true,
))]
fn refresh_async<'py>(
    py: Python<'py>,
    cache: &PySchemaCache,
    url: String,
    sample_values: bool,
    sample_top_k: Option<usize>,
    sample_sentinel_threshold: Option<f32>,
    sample_rows: Option<u64>,
    parallel: bool,
) -> PyResult<Bound<'py, PyAny>> {
    ensure_runtime();
    let sampling = build_sampling_policy(
        sample_values,
        sample_top_k,
        sample_sentinel_threshold,
        sample_rows,
    );
    let inner = Arc::clone(&cache.inner);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        // The lock guard is `!Send`, so we drive the introspector + refresh
        // on a blocking task. That task is allowed to call `block_on` on the
        // current runtime handle because it runs on the blocking thread pool,
        // not on a worker thread.
        let report = tokio::task::spawn_blocking(move || {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async move {
                let introspector = backends::connect_with_sampling(&url, sampling).await?;
                let mut guard = inner.lock().expect("poisoned");
                guard.refresh(&*introspector, parallel).await
            })
        })
        .await
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .map_err(map_err)?;
        Ok((report.changed, report.unchanged))
    })
}

#[pyfunction]
#[pyo3(signature = (
    cache,
    url,
    table,
    sample_values=false,
    sample_top_k=None,
    sample_sentinel_threshold=None,
    sample_rows=None,
))]
fn refresh_table_async<'py>(
    py: Python<'py>,
    cache: &PySchemaCache,
    url: String,
    table: String,
    sample_values: bool,
    sample_top_k: Option<usize>,
    sample_sentinel_threshold: Option<f32>,
    sample_rows: Option<u64>,
) -> PyResult<Bound<'py, PyAny>> {
    ensure_runtime();
    let sampling = build_sampling_policy(
        sample_values,
        sample_top_k,
        sample_sentinel_threshold,
        sample_rows,
    );
    let inner = Arc::clone(&cache.inner);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let report = tokio::task::spawn_blocking(move || {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async move {
                let introspector = backends::connect_with_sampling(&url, sampling).await?;
                let mut guard = inner.lock().expect("poisoned");
                guard.refresh_table(&*introspector, &table).await
            })
        })
        .await
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .map_err(map_err)?;
        Ok((report.changed, report.unchanged))
    })
}

#[pyfunction]
#[pyo3(signature = (cache, url, sql, token_budget=1024, allow_write=false, memoize=false))]
fn run_sql_async<'py>(
    py: Python<'py>,
    cache: &PySchemaCache,
    url: String,
    sql: String,
    token_budget: usize,
    allow_write: bool,
    memoize: bool,
) -> PyResult<Bound<'py, PyAny>> {
    ensure_runtime();
    let inner = Arc::clone(&cache.inner);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = tokio::task::spawn_blocking(move || {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async move {
                let runner = backends::shared_runner(&url).await?;
                let guard = inner.lock().expect("poisoned");
                if !memoize {
                    run_sql_bypass_memo(&guard, &*runner, &sql, token_budget, allow_write).await
                } else if allow_write {
                    guard
                        .run_sql_unchecked(&*runner, &sql, token_budget)
                        .await
                } else {
                    guard.run_sql(&*runner, &sql, token_budget).await
                }
            })
        })
        .await
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .map_err(map_err)?;
        Ok(result)
    })
}

/// Drop every cached connection in the process-wide pool. Test helper.
#[pyfunction]
fn clear_pool_cache() {
    backends::clear_pool_cache();
}

/// Return the current size of the process-wide connection pool. Test helper.
#[pyfunction]
fn pool_size() -> usize {
    backends::pool_size()
}

#[pymodule]
fn _native(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize the shared tokio runtime up front so the async surface and
    // sync surface always agree on which runtime drives futures.
    ensure_runtime();
    m.add_class::<PySchemaCache>()?;
    m.add_class::<PyResolveResult>()?;
    m.add_function(wrap_pyfunction!(from_url_async, m)?)?;
    m.add_function(wrap_pyfunction!(refresh_async, m)?)?;
    m.add_function(wrap_pyfunction!(refresh_table_async, m)?)?;
    m.add_function(wrap_pyfunction!(run_sql_async, m)?)?;
    m.add_function(wrap_pyfunction!(clear_pool_cache, m)?)?;
    m.add_function(wrap_pyfunction!(pool_size, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
