# Library API

wasm2rs is also usable as a library. For a minimal example see the [README](../README.md#library-api).

## Transpiling a module

```rust
use wasm2rs::{TranspileOptions, transpile_with_options};

let wasm: Vec<u8> = std::fs::read("module.wasm")?;
let rust = transpile_with_options(&wasm, &TranspileOptions::default())?;
```

`wasm2rs::dump(&wasm)` returns the same summary as `--dump` (a `ModuleSummary` that implements `Display`).
`wasm2rs::extract_core_modules(&component)` returns a component's core modules as `Vec<Vec<u8>>`.
`wasm2rs::transpile_component(&component, &ComponentOptions)` turns a component into one Rust source in one call: the program core module, the typed import `Glue` and the `Exports` trait. `ComponentOptions { transpile: TranspileOptions, external_types: Option<String> (default `Some("wasm2rs_wasi")`), exports: bool (default `true`) }`; `named_imports`, `named_exports` and `reentrant_imports` are forced on in `transpile`, because the glue needs them. The program module is the one `wit_exports` says lifts the exports; a component without exports uses the module instantiated with the lowered imports. A component whose core modules are wired to each other (a preview1 adapter with `wit-component`'s shim and fixup, exports lifted from several modules, a dylink-style component such as `componentize-py`'s) is transpiled as one program: its modules are linked into a single `Instance` and the adapter's calls become `Host` methods of the WIT functions they lower; every other component keeps the single-module path with unchanged output. A core module input, a component with no core module or an undecidable choice (an export-less component with several candidate modules) are `InvalidInput` errors saying why; lifts that allocate through different `realloc` functions of several linked modules are `Unsupported` (#314). `transpile_component_with_exports` also returns the lifted `Vec<WitExport>`. See [component-input-design.md](component-input-design.md).
`wasm2rs::wit_host_trait(&component)` returns the Rust source of the typed `Host` trait that `--wit-host` writes, `wasm2rs::wit_host_glue(&component)` that text plus the `Glue` of `--wit-host --glue`, and `wasm2rs::wit_host_trait_with_options` / `wit_host_glue_with_options` take a `WitOptions { naming: WitNaming::Default | Qualified, external_types: Option<String>, default_trap: bool, exports: bool }` (see `--external-types`; the plain functions are the default options; `default_trap` gives every method of the generated `Host` trait a default body that panics naming the method), and `wasm2rs::wit_functions(&component)` the decoded `Vec<WitFunc>` (interface, function name, parameter and result `WitType`s, with `WitType::Own`/`Borrow` of a `WitResource`; every record, variant, enum, flags and resource records the `interface` that defines it; one synthetic `[resource-drop]<r>` function per defined resource whose `resource.drop` the component lowers; every drop if the component has no top-level core module or has a nested component that lowers a drop) behind it. `WitOptions::exports` (library only, no standalone CLI flag; `--component` turns it on; glue only, off by default so the output is unchanged) adds the typed export side to the glue: `pub trait Exports` with one method per exported function, named like the `--named-exports` method of the core export it lifts (so `wasi:cli/run@0.2.0#run` is `export_wasi_cli_run_0_2_0_run(&mut self) -> Result<(), ()>`), and `impl<H: Host> Exports for Instance<Glue<H>>`. The host lowers the arguments (flat, or one record in guest memory past 16 flat values; a `string`/`list` is written into memory from the lift's `cabi_realloc`) and lifts the result under the same checks and per-call budget as the import glue, then calls the lift's `post-return` with the core result. A guest trap stays a panic. Call it as `Exports::name(&mut inst, ..)`, because the transpiled `Instance` has an inherent method of the same name (the flat core export) that `inst.name(..)` would pick. Needs the same shapes as `wit_exports`; a `realloc` other than `cabi_realloc` (when the function allocates; the linked path of `transpile_component` names the lifts' realloc `cabi_realloc` itself) and a method name that depends on the other exports are `Unsupported` errors; transpile the core module with `--named-exports` (a module without the method fails to build). `wasm2rs::wit_exports(&component)` is the export side: one `WitExport` per exported function (`func`, the `core_module` index among the top-level core modules, the `core_export` name, and the `memory`, `realloc` and `post_return` export names of its `canon lift`, and its string `encoding`: `WitStringEncoding::Utf8` or `Utf16`). Only a lift that is a direct export of an instantiated top-level core module with UTF-8 or UTF-16 strings is read (the glue transcodes a UTF-16 string to and from a Rust `String`; an unpaired surrogate traps); a shim or adapter instance, a `canon lower`, `compact-utf16` or a nested component's export is an `Unsupported` error naming the export. An import lowered with UTF-16 strings is served the same way (the glue reads what the guest passes as UTF-16 and writes what the host returns as code units); one lowered with `compact-utf16` or with both UTF-8 and UTF-16, with a string in its signature, is `Unsupported`.

Code generation is configured through `TranspileOptions` (`unsafe_memory`, `simd_intrinsics`, `call_depth_limit`, `typed_traps`, `split_dispatch`, `entry_exports`, `jobs`, `named_exports`, `named_imports`, `import_memory`, `reentrant_imports`). It is `#[non_exhaustive]`, so build it from `TranspileOptions::default()` and either assign fields or chain the `with_*` methods (`TranspileOptions::default().with_named_exports(true)`); a struct literal only compiles inside the crate. It is `Clone` and `Default` but not `Copy`, so pass it by reference. With `typed_traps`, the five runtime-helper traps (`unreachable`, indirect-call type mismatch, call stack exhausted, the two float-to-integer conversions) unwind with a `Wasm2RsTrap` payload that a host downcasts from the `catch_unwind` result instead of matching a message; `resume_unwind` does not call the panic hook, so they print nothing to stderr, and every other trap is still a string panic.
`transpile(&wasm)` is the shortcut for the default options.

Streaming/split transpilation is available via `transpile_split` / `transpile_split_with_options`, which take a `SplitOptions` (`SplitOptions::single_file()` for one file) and call a sink with each `SourceFile`. Errors are reported as `TranspileError`. `cargo_manifest` produces the recommended `Cargo.toml` for split output.

## Using from `build.rs`

Add `wasm2rs` as a build-dependency (`default-features = false`), transpile in `build.rs`, and `include!` the result:

```rust
// build.rs
fn main() {
    wasm2rs::build::transpile_to_out_dir("assets/foo.wasm", "foo.rs").unwrap();
}

// src/lib.rs
mod foo {
    include!(concat!(env!("OUT_DIR"), "/foo.rs"));
}
```

`transpile_to_out_dir` also prints `cargo:rerun-if-changed` for the wasm file. `transpile_to_out_dir_with_options` takes a `TranspileOptions` (e.g. `named_exports`), and `transpile_to_dir` writes to an explicit directory.

A component (`wasm32-wasip2`) goes through `transpile_component_to_out_dir`, which writes `$OUT_DIR/{file stem}.rs` (the glue, the `Exports` trait and the program module, see `transpile_component`); `wasm2rs_macros::include_component!` includes it:

```rust
// build.rs
fn main() {
    wasm2rs::build::transpile_component_to_out_dir(
        "assets/run.wasm",
        &wasm2rs::ComponentOptions::default(),
    )
    .unwrap();
}

// src/lib.rs (depends on wasm2rs-wasi and wasm2rs-macros)
mod run {
    wasm2rs_macros::include_component!("run");
}
```

Pass the name *without* the `.wasm` extension: `include_component!("run.wasm")` is a `compile_error!` that says so (the file is named after the input's file stem, `run.rs`). A dot inside the name (`include_component!("foo.bar")` for `assets/foo.bar.wasm`) is fine.

## Using the proc macro

For a one-line embed without a build script, use the `wasm2rs-macros` crate:

```rust
mod foo {
    wasm2rs_macros::include_wasm!("assets/foo.wasm", named_exports);
}
```

The path (a plain or raw string literal) is relative to `CARGO_MANIFEST_DIR` (a leading `$OUT_DIR/` refers to a file a build script wrote). The options are `named_exports`, `unsafe_memory`, `simd_intrinsics`, `named_imports`, `import_memory` and `reentrant_imports`, with the meaning of the CLI flags of the same name (an invalid combination is a compile error). The generated source is parsed into tokens at compile time, so for very large modules prefer `build.rs` with `include!`.

The `mimalloc` allocator is behind the default `cli` feature. Depend on the crate with `default-features = false` to use it as a library without compiling mimalloc.
