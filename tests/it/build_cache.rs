//! Behavioural tests for the generated-code build cache in `common::build`.
//!
//! The transpiler emits deterministic Rust for a given module, so re-running the
//! suite recompiles byte-identical programs. `build` therefore memoizes compiled
//! binaries by content hash; these tests pin that a repeated build is a genuine
//! cache hit (no recompile) and still yields a runnable binary.
use crate::common;

/// What tells a recompile from a hit: a recompile publishes a new file, so its inode differs. (The
/// mtime cannot: a hit refreshes it, so that pruning sees the entry as in use.)
#[cfg(unix)]
fn file_identity(path: &std::path::Path) -> u64 {
    std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(path).expect("binary metadata"))
}

#[cfg(not(unix))]
fn file_identity(path: &std::path::Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.created())
        .expect("binary creation time")
}

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
    let id1 = file_identity(&first);

    let second = common::build("cache_hit_b", program);
    let id2 = file_identity(&second);

    assert_eq!(
        first, second,
        "identical programs must map to the same cached binary path"
    );
    assert_eq!(
        id1, id2,
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

/// CI points the cache at a directory it persists between runs; locally it stays in the temp dir.
#[test]
fn cache_dir_honours_override_and_defaults_to_temp_dir() {
    let over = std::path::Path::new("/ci/persisted/testcache");
    assert_eq!(common::resolve_cache_dir(Some(over.as_os_str())), over);
    assert_eq!(
        common::resolve_cache_dir(None),
        std::env::temp_dir().join("wasm2rs-testcache")
    );
    // An empty variable (unset-by-assignment in a shell or workflow) must not mean "cwd".
    assert_eq!(
        common::resolve_cache_dir(Some(std::ffi::OsStr::new(""))),
        std::env::temp_dir().join("wasm2rs-testcache")
    );
}

/// A cache restored from an older generator must never serve a stale binary: the key is derived
/// from the generated program, so a changed emission maps to a different entry.
#[test]
fn changed_generator_output_gets_a_new_cache_entry() {
    let wat =
        r#"(module (memory 1) (func (export "f") (param i32) (result i32) local.get 0 i32.load))"#;
    let wasm = wat::parse_str(wat).expect("valid wat");
    let mut opts = wasm2rs::TranspileOptions::default();
    let a = wasm2rs::transpile_with_options(&wasm, &opts).expect("transpile");
    opts.unsafe_memory = true;
    let b = wasm2rs::transpile_with_options(&wasm, &opts).expect("transpile");
    assert_ne!(a, b, "precondition: the option changes the emitted source");
    let pa = common::build("gen_key_a", &format!("{a}\nfn main() {{}}\n"));
    let pb = common::build("gen_key_b", &format!("{b}\nfn main() {{}}\n"));
    assert_ne!(pa, pb);
}
