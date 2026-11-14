//! Behavioural tests for the shared test cache's pruning (`common::prune`).
//!
//! The cache under `$TMPDIR/wasm2rs-testcache` is content-addressed and never shrinks on its
//! own; pruning drops what has not been used lately (cache hits refresh the mtime), then the
//! least recently used entries beyond the size cap, and the leftovers of builds that died.
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::common::prune::{Policy, prune};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wasm2rs-prune-test-{}-{tag}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Creates `name` with `len` bytes whose mtime is `age` before `now`.
fn entry(dir: &Path, name: &str, len: usize, now: SystemTime, age: Duration) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, vec![0u8; len]).unwrap();
    set_age(&path, now, age);
    path
}

fn set_age(path: &Path, now: SystemTime, age: Duration) {
    let file = File::options()
        .write(true)
        .open(path)
        .unwrap_or_else(|_| File::open(path).unwrap());
    file.set_times(FileTimes::new().set_modified(now - age))
        .unwrap();
}

fn policy() -> Policy {
    Policy {
        max_age: 14 * DAY,
        max_bytes: u64::MAX,
        work_max_age: Duration::from_secs(6 * 60 * 60),
        checkout_max_age: 14 * DAY,
    }
}

#[test]
fn entries_older_than_the_max_age_are_removed_and_fresh_ones_kept() {
    let dir = scratch("age");
    let now = SystemTime::now();
    let old = entry(&dir, "gen-0000000000000001", 10, now, 15 * DAY);
    let old_example = entry(&dir, "ex-0000000000000002", 10, now, 30 * DAY);
    let fresh = entry(&dir, "gen-0000000000000003", 10, now, DAY);
    prune(&dir, now, &policy());
    assert!(!old.exists());
    assert!(!old_example.exists());
    assert!(fresh.exists());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_least_recently_used_entries_go_first_when_over_the_size_cap() {
    let dir = scratch("cap");
    let now = SystemTime::now();
    let oldest = entry(&dir, "gen-a", 100, now, 3 * DAY);
    let middle = entry(&dir, "ex-b", 100, now, 2 * DAY);
    let newest = entry(&dir, "gen-c", 100, now, DAY);
    let report = prune(
        &dir,
        now,
        &Policy {
            max_bytes: 200,
            ..policy()
        },
    );
    assert!(!oldest.exists());
    assert!(middle.exists());
    assert!(newest.exists());
    assert_eq!(report.removed, 1);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn stale_work_dirs_are_removed_but_a_live_build_is_not() {
    let dir = scratch("work");
    let now = SystemTime::now();
    let stale = dir.join("work-x-1-0");
    fs::create_dir_all(stale.join("sub")).unwrap();
    fs::write(stale.join("sub/gen.rs"), "fn main() {}").unwrap();
    set_age(&stale, now, DAY);
    let live = dir.join("work-y-2-0");
    fs::create_dir_all(&live).unwrap();
    set_age(&live, now, Duration::from_secs(60));
    prune(&dir, now, &policy());
    assert!(!stale.exists());
    assert!(live.exists());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn files_that_are_not_cache_entries_are_left_alone() {
    let dir = scratch("other");
    let now = SystemTime::now();
    let rlib = entry(
        &dir,
        "libwasm2rs_wasi-0000000000000001.rlib",
        10,
        now,
        100 * DAY,
    );
    let stamp = entry(&dir, ".pruned", 0, now, 100 * DAY);
    prune(&dir, now, &policy());
    assert!(rlib.exists());
    assert!(stamp.exists());
    fs::remove_dir_all(&dir).ok();
}

/// Creates the per-checkout directory `name` holding a file of the given age, nested so that the
/// directory's own mtime says nothing about the use of what is inside.
fn checkout(dir: &Path, name: &str, now: SystemTime, inner_age: Duration) -> PathBuf {
    let path = dir.join(name);
    fs::create_dir_all(path.join("wasi-target/debug")).unwrap();
    entry(
        &path.join("wasi-target/debug"),
        "libx.rlib",
        10,
        now,
        inner_age,
    );
    // creating the file touched the directories above it
    for sub in ["wasi-target/debug", "wasi-target", ""] {
        set_age(&path.join(sub), now, 100 * DAY);
    }
    path
}

#[test]
fn an_idle_checkout_dir_is_removed_and_a_used_one_is_kept() {
    let dir = scratch("checkout");
    let now = SystemTime::now();
    let idle = checkout(&dir, "checkout-0000000000000001", now, 15 * DAY);
    let used = checkout(&dir, "checkout-0000000000000002", now, DAY);
    let report = prune(&dir, now, &policy());
    assert!(!idle.exists());
    assert!(used.exists());
    assert_eq!(report.removed, 1);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn one_recent_file_deep_inside_keeps_the_whole_checkout_dir() {
    let dir = scratch("checkout-newest");
    let now = SystemTime::now();
    let path = checkout(&dir, "checkout-0000000000000003", now, 30 * DAY);
    entry(&path, ".used", 0, now, Duration::from_secs(60));
    prune(&dir, now, &policy());
    assert!(path.join("wasi-target/debug/libx.rlib").exists());
    fs::remove_dir_all(&dir).ok();
}

/// A cache restored from before the per-checkout layout still holds these at the top level.
#[test]
fn legacy_per_checkout_entries_are_aged_out_by_their_newest_inner_mtime() {
    let dir = scratch("legacy");
    let now = SystemTime::now();
    let idle = checkout(&dir, "wasi-target-00000000000000aa", now, 20 * DAY);
    let idle_example = checkout(&dir, "example-wasi-target-00000000000000aa", now, 20 * DAY);
    let used = checkout(&dir, "wasi-target-00000000000000bb", now, DAY);
    let idle_lock = entry(&dir, "wasi-target-00000000000000aa.lock", 0, now, 20 * DAY);
    let used_lock = entry(&dir, "wasi-target-00000000000000bb.lock", 0, now, DAY);
    prune(&dir, now, &policy());
    assert!(!idle.exists());
    assert!(!idle_example.exists());
    assert!(!idle_lock.exists());
    assert!(used.exists());
    assert!(used_lock.exists());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn pruning_a_missing_directory_is_a_no_op() {
    let dir = std::env::temp_dir().join("wasm2rs-prune-test-does-not-exist");
    fs::remove_dir_all(&dir).ok();
    assert_eq!(prune(&dir, SystemTime::now(), &policy()).removed, 0);
}
