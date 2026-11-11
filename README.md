# wasm2rs

[![Crates.io](https://img.shields.io/crates/v/wasm2rs.svg)](https://crates.io/crates/wasm2rs)
[![Documentation](https://docs.rs/wasm2rs/badge.svg)](https://docs.rs/wasm2rs)
[![build](https://github.com/hirosassa/wasm2rs/actions/workflows/test.yaml/badge.svg?branch=main)](https://github.com/hirosassa/wasm2rs/actions/workflows/test.yaml)
[![codecov](https://codecov.io/gh/hirosassa/wasm2rs/branch/main/graph/badge.svg)](https://codecov.io/gh/hirosassa/wasm2rs)

**wasm2rs** transpiles a WebAssembly binary into standalone, native Rust source code.

Instead of embedding a WebAssembly runtime and JIT-compiling a `.wasm` module at startup, wasm2rs turns the module into ordinary Rust that you compile with `rustc`/`cargo` like any other crate.
The generated code preserves WebAssembly semantics (wrapping arithmetic, linear-memory bounds checks, trap behavior) while running as plain native code — no interpreter, no runtime dependency.


## Features

- **Standalone Rust source**: the output is a normal crate with no runtime dependency on a WebAssembly engine.
- **Memory-efficient** on very large modules: output can be split across many files ([split output](docs/cli-reference.md#single-file-vs-split-output)).
- **Wide proposal coverage**: SIMD and relaxed SIMD, atomics, multi-memory, tail calls, exception handling, GC, stack-switching continuations.
- **WASI**: preview1 is implemented natively; wasi-threads and emscripten builds run on native threads ([docs/wasi-threads-e2e.md](docs/wasi-threads-e2e.md)).
- **Components (WASI 0.2)**: `--component` transpiles a `wasm32-wasip2` component against the typed `wasm2rs-wasi` host, including `wasi:http` ([docs/wasi-p2-status.md](docs/wasi-p2-status.md)).
- **Parallel code generation** with byte-identical output (`--jobs`).

## Installation

Prebuilt binaries for Linux, macOS and Windows are attached to each [GitHub Release](https://github.com/hirosassa/wasm2rs/releases), named `wasm2rs-<target-triple>.tar.gz` (`.zip` on Windows).

```bash
# macOS (Apple Silicon); for Linux use wasm2rs-x86_64-unknown-linux-gnu.tar.gz
curl -sSL https://github.com/hirosassa/wasm2rs/releases/latest/download/wasm2rs-aarch64-apple-darwin.tar.gz | tar xz
sudo mv wasm2rs /usr/local/bin/
```

Or build from source (Rust 2024 edition toolchain):

```bash
cargo install wasm2rs          # from crates.io
cargo install --path .         # from a local checkout
```

## Usage

```
wasm2rs [flags] <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]
wasm2rs --component [flags] <component.wasm> [output]
wasm2rs --dump <input.wasm>
```

| Argument | Description |
|---|---|
| `<input.wasm>` | Path to the input WebAssembly binary (required). |
| `[output]` | Output file, or output directory when splitting. Omit to write to stdout. |
| `[funcs_per_file]` | Max functions per chunk file. `0` (default) = unlimited. |
| `[max_bytes_per_file]` | Max bytes per chunk file, enforced at function boundaries. `0` (default) = unlimited. |

Main flags (all of them are described in [docs/cli-reference.md](docs/cli-reference.md)):

| Flag | Description |
|---|---|
| `--unsafe-memory` | Emit linear-memory accesses without bounds checks. Faster, but an out-of-bounds access becomes undefined behavior. Off by default. |
| `--jobs=N` | Threads used to generate function bodies; output is byte-identical for every value. `0` (default) = automatic. |
| `--entry-exports=a,b` | Emit only the functions reachable from the listed exports (plus the start function). |
| `--component` | Transpile a WebAssembly component (e.g. `wasm32-wasip2` output) in one step. |
| `--dump` | Print a summary of the module and generate no code. |

Also available: `--named-exports`, `--named-imports`, `--import-memory`, `--reentrant-imports`, `--split-dispatch`, `--extract-core`, `--wit-host`, `--glue`, `--external-types`.

### Examples

```bash
wasm2rs module.wasm module.rs                    # single Rust file
wasm2rs module.wasm out_dir 1                    # split: one function per file
wasm2rs module.wasm out_dir 4 102400             # split: at most 4 functions and 100 KB per file
wasm2rs --unsafe-memory module.wasm module.rs    # unchecked linear-memory access
wasm2rs --dump module.wasm                       # inspect without generating code
```

### Single file vs. split output

- **Single file**: when both `funcs_per_file` and `max_bytes_per_file` are `0` (the default), the module is emitted as one Rust source file (to `output`, or stdout).
- **Split output**: when either cap is positive, `output` is a directory holding a `lib.rs` crate root, `funcs_0.rs`, `funcs_1.rs`, … and a ready-to-use `Cargo.toml`. Build it with `cargo build --release` inside that directory.

## Library API

```rust
use wasm2rs::{TranspileOptions, transpile_with_options};

let wasm: Vec<u8> = std::fs::read("module.wasm")?;
let rust = transpile_with_options(&wasm, &TranspileOptions::default())?;
```

From a `build.rs` (`wasm2rs` as a build-dependency with `default-features = false`):

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

Or embed it in one line with the `wasm2rs-macros` crate:

```rust
mod foo {
    wasm2rs_macros::include_wasm!("assets/foo.wasm", named_exports);
}
```

`TranspileOptions`, split transpilation, components (`transpile_component`, `include_component!`) and the WIT API are covered in [docs/library-api.md](docs/library-api.md).

## Documentation

- [CLI reference](docs/cli-reference.md): every argument and flag
- [Library API](docs/library-api.md): `TranspileOptions`, split API, `build.rs`, proc macros, components, WIT API
- [WASI p2 status](docs/wasi-p2-status.md), [typed host design](docs/typed-host-design.md), [`wasm2rs-wasi` design](docs/wasm2rs-wasi-design.md), [`wasi:http` design](docs/wasi-http-design.md)
- [Component input](docs/component-input-design.md), [component exports](docs/component-exports-design.md), [component model PoC](docs/component-model-poc.md)
- [wasi-threads end-to-end tests](docs/wasi-threads-e2e.md)
- [Differential fuzzing](docs/differential-fuzzing.md), [HTTP fuzzing server](docs/fuzzing-http-server.md), [bounds-check benchmark](docs/bounds-check-bench.md)
- [CONTRIBUTING](CONTRIBUTING.md) and [CHANGELOG](CHANGELOG.md)

## Testing

```bash
cargo test --release
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for running subsets and the optional end-to-end tools.

## License

Licensed under the [MIT License](LICENSE).
