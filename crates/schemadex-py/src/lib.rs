//! PyO3 bindings for schemadex. Exposes the cache, resolution, and
//! agent-describe APIs to Python.

#![allow(clippy::useless_conversion)] // false-positive from `#[pymethods]` expansion

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::IntoPy;

use schemadex_core::{
    backends, cache::CacheOptions, describe_for_agent as core_describe,
    resolve_column as core_resolve, sampling::SamplingPolicy, DescribeOptions, ResolveResult,
    SchemaCache as CoreCache, SchemadexError,
};
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
    ) -> PyResult<Self> {
        let url = url.to_string();
        let opts = CacheOptions {
            ttl: ttl_seconds
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(24 * 3600)),
            cache_dir: cache_dir.map(std::path::PathBuf::from),
            parallel,
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
        Ok(PySchemaCache {
            inner: Arc::new(Mutex::new(cache)),
        })
    }

    /// Load a previously persisted cache from disk without contacting the DB.
    #[staticmethod]
    #[pyo3(signature = (url, cache_dir=None))]
    fn load(url: &str, cache_dir: Option<String>) -> PyResult<Option<Self>> {
        let url = url.to_string();
        let opts = CacheOptions {
            cache_dir: cache_dir.map(std::path::PathBuf::from),
            ..CacheOptions::default()
        };
        let cache = rt()
            .block_on(async move { CoreCache::load(&url, &opts).await })
            .map_err(map_err)?;
        Ok(cache.map(|c| PySchemaCache {
            inner: Arc::new(Mutex::new(c)),
        }))
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

    fn resolve(&self, table: &str, candidate: &str) -> PyResult<PyResolveResult> {
        let guard = self.inner.lock().expect("poisoned");
        let t = guard
            .database()
            .table(table)
            .ok_or_else(|| PyRuntimeError::new_err(format!("table not found: {table}")))?;
        Ok(core_resolve(t, candidate).into())
    }

    #[pyo3(signature = (max_tokens=2048, hint=None, tables=None, include_samples=true, include_foreign_keys=true))]
    fn describe_for_agent(
        &self,
        max_tokens: usize,
        hint: Option<String>,
        tables: Option<Vec<String>>,
        include_samples: bool,
        include_foreign_keys: bool,
    ) -> PyResult<(String, usize)> {
        let opts = DescribeOptions {
            max_tokens,
            hint,
            tables,
            include_samples,
            include_foreign_keys,
        };
        let guard = self.inner.lock().expect("poisoned");
        core_describe(guard.database(), &opts).map_err(map_err)
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

    /// Execute a SELECT through a fresh connection to `url` and return a
    /// markdown-rendered result table that fits inside ``token_budget``.
    ///
    /// Returns ``(rendered_table, token_count)``. Rows are dropped from the
    /// bottom until the rendered table fits; if anything was dropped, a
    /// ``_(truncated to N rows)_`` marker is appended.
    ///
    /// DuckDB URLs are not supported yet — the QueryRunner trait isn't wired
    /// up for the synchronous duckdb backend.
    #[pyo3(signature = (url, sql, token_budget=1024))]
    fn run_sql(&self, url: &str, sql: &str, token_budget: usize) -> PyResult<(String, usize)> {
        let url = url.to_string();
        let sql = sql.to_string();
        let inner = Arc::clone(&self.inner);
        rt()
            .block_on(async move {
                let runner = backends::connect_runner(&url).await?;
                let guard = inner.lock().expect("poisoned");
                guard.run_sql(&*runner, &sql, token_budget).await
            })
            .map_err(map_err)
    }
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
) -> PyResult<Bound<'py, PyAny>> {
    ensure_runtime();
    let opts = CacheOptions {
        ttl: ttl_seconds
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(24 * 3600)),
        cache_dir: cache_dir.map(std::path::PathBuf::from),
        parallel,
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
        Ok(PySchemaCache {
            inner: Arc::new(Mutex::new(cache)),
        })
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
#[pyo3(signature = (cache, url, sql, token_budget=1024))]
fn run_sql_async<'py>(
    py: Python<'py>,
    cache: &PySchemaCache,
    url: String,
    sql: String,
    token_budget: usize,
) -> PyResult<Bound<'py, PyAny>> {
    ensure_runtime();
    let inner = Arc::clone(&cache.inner);
    pyo3_async_runtimes::tokio::future_into_py(py, async move {
        let result = tokio::task::spawn_blocking(move || {
            let handle = tokio::runtime::Handle::current();
            handle.block_on(async move {
                let runner = backends::connect_runner(&url).await?;
                let guard = inner.lock().expect("poisoned");
                guard.run_sql(&*runner, &sql, token_budget).await
            })
        })
        .await
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .map_err(map_err)?;
        Ok(result)
    })
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
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
