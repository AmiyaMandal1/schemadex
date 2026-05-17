//! Federate multiple SchemaCaches under a common describe surface.

use crate::SchemaCache;

pub struct Federation {
    pub caches: Vec<SchemaCache>,
}

impl Default for Federation {
    fn default() -> Self {
        Self::new()
    }
}

impl Federation {
    pub fn new() -> Self {
        Self { caches: vec![] }
    }

    pub fn add(&mut self, cache: SchemaCache) {
        self.caches.push(cache);
    }

    /// Return all tables across all caches, prefixed with cache index.
    pub fn list_tables(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, c) in self.caches.iter().enumerate() {
            for t in c.database().list_tables() {
                out.push(format!("db{i}.{}", t));
            }
        }
        out
    }

    pub fn table(&self, qualified: &str) -> Option<(&SchemaCache, &crate::model::Table)> {
        let (prefix, rest) = qualified.split_once('.')?;
        if !prefix.starts_with("db") {
            return None;
        }
        let idx: usize = prefix[2..].parse().ok()?;
        let cache = self.caches.get(idx)?;
        let table = cache.database().table(rest)?;
        Some((cache, table))
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::backends;
    use crate::cache::CacheOptions;
    use crate::SchemaCache;
    use tempfile::TempDir;

    async fn build_sqlite_cache(tmp: &TempDir, name: &str, schema_sql: &str) -> SchemaCache {
        let db_path = tmp.path().join(format!("{name}.sqlite"));
        let url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        sqlx::raw_sql(schema_sql).execute(&pool).await.unwrap();
        pool.close().await;
        let opts = CacheOptions {
            cache_dir: Some(tmp.path().join(format!("cache-{name}"))),
            ..CacheOptions::default()
        };
        let introspector = backends::connect(&url).await.unwrap();
        SchemaCache::from_introspector(&*introspector, &url, &opts)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn list_tables_prefixes_each_cache() {
        let tmp = TempDir::new().unwrap();
        let cache_a = build_sqlite_cache(
            &tmp,
            "a",
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
        )
        .await;
        let cache_b = build_sqlite_cache(
            &tmp,
            "b",
            "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT);",
        )
        .await;
        let mut fed = Federation::new();
        fed.add(cache_a);
        fed.add(cache_b);
        let tables = fed.list_tables();
        assert!(tables.iter().any(|t| t == "db0.users"), "{:?}", tables);
        assert!(tables.iter().any(|t| t == "db1.products"), "{:?}", tables);

        let (_c, t) = fed.table("db0.users").expect("db0.users present");
        assert_eq!(t.name, "users");
    }
}
