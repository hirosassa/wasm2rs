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
- **Wide proposal coverage**: SIMD and relaxed SIMD, atomics and shared memory, multi-memory, tail calls, exception handling (including legacy), GC, stack-switching continuations.
- **WASI**: preview1 is implemented natively, other imports go through a generated host `Imports` trait; wasi-threads and emscripten builds run on native threads ([docs/wasi-threads-e2e.md](docs/wasi-threads-e2e.md)). The preview1 preopen check is check-then-use and can be raced by a sibling wasi-threads thread or a host process that swaps in a symlink; see the known limitation in [docs/cli-reference.md](docs/cli-reference.md).
- **Components (WASI 0.2)**: `--component` transpiles a component (Rust, Python, JS; preview1-adapter components too) against the typed `wasm2rs-wasi` host, including `wasi:http` ([verified toolchains](#components), [docs/wasi-p2-status.md](docs/wasi-p2-status.md)).
- **Parallel code generation** with byte-identical output (`--jobs`).

## Installation

Prebuilt binaries for Linux, macOS and Windows are attached to each [GitHub Release](https://github.com/hirosassa/wasm2rs/releases), named `wasm2rs-<target-triple>.tar.gz` (`.zip` on Windows). Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`.

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
| `--simd-intrinsics` | Emit `core::arch` (NEON/SSE2) bodies for the integer SIMD lane ops (add/sub/mul/neg/abs, saturating add/sub, `avgr_u`, integer compares); other targets keep the scalar loop. Bit-identical results, but the generated code contains `unsafe`. Off by default. |
| `--unsafe-memory` | Emit linear-memory accesses without bounds checks. Faster, but an out-of-bounds access becomes undefined behavior. Off by default. |
| `--jobs=N` | Threads used to generate function bodies; output is byte-identical for every value. `0` (default) = automatic. |
| `--entry-exports=a,b` | Emit only the functions reachable from the listed exports (plus the start function). |
| `--component` | Transpile a WebAssembly component (e.g. `wasm32-wasip2` output) in one step. |
| `--dump` | Print a summary of the module and generate no code. |

Also available: `--named-exports`, `--named-imports`, `--import-memory`, `--reentrant-imports`, `--split-dispatch`, `--call-depth-limit=N`, `--extract-core`, `--wit-host`, `--glue`, `--external-types`.

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

## Components

`--component` takes a WebAssembly component (WASI 0.2) and writes one Rust source: the component's core modules linked into one `Instance`, plus a `Glue` that connects them to the typed `wasm2rs-wasi` host and an `Exports` trait for the lifted exports. Build the result with `rustc` or `cargo` against the `wasm2rs-wasi` crate.

### Verified toolchains

Each toolchain was run end to end (`transpile_component`, `rustc`, run) with `examples/wasip2_run` (`wasi:cli/run`) and, where the table says so, `examples/wasip2_serve` (`wasi:http/incoming-handler`). Details, timings and reproduction commands are in [docs/component-toolchain-survey.md](docs/component-toolchain-survey.md).

| Toolchain | `wasi:cli/run` | `wasi:http` | Notes |
|---|---|---|---|
| Rust `wasm32-wasip2` | works | works | About 16 s to build and run a hello. |
| Rust `wasm32-wasip1` + preview1 adapter | works | not tried | The adapter is linked into the same `Instance`. |
| Python (`componentize-py`, CPython as 14 dynamically linked core modules) | works | works | `-C opt-level=1` takes about 5.5 minutes and 7.5 GB; `-C opt-level=0` serves in about 75 s. |
| JS (`jco componentize`) | works | works | `-C opt-level=0` only (about 90 s). At `-C opt-level=1` LLVM did not finish in 80 minutes (#331). |
| Go (`GOOS=wasip1` + adapter) | only with an empty environment | not tried | The adapter's pages overlap Go's heap when the environment is inherited; wasmtime fails the same way (#311). |

TinyGo, `cargo-component` with a custom world and wasi-sdk C/C++ components are untried.

### Usage

```bash
rustc --target wasm32-wasip2 -O hello.rs -o hello.wasm
wasm2rs --component hello.wasm hello_component.rs   # one Rust source (stdout if omitted)

cargo run --release --example wasip2_run -- hello.wasm                # transpile, build and run wasi:cli/run
cargo run --release --example wasip2_serve -- handler.wasm --listen 127.0.0.1:8080   # serve wasi:http/incoming-handler
```

The examples build the component with `rustc -C opt-level=1`; set `WASM2RS_EXAMPLE_OPT_LEVEL=0` to change the level. From Rust code use `wasm2rs::transpile_component`, `wasm2rs::build::transpile_component_to_out_dir` and `wasm2rs_macros::include_component!` ([docs/library-api.md](docs/library-api.md)).

### Known limits

- **Large components: start at `opt-level=0`.** The JS and Python components produce 125 MB and 122 MB of Rust, and LLVM time is the bottleneck, not wasm2rs (about 1 s to transpile Python).
- **One source only.** `--component` refuses the split arguments (`funcs_per_file`, `max_bytes_per_file`); the other generated-code flags that it supports are `--unsafe-memory`, `--simd-intrinsics`, `--jobs`, `--split-dispatch`, `--call-depth-limit` and `--external-types`.
- **Linked core modules (adapters, dylink libraries) may only use MVP, sign-extension, saturating float-to-int, bulk-memory and reference-types operators.** Any other operator (SIMD, atomics, exceptions, ...) is refused with an error naming it; lifts that allocate through different `realloc` functions of several modules are also refused (#314).
- `wasip2_serve` serves one request at a time over HTTP/1.x (listening on `127.0.0.1:8080` by default); `wasip2_run` and `wasip2_serve` have no outgoing HTTP client.

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
- [Component toolchain survey](docs/component-toolchain-survey.md): which real components run, with timings and reproduction commands
- [WASI p2 status](docs/wasi-p2-status.md), [typed host design](docs/typed-host-design.md), [`wasm2rs-wasi` design](docs/wasm2rs-wasi-design.md), [`wasi:http` design](docs/wasi-http-design.md)
- [Component input](docs/component-input-design.md), [component exports](docs/component-exports-design.md), [component model PoC](docs/component-model-poc.md)
- [Core module linking](docs/core-module-linking-design.md), [dynamic linking (`componentize-py`)](docs/f2-dynamic-linking-design.md), [`rustc` time on large components](docs/f4-rustc-time-spike.md)
- [wasi-threads end-to-end tests](docs/wasi-threads-e2e.md)
- [Differential fuzzing](docs/differential-fuzzing.md), [HTTP fuzzing server](docs/fuzzing-http-server.md), [bounds-check benchmark](docs/bounds-check-bench.md)
- [CONTRIBUTING](CONTRIBUTING.md) and [CHANGELOG](CHANGELOG.md)

## Testing

```bash
cargo test --release --workspace
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for running subsets and the optional end-to-end tools.

## License

Licensed under the [MIT License](LICENSE).
