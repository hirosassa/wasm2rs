# wasm2rs-macros

[![Crates.io](https://img.shields.io/crates/v/wasm2rs-macros.svg)](https://crates.io/crates/wasm2rs-macros)
[![Documentation](https://docs.rs/wasm2rs-macros/badge.svg)](https://docs.rs/wasm2rs-macros)

Procedural macros that embed a WebAssembly module as native Rust source at compile time, using [wasm2rs](https://crates.io/crates/wasm2rs).

`include_wasm!` reads a `.wasm` file, transpiles it with wasm2rs and expands to the generated items. There is no build script and no WebAssembly runtime: the module is compiled by `rustc` like the rest of your crate.

## Usage

```toml
[dependencies]
wasm2rs-macros = "0.4"
```

```rust
mod foo {
    wasm2rs_macros::include_wasm!("assets/foo.wasm", named_exports);
}

fn main() {
    // `named_exports` adds a function named after each export.
    println!("{}", foo::add(40, 2));
}
```

The invocation expands to the generated items, so wrap it in a `mod` to scope them. The `.wasm` file is tracked, so the macro re-expands when it changes.

## Syntax

```rust
wasm2rs_macros::include_wasm!("path.wasm" [, option]*);
```

- **Path**: a plain or raw string literal, relative to the crate's `CARGO_MANIFEST_DIR`. A leading `$OUT_DIR/` refers to the `OUT_DIR` of the crate being compiled, for a wasm file that a build script produced.
- **Options**: any of the following, with the meaning of the CLI flag of the same name (`named_exports` ↔ `--named-exports`). An invalid combination is a compile error.

| Option | Effect |
|---|---|
| `named_exports` | Emit a method or function named after each exported function, e.g. `add(1, 2)`. |
| `unsafe_memory` | Emit linear-memory accesses without bounds checks. Faster, but an out-of-bounds access is undefined behavior. |
| `named_imports` | Name the host `Imports` trait methods after the imports (`env_log`) instead of `import{j}`. |
| `import_memory` | Make the module's memory host-owned, lent through `Imports::memory`/`memory_mut`. |
| `reentrant_imports` | Let the host call back into the guest while an import runs. |

See the [wasm2rs README](https://github.com/hirosassa/wasm2rs#readme) for details on each option, the generated API, WASI support and supported proposals.

## Components

`include_component!("name")` includes the Rust that a build script wrote for a WebAssembly *component* with `wasm2rs::build::transpile_component_to_out_dir("assets/name.wasm", &options)`. It expands to `include!` of `$OUT_DIR/name.rs`; the name is the input file's name without the `.wasm` extension (`include_component!("name.wasm")` is a compile error). Unlike `include_wasm!`, the transpiling happens in the build script, not in the macro.

## When not to use it

The generated source is parsed into a token stream at compile time, which costs time and memory on a very large module. For those, use a build script (`wasm2rs::build`) with `include!` instead.

## License

Licensed under the [MIT License](https://github.com/hirosassa/wasm2rs/blob/main/LICENSE).
