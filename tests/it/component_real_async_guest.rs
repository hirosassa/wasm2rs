//! A Rust async guest (`wit-bindgen` 0.63, `wasm32-wasip2`) transpiled without being run: the
//! WASI 0.3 M1 read model and glue. The file is not in the repository: see the guest in
//! `docs/wasi-p3-spike.md`.

use wasm2rs::{ComponentOptions, transpile_component_with_exports, wit_exports};

/// Set `WASM2RS_P3_ASYNC_GUEST` to the spike guest component and run with `--ignored`.
#[test]
#[ignore = "needs WASM2RS_P3_ASYNC_GUEST (the wit-bindgen async spike guest component)"]
fn the_spike_async_guest_transpiles_to_typed_callback_glue() {
    let path = std::env::var_os("WASM2RS_P3_ASYNC_GUEST")
        .expect("WASM2RS_P3_ASYNC_GUEST names the component");
    let component = std::fs::read(path).unwrap();
    let exports = wit_exports(&component).unwrap();
    let async_exports: Vec<_> = exports
        .iter()
        .filter(|e| e.async_abi.is_some())
        .map(|e| e.func.name.as_str())
        .collect();
    assert_eq!(
        async_exports,
        ["double-slow", "sum-stream", "await-future"],
        "the three async exports are read as callback lifts"
    );
    let (code, _) =
        transpile_component_with_exports(&component, &ComponentOptions::default()).unwrap();
    for needle in [
        "pub enum CallbackCode",
        "fn double_slow_start(&mut self, n: u32) -> CallbackCode;",
        "fn double_slow(&mut self, n: u32, events: &mut dyn FnMut(CallbackCode) -> Option<[u32; 3]>) -> u32;",
        "fn sum_stream_start(&mut self, s: WitStream<u32>) -> CallbackCode;",
        "fn await_future_start(&mut self, f: WitFuture<u32>) -> CallbackCode;",
        // The async-lowered import completes through the host: status RETURNED.
        "fn test_p3_src_slow(&mut self, g: &mut Instance<Self>",
    ] {
        assert!(code.contains(needle), "{needle}");
    }
}
