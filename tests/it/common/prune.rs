//! Pruning of the shared test cache (`$TMPDIR/wasm2rs-testcache`, or `$WASM2RS_TESTCACHE_DIR`),
//! which is content-addressed and would otherwise only grow.
//!
//! Safe to run from several test processes at once: every removal ignores errors (another
//! process may have removed the entry first) and a cache hit refreshes the mtime, so only
//! entries nobody has used lately are dropped.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Prefixes of cache entries: compiled test programs (`gen-`) and example programs (`ex-`).
const ENTRY_PREFIXES: &[&str] = &["gen-", "ex-"];
/// Prefix of the per-build working directories that a dead build leaves behind.
const WORK_PREFIX: &str = "work-";

pub struct Policy {
    /// Entries unused for longer than this are removed.
    pub max_age: Duration,
    /// Total size of the entries to keep; the least recently used go first.
    pub max_bytes: u64,
    /// Working directories older than this belong to a build that is no longer running.
    pub work_max_age: Duration,
}

impl Policy {
    /// 14 days, 10 GiB and 6 hours; `$WASM2RS_TESTCACHE_MAX_AGE_DAYS` and
    /// `$WASM2RS_TESTCACHE_MAX_GIB` override the first two.
    pub fn from_env() -> Self {
        let var = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        Policy {
            max_age: Duration::from_secs(var("WASM2RS_TESTCACHE_MAX_AGE_DAYS", 14) * 24 * 60 * 60),
            max_bytes: var("WASM2RS_TESTCACHE_MAX_GIB", 10) << 30,
            work_max_age: Duration::from_secs(6 * 60 * 60),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Entries and working directories removed.
    pub removed: usize,
}

/// Marks `path` as used just now, so that [`prune`] sees it as recent. Best effort.
///
/// Opened read-only: a binary that is open for writing cannot be `exec`ed (`ETXTBSY`), and a
/// concurrent test may be about to run this one. The owner may set times through a read-only fd.
pub fn touch(path: &Path) {
    if let Ok(file) = fs::File::open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// Prunes `dir` as of `now`: stale entries, then the least recently used beyond the size cap,
/// and the working directories of builds that died.
pub fn prune(dir: &Path, now: SystemTime, policy: &Policy) -> Report {
    let mut report = Report::default();
    let Ok(read) = fs::read_dir(dir) else {
        return report;
    };
    let mut live: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for item in read.flatten() {
        let name = item.file_name();
        let name = name.to_string_lossy();
        let Ok(meta) = item.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or_default();
        if name.starts_with(WORK_PREFIX) {
            if age > policy.work_max_age && fs::remove_dir_all(item.path()).is_ok() {
                report.removed += 1;
            }
        } else if meta.is_file() && ENTRY_PREFIXES.iter().any(|p| name.starts_with(p)) {
            if age > policy.max_age {
                if fs::remove_file(item.path()).is_ok() {
                    report.removed += 1;
                }
            } else {
                live.push((modified, meta.len(), item.path()));
            }
        }
    }
    let mut total: u64 = live.iter().map(|(_, len, _)| len).sum();
    live.sort();
    for (_, len, path) in live {
        if total <= policy.max_bytes {
            break;
        }
        if fs::remove_file(path).is_ok() {
            report.removed += 1;
        }
        total = total.saturating_sub(len);
    }
    report
}
