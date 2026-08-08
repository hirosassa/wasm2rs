# wasm2rs

[![Crates.io](https://img.shields.io/crates/v/wasm2rs.svg)](https://crates.io/crates/wasm2rs)
[![Documentation](https://docs.rs/wasm2rs/badge.svg)](https://docs.rs/wasm2rs)
[![build](https://github.com/hirosassa/wasm2rs/actions/workflows/test.yaml/badge.svg?branch=main)](https://github.com/hirosassa/wasm2rs/actions/workflows/test.yaml)
[![codecov](https://codecov.io/gh/hirosassa/wasm2rs/branch/main/graph/badge.svg)](https://codecov.io/gh/hirosassa/wasm2rs)

**wasm2rs** transpiles a WebAssembly binary into standalone, native Rust source code.

Instead of embedding a WebAssembly runtime and JIT-compiling a `.wasm` module at startup, wasm2rs turns the module into ordinary Rust that you compile with `rustc`/`cargo` like any other crate.
The generated code preserves WebAssembly semantics (wrapping arithmetic, linear-memory bounds checks, trap behavior) while running as plain native code — no interpreter, no runtime dependency.

## Features

- Emits **standalone Rust source** — the output is a normal crate with no runtime dependency on a WebAssembly engine.
- **Memory-efficient** for very large modules: output can be split across many files so no single file (and no peak allocation) balloons.
  The binary uses `mimalloc` to keep peak RSS down while transpiling gigabyte-scale output.
- Wide **WebAssembly proposal coverage**, including advanced-phase features.

## Installation

### Download a prebuilt binary

Prebuilt binaries for Linux, macOS, and Windows are attached to each [GitHub Release](https://github.com/hirosassa/wasm2rs/releases).
Pick the archive for your platform, extract it, and put the `wasm2rs` binary somewhere on your `PATH`.

```bash
# Linux (x86_64) — adjust the target and version to match your platform / the latest release
curl -sSL https://github.com/hirosassa/wasm2rs/releases/latest/download/wasm2rs-x86_64-unknown-linux-gnu.tar.gz \
  | tar xz
sudo mv wasm2rs /usr/local/bin/

# macOS (Apple Silicon)
curl -sSL https://github.com/hirosassa/wasm2rs/releases/latest/download/wasm2rs-aarch64-apple-darwin.tar.gz \
  | tar xz
sudo mv wasm2rs /usr/local/bin/
```

Available targets:

| Platform | Archive |
|---|---|
| Linux x86_64 | `wasm2rs-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `wasm2rs-aarch64-unknown-linux-gnu.tar.gz` |
| macOS x86_64 | `wasm2rs-x86_64-apple-darwin.tar.gz` |
| macOS aarch64 | `wasm2rs-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `wasm2rs-x86_64-pc-windows-msvc.zip` |

### Build from source

Requires a Rust toolchain (2024 edition).

```bash
# Install from a local checkout
cargo install --path .

# Or install the published crate from crates.io
cargo install wasm2rs

# Or just build
cargo build --release
```

## Usage

```
wasm2rs [--unsafe-memory] <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]
```

| Argument | Description |
|---|---|
| `<input.wasm>` | Path to the input WebAssembly binary (required). |
| `[output]` | Output file, or output directory when splitting. Omit to write to stdout. |
| `[funcs_per_file]` | Max functions per chunk file. `0` (default) = unlimited. |
| `[max_bytes_per_file]` | Max bytes per chunk file, enforced at function boundaries. `0` (default) = unlimited. |

| Flag | Description |
|---|---|
| `--unsafe-memory` | Emit linear-memory loads/stores without bounds checks. Faster, but only sound if the module never accesses memory out of bounds — an out-of-bounds access becomes undefined behavior. Off by default. May appear anywhere in the arguments. |

### Examples

```bash
# Transpile to a single Rust file
wasm2rs module.wasm module.rs

# Write to stdout
wasm2rs module.wasm

# Split a large module into a crate directory (one function per file)
wasm2rs module.wasm out_dir 1

# Split with both caps: at most 4 functions and 100 KB per chunk file
wasm2rs module.wasm out_dir 4 102400

# Emit unchecked linear-memory access
wasm2rs --unsafe-memory module.wasm module.rs
```

### Single file vs. split output

- **Single file** — when both `funcs_per_file` and `max_bytes_per_file` are `0` (the default), the whole module is emitted as one Rust source file (to `output`, or stdout if omitted).
- **Split output** — when either cap is positive, `output` is treated as a directory.
  wasm2rs writes a `lib.rs` crate root plus `funcs_0.rs`, `funcs_1.rs`, … chunk files, and also drops in a ready-to-use `Cargo.toml` (with a size-optimized `[profile.release]`).
  Splitting keeps very large modules compilable while bounding peak memory.
  Build it directly:

  ```bash
  cd out_dir
  cargo build --release
  ```

## Library API

wasm2rs is also usable as a library:

```rust
use wasm2rs::{TranspileOptions, transpile_with_options};

let wasm: Vec<u8> = std::fs::read("module.wasm")?;
let rust = transpile_with_options(&wasm, &TranspileOptions { unsafe_memory: false })?;
```

Streaming/split transpilation is available via `transpile_split_with_options`, and `cargo_manifest` produces the recommended `Cargo.toml` for split output.

## License

Licensed under the [MIT License](LICENSE).
