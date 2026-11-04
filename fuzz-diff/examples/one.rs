//! `cargo run --release --example one -- <file.wasm>`: show both reports for one module.
fn main() {
    let path = std::env::args().nth(1).expect("path");
    let wasm = std::fs::read(&path).expect("read");
    let (exports, mem, _) = wasm2rs_fuzz_diff::inspect(&wasm).expect("inspect");
    println!(
        "--- wasmtime\n{:?}",
        wasm2rs_fuzz_diff::run_wasmtime(&wasm, &exports, mem.as_deref())
    );
    let work = std::env::temp_dir().join(format!("wasm2rs-fuzz-one-{}", std::process::id()));
    println!(
        "--- wasm2rs\n{:?}",
        wasm2rs_fuzz_diff::run_wasm2rs(&wasm, &exports, mem.is_some(), &work)
    );
}
