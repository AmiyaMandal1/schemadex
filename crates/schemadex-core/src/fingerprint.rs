use sha2::{Digest, Sha256};

pub fn sha256_hex(input: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_ref());
    hex::encode(hasher.finalize())
}

/// Stable hash for a database URL — strip credentials before hashing so the
/// cache key doesn't leak secrets onto disk.
pub fn hash_database_url(url: &str) -> String {
    let scrubbed = scrub_url(url);
    let short = &sha256_hex(scrubbed.as_bytes())[..16];
    short.to_string()
}

fn scrub_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_password(None);
            let _ = u.set_username("");
            u.to_string()
        }
        Err(_) => url.to_string(),
    }
}

pub fn ddl_hash(payload: &str) -> String {
    sha256_hex(payload.as_bytes())
}
