# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `floor` / `ceil` / `trunc` / `nearest` now return a quiet NaN for a NaN input on
  every host. On baseline x86_64 Linux the host's rounding returned a signaling NaN
  unchanged, which the spec forbids (found by the spec testsuite). The results are
  routed through small `quiet_f32` / `quiet_f64` helpers, emitted only when used;
  ordinary values are unaffected.

## [0.2.0] - 2026-11-01

### Fixed

- A trapping load whose value is discarded by `drop` or `select` is still
  evaluated, so the trap is no longer lost.

### Changed

- Updated `wasmparser` / `wat` to 0.263.
- Smaller generated code and fewer allocations while transpiling (no offset-literal
  type suffixes, fewer temporary allocations, cheaper single-file memory).
- **A `shared` memory no longer sits behind a `Mutex`.** Its bytes are reserved once
  (up to the declared maximum) and accessed without a lock, so threads that touch
  disjoint memory now scale instead of serialising (8 threads of load/store work:
  6.4 s -> 19 ms; one thread: 261 ms -> 18 ms). Plain loads/stores are ordinary
  non-atomic accesses (racing on the same bytes gives unspecified values, as in
  wasm); atomic loads/stores/RMW/cmpxchg and `atomic.fence` are real sequentially
  consistent atomics and now trap on a misaligned address; `memory.grow` on a shared
  memory stops at its declared maximum. `SharedMemory::with_max(min, max)` is new;
  `SharedMemory::new(min)` still works and reserves the wasm32 limit.
- **`TranspileOptions` is no longer `Copy`** (it now owns `entry_exports`); it is
  still `Clone` and `Default`. Pass it by reference or `.clone()` it.

### Added

- **Parallel code generation: `--jobs=N` CLI flag and `TranspileOptions::jobs`.**
  Function bodies are generated on several threads and joined in function-index
  order, so the output is byte-identical for every value. `0` (default) picks the
  available parallelism (at most 8); `1` stays on the calling thread.
- **Emscripten pthread and emscripten JS-glue builds** run on native threads
  (see `docs/wasi-threads-e2e.md`).
- **`--entry-exports=a,b` CLI flag and `TranspileOptions::entry_exports`** emit only the
  functions reachable from the chosen exports (plus the start function and the
  entry points the generated code calls itself). Surviving functions keep their
  wasm-index names. Off by default, so existing output is unchanged. On a 40k-function
  C++ module (26k exports) keeping ~25 exports drops 37% of the functions and 24%
  of the source; reachable table reads keep every element-segment function, which
  bounds the saving.
- **`--dump` CLI flag and `wasm2rs::dump` API** print a summary of a module without
  generating code: function counts, each import and how it resolves (native WASI
  or host trait), exports, memory limits, the proposals in use, and whether the
  generated module needs a host `Imports` implementation. It reads the binary
  directly, so it also works for modules the transpiler rejects.
- **Twelve more WASI preview1 functions are generated natively** instead of being
  left to the host trait: `fd_tell`, `fd_sync`, `fd_datasync`, `fd_advise`,
  `fd_allocate`, `fd_filestat_set_size`, `fd_filestat_set_times`,
  `path_filestat_set_times`, `path_readlink`, `fd_fdstat_set_flags`,
  `fd_fdstat_set_rights` and `fd_renumber`. Requests `std` cannot honour (changing
  descriptor flags, renumbering stdio, setting times on a symlink itself) return
  `ENOTSUP` rather than being silently ignored.
- **`poll_oneoff`, `sock_*` and `proc_raise` are generated natively.**
  `poll_oneoff` supports clock subscriptions (relative and absolute, sleeping until
  the earliest) and `fd_read`/`fd_write` subscriptions (stdio and open files are
  always ready, an unopened fd is an `EBADF` event); no memory or fd-table lock is
  held while sleeping. `sock_accept/recv/send/shutdown` return `EBADF` or
  `ENOTSOCK` (no socket descriptors exist), and `proc_raise` returns `EINVAL` or
  `ENOSYS`. A host that implemented any of these through the `Imports` trait no
  longer receives those calls.

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

[Unreleased]: https://github.com/hirosassa/wasm2rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/hirosassa/wasm2rs/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/hirosassa/wasm2rs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hirosassa/wasm2rs/releases/tag/v0.1.0
