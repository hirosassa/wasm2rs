//! Exit status 0 when the module (path given, or the file named by
//! `$WASM_SHRINK_INPUT`... here simply argv[1]) makes wasmtime and wasm2rs disagree.
//! Meant as the predicate for `wasm-tools shrink`.
use wasm2rs_fuzz_diff::{Verdict, differential};

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let wasm = std::fs::read(path).expect("read");
    let work = std::env::temp_dir().join(format!("wasm2rs-fuzz-shrink-{}", std::process::id()));
    let verdict = differential(&wasm, &work);
    std::fs::remove_dir_all(&work).ok();
    let hit = matches!(verdict, Verdict::Mismatch { .. } | Verdict::Broken(_));
    std::process::exit(if hit { 0 } else { 1 });
}
