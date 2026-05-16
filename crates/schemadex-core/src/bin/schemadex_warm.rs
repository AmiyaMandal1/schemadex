//! Pre-build a SchemaCache for `--url`. Optional `--sample-values` flag
//! collects top-K samples + sentinels. Optional `--refresh` forces
//! re-introspection of an existing cache. Optional `--embeddings` pre-builds
//! the column-name embedding index used by the Python embedding-resolve
//! fallback. Drop into Dockerfiles or CI warmup steps.

use schemadex_core::cache::EmbeddingIndex;
use std::collections::BTreeMap;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    // argparse via simple `args()` iterator (no clap, keep deps light)
    let mut args = std::env::args().skip(1);
    let mut url: Option<String> = None;
    let mut cache_dir: Option<String> = None;
    let mut sample = false;
    let mut refresh = false;
    let mut embeddings = false;
    let mut ollama_url = String::from("http://localhost:11434");
    let mut model = String::from("nomic-embed-text-v2-moe");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => url = args.next(),
            "--cache-dir" => cache_dir = args.next(),
            "--sample-values" => sample = true,
            "--refresh" => refresh = true,
            "--embeddings" => embeddings = true,
            "--ollama-url" => {
                if let Some(v) = args.next() {
                    ollama_url = v;
                }
            }
            "--embedding-model" => {
                if let Some(v) = args.next() {
                    model = v;
                }
            }
            "--help" | "-h" => {
                println!("usage: schemadex-warm --url <URL> [--cache-dir DIR] [--sample-values] [--refresh] [--embeddings] [--ollama-url URL] [--embedding-model NAME]");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("unknown arg: {a}");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(url) = url else {
        eprintln!("--url required");
        return ExitCode::FAILURE;
    };

    let opts = schemadex_core::cache::CacheOptions {
        ttl: std::time::Duration::from_secs(24 * 3600),
        cache_dir: cache_dir.map(std::path::PathBuf::from),
        parallel: true,
        ..Default::default()
    };
    let policy = if sample {
        Some(schemadex_core::sampling::SamplingPolicy::default_policy())
    } else {
        None
    };

    let started = std::time::Instant::now();
    let result: schemadex_core::Result<schemadex_core::SchemaCache> = (async {
        let introspector = schemadex_core::backends::connect_with_sampling(&url, policy).await?;
        let mut cache = schemadex_core::SchemaCache::from_introspector(&*introspector, &url, &opts)
            .await?;
        if refresh {
            cache.refresh(&*introspector, true).await?;
        }
        Ok(cache)
    })
    .await;

    let cache = match result {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warm failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if embeddings {
        // Build the index by hitting Ollama once per (table, column) pair.
        // The CLI is intentionally light on retries — this is a one-shot
        // warmup; if Ollama is unhealthy the operator should fix it and
        // re-run.
        match build_embeddings(cache.database(), &ollama_url, &model).await {
            Ok(idx) => {
                if let Err(e) = cache.store_embeddings(&idx).await {
                    eprintln!("store_embeddings failed: {e}");
                    return ExitCode::FAILURE;
                }
                println!(
                    "embeddings indexed: {} tables, model={} dim={}",
                    idx.by_column.len(),
                    idx.model,
                    idx.dim
                );
            }
            Err(e) => {
                eprintln!("embedding warmup failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let n = cache.database().tables.len();
    let path = cache.cache_path().display();
    let elapsed = started.elapsed();
    println!("warmed {n} tables in {:.2?} -> {path}", elapsed);
    ExitCode::SUCCESS
}

async fn build_embeddings(
    db: &schemadex_core::Database,
    ollama_url: &str,
    model: &str,
) -> Result<EmbeddingIndex, String> {
    let client = reqwest_blocking_client();
    let mut by_column: BTreeMap<String, BTreeMap<String, Vec<f32>>> = BTreeMap::new();
    let mut dim = 0usize;
    for t in &db.tables {
        let qn = t.qualified_name();
        let mut cols: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        for c in &t.columns {
            let v = client
                .embed(ollama_url, model, &c.name)
                .map_err(|e| format!("embed {qn}.{}: {e}", c.name))?;
            if dim == 0 {
                dim = v.len();
            }
            cols.insert(c.name.clone(), v);
        }
        by_column.insert(qn, cols);
    }
    Ok(EmbeddingIndex {
        model: model.to_string(),
        dim,
        by_column,
    })
}

// Tiny synchronous HTTP client wrapper so we don't pull `reqwest` into the
// dep graph. Uses the stdlib `std::net::TcpStream` indirectly via `ureq`-style
// hand-rolled HTTP. To keep deps zero we implement the bare-minimum POST
// against `/api/embeddings`.
struct BlockingHttp;
fn reqwest_blocking_client() -> BlockingHttp {
    BlockingHttp
}

impl BlockingHttp {
    fn embed(
        &self,
        ollama_url: &str,
        model: &str,
        text: &str,
    ) -> Result<Vec<f32>, String> {
        // Parse host/port/path from the URL.
        let stripped = ollama_url.trim_end_matches('/');
        let url = format!("{stripped}/api/embeddings");
        let body = serde_json::json!({"model": model, "prompt": text}).to_string();

        // Delegate to a blocking helper that does a single HTTP/1.1 POST.
        let resp = http_post(&url, &body).map_err(|e| format!("HTTP error: {e}"))?;
        let payload: serde_json::Value =
            serde_json::from_str(&resp).map_err(|e| format!("decode error: {e}"))?;
        let vec = payload
            .get("embedding")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("no 'embedding' in response: {payload}"))?;
        let mut out = Vec::with_capacity(vec.len());
        for v in vec {
            out.push(v.as_f64().unwrap_or(0.0) as f32);
        }
        Ok(out)
    }
}

fn http_post(url: &str, body: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let parsed = url::Url::parse(url).map_err(|e| e.to_string())?;
    if parsed.scheme() != "http" {
        // The warm binary speaks only plain HTTP — Ollama default is localhost.
        // HTTPS support would pull in rustls; out of scope for a warmup tool.
        return Err(format!("only http:// is supported, got {}", parsed.scheme()));
    }
    let host = parsed.host_str().ok_or("missing host")?;
    let port = parsed.port().unwrap_or(80);
    let path = if parsed.path().is_empty() { "/" } else { parsed.path() };
    let addr = format!("{host}:{port}");
    let mut stream = TcpStream::connect(&addr).map_err(|e| e.to_string())?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    // Split headers / body.
    let idx = raw
        .find("\r\n\r\n")
        .ok_or("malformed HTTP response (no header terminator)")?;
    Ok(raw[idx + 4..].to_string())
}
