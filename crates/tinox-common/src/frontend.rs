//! Shared persistent frontend queries and opt-in stage timings.
use std::time::Instant;

/// Opt-in timings, emitted on scope exit (including failed queries).
/// Import and visibility stages include their nested parse/query stages.
pub struct Stage {
    name: &'static str,
    start: Option<Instant>,
}
impl Stage {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: std::env::var_os("TINOX_TIMINGS")
                .filter(|v| v == "1")
                .map(|_| Instant::now()),
        }
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            eprintln!(
                "[frontend-timing] {} {:.3} ms",
                self.name,
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}

use bincode::Options;
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_ENTRY: u64 = 64 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_ENTRY)
        .reject_trailing_bytes()
}
fn checksum(key: &str, payload: &[u8]) -> Vec<u8> {
    let mut hash = Sha256::new();
    hash.update(key.as_bytes());
    hash.update(payload);
    hash.finalize().to_vec()
}
fn root() -> Option<PathBuf> {
    std::env::var_os("TINOX_FRONTEND_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .map(|p| p.join("tinox/frontend-v1"))
        })
}
fn trace(kind: &str, outcome: &str, key: &str) {
    if std::env::var_os("TINOX_CACHE_TRACE").is_some_and(|v| v == "1") {
        eprintln!("[frontend-cache] {kind} {outcome} {key}");
    }
}

/// Immutable, content-addressed query. Inputs must describe the COMPLETE query,
/// including ordering, origins, spans, annotations and any node-ID numbering.
/// Filesystem resolution/validation remains outside this cache and runs afresh.
/// Cache I/O or decoding failures always fall back to computing the query.
pub fn query<K: Serialize, V: Serialize + DeserializeOwned>(
    kind: &str,
    input: &K,
    compute: impl FnOnce() -> V,
) -> V {
    let _stage = Stage::new("query");
    if std::env::var_os("TINOX_FRONTEND_CACHE").is_some_and(|v| v == "0") {
        trace(kind, "disabled", "");
        return compute();
    }
    let Some(root) = root() else { return compute() };
    let Ok(encoded) = codec().serialize(input) else {
        return compute();
    };
    let mut hash = Sha256::new();
    hash.update(env!("TINOX_FRONTEND_SCHEMA"));
    hash.update(kind.as_bytes());
    hash.update([0]);
    hash.update(&encoded);
    let key = format!("{:x}", hash.finalize());
    let dir = root.join(env!("TINOX_FRONTEND_SCHEMA")).join(kind);
    let path = dir.join(&key);
    let cached = (|| {
        use std::io::Read;
        let file = fs::File::open(&path).ok()?;
        if file.metadata().ok()?.len() > MAX_ENTRY + 32 {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_ENTRY + 33).read_to_end(&mut bytes).ok()?;
        let (checksum, payload) = bytes.split_at_checked(32)?;
        if checksum != self::checksum(&key, payload) {
            return None;
        }
        codec().deserialize(payload).ok()
    })();
    if let Some(value) = cached {
        trace(kind, "hit", &key);
        return value;
    }
    trace(kind, "miss", &key);
    let value = compute();
    if let Ok(payload) = codec().serialize(&value) {
        let _ = (|| -> std::io::Result<()> {
            use std::io::Write;
            fs::create_dir_all(&dir)?;
            let temp = dir.join(format!(
                ".tmp-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            let result = (|| {
                file.write_all(&checksum(&key, &payload))?;
                file.write_all(&payload)?;
                drop(file);
                fs::rename(&temp, &path)
            })();
            let _ = fs::remove_file(temp);
            result
        })();
    }
    value
}
