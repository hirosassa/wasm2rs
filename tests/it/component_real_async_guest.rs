//! A Rust async guest (`wit-bindgen` 0.63, `wasm32-wasip2`) transpiled without being run: the
//! WASI 0.3 M1 read model and glue. The file is not in the repository: see the guest in
//! `docs/wasi-p3-spike.md`.

use wasm2rs::{ComponentOptions, TranspileError, transpile_component_with_exports, wit_exports};

/// Set `WASM2RS_P3_ASYNC_GUEST` to the spike guest component and run with `--ignored`.
#[test]
#[ignore = "needs WASM2RS_P3_ASYNC_GUEST (the wit-bindgen async spike guest component)"]
fn the_spike_async_guest_is_refused_for_its_builtin_imports() {
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
    // Its own `Host` (no external types): the guest imports functions that return a stream and a
    // future, and the library has no handle types for them.
    let options = ComponentOptions {
        external_types: None,
        ..ComponentOptions::default()
    };
    // Its core module imports the async built-ins (`task.return`, `waitable-set.*`, ...): the
    // glue cannot serve them before the M2 runtime, so the transpile names the feature.
    let err = transpile_component_with_exports(&component, &options)
        .expect_err("the built-in imports are refused");
    assert!(matches!(err, TranspileError::Unsupported(_)), "{err:?}");
    assert!(err.to_string().contains("canonical built-in"), "{err}");
}
