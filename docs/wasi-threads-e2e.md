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

Both exercise the `start` section (`__wasm_init_memory`), `wasi::thread-spawn`,
the shared memory, and WASI on shared memory.

## Skipping

The tests need tools that are not part of a normal checkout. When a tool is
missing the test prints a `skipped: ...` line and **passes**, so a green run
does not prove the test ran. Use `--nocapture` to see the messages:

```sh
cargo test --test it wasi_sdk_e2e -- --nocapture
```

CI does not install these tools, so CI skips them. Run them locally when you
change thread, start-section, shared-memory or WASI code.

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
