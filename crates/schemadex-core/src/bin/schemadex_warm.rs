//! Pre-build a SchemaCache for `--url`. Optional `--sample-values` flag
//! collects top-K samples + sentinels. Optional `--refresh` forces
//! re-introspection of an existing cache. Drop into Dockerfiles or CI
//! warmup steps.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    // argparse via simple `args()` iterator (no clap, keep deps light)
    let mut args = std::env::args().skip(1);
    let mut url: Option<String> = None;
    let mut cache_dir: Option<String> = None;
    let mut sample = false;
    let mut refresh = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => url = args.next(),
            "--cache-dir" => cache_dir = args.next(),
            "--sample-values" => sample = true,
            "--refresh" => refresh = true,
            "--help" | "-h" => {
                println!("usage: schemadex-warm --url <URL> [--cache-dir DIR] [--sample-values] [--refresh]");
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

    match result {
        Ok(cache) => {
            let n = cache.database().tables.len();
            let path = cache.cache_path().display();
            let elapsed = started.elapsed();
            println!("warmed {n} tables in {:.2?} -> {path}", elapsed);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("warm failed: {e}");
            ExitCode::FAILURE
        }
    }
}
