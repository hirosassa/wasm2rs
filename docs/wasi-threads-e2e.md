# wasi-threads end-to-end tests

`tests/it/wasi_sdk_e2e.rs` builds real multi-threaded WebAssembly programs with
external toolchains, transpiles them with wasm2rs, compiles the generated Rust
and runs it natively. The rest of the suite uses hand-written WAT; these tests
check that binaries produced by real compilers work too.

| Test | Toolchain | What it covers |
|---|---|---|
| `a_wasi_sdk_pthread_reactor_runs_after_initialize` | wasi-sdk `clang` | A C reactor with a constructor and four pthreads. The test calls `_initialize`, then `run` twice on one instance. |
| `a_wasi_sdk_pthread_command_counts_across_threads` | wasi-sdk `clang` | A C command (`_start`, no `-mexec-model`) with four pthreads; `main` returns non-zero on a wrong count, which exits the process non-zero. |
| `a_rust_wasi_threads_command_counts_across_threads` | `rustc --target wasm32-wasip1-threads` | A Rust command (`_start`) that spawns four `std::thread`s on a shared atomic counter and asserts the total itself. |
| `an_emscripten_pthread_command_counts_across_threads` | `emcc -pthread -sSTANDALONE_WASM` (emsdk) | An emscripten command (`_start`) with four pthreads on a shared atomic counter. Emscripten asks its JS host for a Worker (`env.__pthread_create_js`); wasm2rs answers with a native thread running a sibling instance. |

The first three exercise the `start` section (`__wasm_init_memory`), `wasi::thread-spawn`,
the shared memory, and WASI on shared memory.

### Emscripten pthreads

A module importing `env.__pthread_create_js` is generated with that import
native: it starts a thread, runs `_emscripten_thread_init`, `_emscripten_tls_init`,
the start routine from the table and `_emscripten_thread_exit`, as emscripten's
Worker does. These exports must exist, the memory must be shared and the table
module-defined, otherwise transpiling is rejected. The JS-only bookkeeping
imports (`_emscripten_thread_cleanup`, mailbox, `emscripten_check_blocking_allowed`,
`emscripten_exit_with_live_runtime`, ...) are no-ops, and
`_emscripten_init_main_thread_js` runs the main thread's init. Everything else
under `env` (for example proxying a call to the main thread) is still left to the
host. Only the standalone (`-sSTANDALONE_WASM`) build is covered; the default JS
build also needs `emscripten_get_now`, `emscripten_resize_heap` and `exit`.

## Skipping

The tests need tools that are not part of a normal checkout. When a tool is
missing the test prints a `skipped: ...` line and **passes**, so a green run
does not prove the test ran. Use `--nocapture` to see the messages:

```sh
cargo test --test it wasi_sdk_e2e -- --nocapture
```

CI runs these tests in two places, and both make a missing tool an error:

- The `build` job installs the Rust target and sets `WASM2RS_REQUIRE_E2E=rust`,
  so the Rust command test must run on every toolchain of the matrix. It has
  no wasi-sdk, so the two wasi-sdk tests skip there.
- The `E2E with wasi-sdk` job downloads wasi-sdk (cached, sha256-verified) and
  runs this module with `WASM2RS_REQUIRE_E2E=1`, so all three tests must run.
  To bump wasi-sdk, update `WASI_SDK_TARBALL`, `WASI_SDK_SHA256` and the
  release tag in the download URL together in `.github/workflows/test.yaml`
  (the sha256 also keys the cache).

### Making a missing tool an error

`WASM2RS_REQUIRE_E2E` turns the "skip" into a failure for the tools it names:

| Value | Effect |
|---|---|
| unset / empty | a missing tool skips the test (the default) |
| `1` | every tool is required |
| `rust`, `wasi-sdk`, `emsdk`, or a comma-separated list such as `rust,wasi-sdk` | only the named tools are required |

```sh
WASM2RS_REQUIRE_E2E=1 cargo test --test it wasi_sdk_e2e
```

## Setup

### Rust target (for the Rust command test)

```sh
rustup target add wasm32-wasip1-threads
```

The test checks that `rustc --print target-libdir --target wasm32-wasip1-threads`
points at an installed directory.

### wasi-sdk (for the C reactor test)

1. Download a release for your platform from
   <https://github.com/WebAssembly/wasi-sdk/releases> (for example
   `wasi-sdk-34.0-arm64-macos.tar.gz`) and extract it somewhere.
2. Point `WASI_SDK_PATH` at the extracted directory. If it is unset, the test
   looks in `/opt/wasi-sdk`.

```sh
export WASI_SDK_PATH=$HOME/tools/wasi-sdk-34.0-arm64-macos
cargo test --test it wasi_sdk_e2e -- --nocapture
```

The test uses `$WASI_SDK_PATH/bin/clang` with
`--target=wasm32-wasip1-threads -pthread -mexec-model=reactor`.

## How a test works

1. The test writes a small source file to a scratch directory under the system
   temp directory (`wasm2rs_sdk_e2e_<name>`) and builds it to a `.wasm`.
2. `export_index` reads the module's export section. The generated Rust names a
   function `func{N}` by its index, so the test looks the index up by export name
   (`_initialize`, `run`, `_start`).
3. `compile_run_wasm` (in `tests/it/common/mod.rs`) transpiles the wasm, appends a
   `main` that drives the instance, builds it with the suite's usual
   `rustc -D warnings` flags, and asserts the program exits 0.

A module with an imported shared memory (the Rust one) needs the host to supply
it, so the test reads the memory's initial size from the import section and
passes `SharedMemory::new(pages * 65536)`. A module that defines its memory
(the C reactor) is created with `Instance::new()`.

## Adding a case

Write the program as a string in the test (no fixture files), build it, look up
the exports you call, and drive them from the `main_body` string. Skip with a
message when your tool is missing, as the existing tests do. Prefer having the
guest assert its own result (a failed assertion traps) or return a value the
host asserts on.

## Timing

Each test takes about one second: building the wasm, transpiling it (tens of
milliseconds), `rustc` at `opt-level=0` (under a second) and running four
threads. The generated binary is cached by content, so a rerun with unchanged
inputs skips the `rustc` step.

### emsdk (for the emscripten test)

Install an [emsdk](https://github.com/emscripten-core/emsdk) and source its
`emsdk_env.sh`, which sets `EMSDK`; the test runs `$EMSDK/upstream/emscripten/emcc`.

```sh
source ~/tools/emsdk/emsdk_env.sh
cargo test --test it an_emscripten_pthread -- --nocapture
```
