//! Pruning of the shared test cache (`$TMPDIR/wasm2rs-testcache`, or `$WASM2RS_TESTCACHE_DIR`),
//! which is content-addressed and would otherwise only grow, and of the per-checkout directories
//! beside the entries (`checkout-<id>/`: one worktree's `wasm2rs-wasi` build and scratch files).
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
/// Prefix of a checkout's own directory, and the names the same state had at the top level before
/// it was grouped (a cache restored from an older run still holds those).
const CHECKOUT_PREFIXES: &[&str] = &["checkout-", "wasi-target-", "example-wasi-target-"];
/// Suffix of the lock file that sits beside a legacy per-checkout target directory.
const LOCK_SUFFIX: &str = ".lock";

pub struct Policy {
    /// Entries unused for longer than this are removed.
    pub max_age: Duration,
    /// Total size of the entries to keep; the least recently used go first.
    pub max_bytes: u64,
    /// Working directories older than this belong to a build that is no longer running.
    pub work_max_age: Duration,
    /// A per-checkout directory none of whose files was touched for this long belongs to a
    /// worktree that is gone.
    pub checkout_max_age: Duration,
}

impl Policy {
    /// 14 days, 10 GiB, 6 hours and 14 days; `$WASM2RS_TESTCACHE_MAX_AGE_DAYS` (which also sets
    /// the last) and `$WASM2RS_TESTCACHE_MAX_GIB` override the first two.
    pub fn from_env() -> Self {
        let var = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let max_age = Duration::from_secs(var("WASM2RS_TESTCACHE_MAX_AGE_DAYS", 14) * 24 * 60 * 60);
        Policy {
            max_age,
            max_bytes: var("WASM2RS_TESTCACHE_MAX_GIB", 10) << 30,
            work_max_age: Duration::from_secs(6 * 60 * 60),
            checkout_max_age: max_age,
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

/// Whether anything in `path` (itself included) was modified at or after `cutoff`. Stops at the
/// first such file, so a directory in use costs one look and only an idle one is walked whole.
/// What cannot be read counts as in use: better to keep a directory than to remove one that a run
/// is working in.
fn modified_since(path: &Path, cutoff: SystemTime) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return true;
    };
    if meta.modified().map_or(true, |m| m >= cutoff) {
        return true;
    }
    if !meta.is_dir() {
        return false;
    }
    let Ok(read) = fs::read_dir(path) else {
        return true;
    };
    read.flatten()
        .any(|child| modified_since(&child.path(), cutoff))
}

/// Prunes `dir` as of `now`: stale entries, then the least recently used beyond the size cap,
/// the working directories of builds that died, and the directories of worktrees that went idle.
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
        if CHECKOUT_PREFIXES.iter().any(|p| name.starts_with(p)) && meta.is_dir() {
            let cutoff = now.checked_sub(policy.checkout_max_age).unwrap_or(now);
            if !modified_since(&item.path(), cutoff) && fs::remove_dir_all(item.path()).is_ok() {
                report.removed += 1;
            }
        } else if name.starts_with(WORK_PREFIX) {
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
    remove_orphaned_locks(&mut report, dir);
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

/// A legacy `<target>.lock` goes with its target directory: the file's own mtime is its creation,
/// which says nothing about whether the target is in use.
fn remove_orphaned_locks(report: &mut Report, dir: &Path) {
    let Ok(read) = fs::read_dir(dir) else { return };
    for item in read.flatten() {
        let name = item.file_name();
        let name = name.to_string_lossy();
        let Some(target) = name.strip_suffix(LOCK_SUFFIX) else {
            continue;
        };
        let legacy = CHECKOUT_PREFIXES.iter().any(|p| target.starts_with(p));
        if legacy && !dir.join(target).exists() && fs::remove_file(item.path()).is_ok() {
            report.removed += 1;
        }
    }
}
