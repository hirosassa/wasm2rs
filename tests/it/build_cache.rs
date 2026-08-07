//! Behavioural tests for the generated-code build cache in `common::build`.
//!
//! The transpiler emits deterministic Rust for a given module, so re-running the
//! suite recompiles byte-identical programs. `build` therefore memoizes compiled
//! binaries by content hash; these tests pin that a repeated build is a genuine
//! cache hit (no recompile) and still yields a runnable binary.
use crate::common;

/// A second build of a byte-identical program must reuse the cached binary
/// rather than recompiling: the returned path is stable and its mtime is
/// unchanged across the two calls.
#[test]
fn identical_program_hits_cache() {
    let program = "\
pub fn answer() -> i32 { 42 }
fn main() { assert_eq!(answer(), 42); }
";
    let first = common::build("cache_hit_a", program);
    let m1 = std::fs::metadata(&first)
        .and_then(|m| m.modified())
        .expect("first binary mtime");

    let second = common::build("cache_hit_b", program);
    let m2 = std::fs::metadata(&second)
        .and_then(|m| m.modified())
        .expect("second binary mtime");

    assert_eq!(
        first, second,
        "identical programs must map to the same cached binary path"
    );
    assert_eq!(
        m1, m2,
        "second build of an identical program must be a cache hit (no recompile)"
    );
    assert!(
        std::process::Command::new(&second)
            .status()
            .expect("run cached binary")
            .success(),
        "cached binary must still run successfully"
    );
}

/// Distinct programs must not collide in the cache.
#[test]
fn distinct_programs_get_distinct_binaries() {
    let a = "fn main() { assert_eq!(1 + 1, 2); }\n";
    let b = "fn main() { assert_eq!(2 + 2, 4); }\n";
    let pa = common::build("cache_distinct_a", a);
    let pb = common::build("cache_distinct_b", b);
    assert_ne!(
        pa, pb,
        "different programs must map to different cache keys"
    );
}
