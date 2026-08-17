# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-08-17

### Added

- **Opt-in flattened-dispatch splitting** via the `--split-dispatch=<n>` CLI flag
  (and the `TranspileOptions::split_dispatch` library option). A flattened
  `loop { match pc { … } }` dispatch whose surviving arm count exceeds the cap is
  split into several sibling *part* functions over a shared state struct, so a
  pathologically large function is emitted as many smaller ones the Rust backend
  can optimise and codegen-parallelise independently — without changing what it
  computes. `0` (the default) keeps each flattened function whole.
- The generated WASI runtime's **preopen root is now configurable**, so the
  directory advertised to the module no longer has to be hard-coded.

### Changed

- `unreachable` and indirect-call type-mismatch **traps are now emitted as cold,
  out-of-line helpers** instead of being inlined at every trap site, shrinking the
  generated code on the hot path.

### Documentation

- Added crates.io / docs.rs / CI / coverage badges and prebuilt-binary install
  instructions to the README.

## [0.1.0] - 2026-08-08

Initial release. wasm2rs transpiles a WebAssembly binary into standalone, native
Rust source code — no interpreter and no runtime dependency on a WebAssembly
engine, while preserving WebAssembly semantics (wrapping arithmetic, linear-memory
bounds checks, and trap behaviour).

### Added

- **Core transpiler and CLI** producing standalone Rust from a `.wasm` module,
  with output to a single file, to stdout, or split across many chunk files for
  very large modules (with a ready-to-use, size-optimised `Cargo.toml`). The
  binary uses `mimalloc` to keep peak memory down on gigabyte-scale output.
- **Library API**: `transpile_with_options`, `transpile_split_with_options`,
  `TranspileOptions`, and `cargo_manifest`.
- **MVP WebAssembly coverage**: the full numeric instruction set (i32/i64/f32/f64
  arithmetic, division/remainder, shifts/rotates, comparisons, conversions,
  float min/max, trapping float-to-int truncation), linear-memory loads/stores,
  globals, direct and indirect calls, and the `unreachable` operator.
- **Control flow and multi-value**: typed block signatures, block/if parameters,
  parameterized (loop-carried) loops, value-carrying `br_table` targets, and
  multi-value function results.
- **Memory and segments**: active and passive data/element segments (with
  `init`/`drop`), bulk memory operations (`memory.fill`/`memory.copy`,
  `table.copy`), imported memory, and imported tables.
- **Reference types**: `ref.null`/`ref.func`/`ref.is_null`, table instructions
  (`get`/`set`/`size`/`grow`/`fill`), `externref` values and tables.
- **Imports via host traits**: imported functions, globals, memory, and tables
  injected through generated Rust traits, plus static and dynamic cross-instance
  module linking.
- **Native WASI (preview 1)** subset implemented directly in Rust — `proc_exit`,
  `fd_write`/`fd_read`, argv/environ, clocks, `random_get`, stdio `fd` operations,
  and a real-filesystem subset (`path_open`, `fd_readdir`, `path_link`, and other
  path-mutating calls).
- **Advanced proposals**:
  - Multi-threaded **atomics** over shared memory.
  - **Multi-memory** (multiple linear memories).
  - **Tail calls** (`return_call` / `return_call_indirect`).
  - **Garbage collection**: `call_ref`/`return_call_ref`, `i31`, typed funcrefs,
    heap-allocated struct and array objects, `ref.test`/`ref.cast`/`br_on_cast`
    subtyping, `ref.eq`/`ref.as_non_null`/`br_on_null`/`br_on_non_null`, and
    aggregate constructors with bulk array operations.
  - **Typed continuations**: `cont.new`, `resume`, `suspend`, `cont.bind`,
    `resume_throw`, and `switch`.
  - Legacy **exception handling**.
  - **SIMD/v128**: lane arithmetic, width-changing lanes, widening
    multiply/add lanes, and relaxed-SIMD fused-multiply-add and dot products.
- **Opt-in `--unsafe-memory` mode** that emits unchecked linear-memory access for
  faster code on modules trusted to stay in bounds (an out-of-bounds access
  becomes undefined behaviour instead of a wasm trap).

[0.1.1]: https://github.com/hirosassa/wasm2rs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hirosassa/wasm2rs/releases/tag/v0.1.0
