//! PyO3 bindings for schemadex. Exposes the cache, resolution, and
//! agent-describe APIs to Python.

#![allow(clippy::useless_conversion)] // false-positive from `#[pymethods]` expansion

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use pyo3::IntoPy;

use schemadex_core::{
    backends, cache::CacheOptions, describe_for_agent as core_describe,
    resolve_column as core_resolve, DescribeOptions, ResolveResult, SchemaCache as CoreCache,
    SchemadexError,
};
use std::sync::Arc;
use std::time::Duration;

fn map_err(e: SchemadexError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

fn rt() -> &'static tokio::runtime::Runtime {
    use std::sync::OnceLock;
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to start tokio runtime")
    })
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
    inner: Arc<CoreCache>,
}

#[pymethods]
impl PySchemaCache {
    /// Build a cache by introspecting `url`. If a fresh on-disk cache exists,
    /// reuse it; otherwise introspect and persist.
    #[staticmethod]
    #[pyo3(signature = (url, ttl_seconds=None, cache_dir=None, parallel=true))]
    fn from_url(
        url: &str,
        ttl_seconds: Option<u64>,
        cache_dir: Option<String>,
        parallel: bool,
    ) -> PyResult<Self> {
        let url = url.to_string();
        let opts = CacheOptions {
            ttl: ttl_seconds
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(24 * 3600)),
            cache_dir: cache_dir.map(std::path::PathBuf::from),
            parallel,
        };
        let cache = rt()
            .block_on(async move {
                let introspector = backends::connect(&url).await?;
                CoreCache::from_introspector(&*introspector, &url, &opts).await
            })
            .map_err(map_err)?;
        Ok(PySchemaCache {
            inner: Arc::new(cache),
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
        Ok(cache.map(|c| PySchemaCache { inner: Arc::new(c) }))
    }

    fn list_tables(&self) -> Vec<String> {
        self.inner.database().list_tables()
    }

    fn get_table<'py>(&self, py: Python<'py>, name: &str) -> PyResult<Option<Bound<'py, PyDict>>> {
        let Some(table) = self.inner.database().table(name) else {
            return Ok(None);
        };
        let value =
            serde_json::to_value(table).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let dict = json_to_py(py, &value)?;
        Ok(Some(dict.downcast_into::<PyDict>()?))
    }

    fn resolve(&self, table: &str, candidate: &str) -> PyResult<PyResolveResult> {
        let t = self
            .inner
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
        core_describe(self.inner.database(), &opts).map_err(map_err)
    }

    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string_pretty(self.inner.database())
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    fn cache_path(&self) -> PyResult<String> {
        Ok(self.inner.cache_path().to_string_lossy().to_string())
    }

    fn fingerprint(&self) -> Option<String> {
        self.inner.database().fingerprint.clone()
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

#[pymodule]
fn _native(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySchemaCache>()?;
    m.add_class::<PyResolveResult>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
