# Component toolchain survey (2026-11-12, wasm2rs 0.4.0)

Goal: find out which real-world `wasm32-wasip2` components `--component` handles today, so the
follow-ups (#184, #185, #186 and new ones) are ordered by what users hit first.

Method: one `wasi:cli/run` "hello" per toolchain, run with `examples/wasip2_run` (release build,
Apple M-series, `transpile_component` + `rustc -C opt-level=1`). Fixtures were built in a scratch
directory; the commands are below so the survey can be repeated.

## Results

| # | Toolchain | Component | Result |
|---|---|---|---|
| A | `rustc --target wasm32-wasip2` (std) | 2.7 MB, one core module | **works** (about 16 s with the `wasm2rs-wasi` build), args passed through |
| B | `wasm32-wasip1` core + `wasm-tools component new --adapt` (command adapter) | 2.2 MB, main + adapter module | **works** (L3 linking; run in 7.0 s with the `wasm2rs-wasi` build, rustc 1.6 s): prints `hello from rust`. The first run failed only in `wasip2_run`, which looked for `wasi:cli/run@0.2.0` while the adapter exports `@0.2.3` |
| C | Go (`GOOS=wasip1`) + the same adapter | 2.5 MB, main + adapter module | **runs when the environment is empty; fails with an environment, on wasmtime too** (#311): transpiles and builds (about 3 min in total; rustc 168 s, 4.3 GB peak RSS at `-C opt-level=1`). With an inherited environment it traps with `assertion failed at adapter line 2745` (the adapter's `State` magic) because the adapter's two extra pages (36 -> 38) overlap Go's heap range (`bloc` 36 pages, `blocMax` 38 pages). The same failure reproduces on wasmtime v30.0.0 with `-S inherit-env`; `wasmtime run` passes an empty environment by default, and `env -i` makes the wasm2rs program print `hello from go`. Not a wasm2rs defect: a Go + adapter incompatibility |
| D | `jco componentize` (StarlingMonkey, JS) | 13.9 MB | **fails** in `rustc` (E0046) after about 10 minutes |
| E | `componentize-py` (CPython 3.14) | 17.6 MB, 14 core modules (12 without the shim and fixup) | **fails**: `unsupported: export exports#init: its memory is not an export of the same core instance as the function`. Four small gates in the linker, then a 116 MB / 1.9 M-line source that `rustc` cannot be expected to build; see [f2-dynamic-linking-design.md](f2-dynamic-linking-design.md) |

## Findings

### F1. Adapter-based components pick the adapter as the program module (B, C)

`wasm-tools component new --adapt` links two core modules: the user's program (`__main_module__`)
and the WASI adapter. The adapter imports `env.memory` and `__main_module__._start`, and it is the
module that exports `wasi:cli/run#run`. `transpile_component` chooses the module by the lifted
exports (see [component-input-design.md](component-input-design.md), decision 4), so it picks the
adapter, which imports its memory, and `reentrant_imports` rejects that.

This is the shape of every component made from a preview1 toolchain (older `cargo-component`,
Go, wasi-sdk C/C++ through the adapter). It is the most likely thing a user hits.

Ways out, cheapest first:
1. A clear error that names the cause ("the module that lifts the exports imports its memory from
   another core module; linking several core modules is not supported") instead of the options
   error. Tiny, and worth doing regardless.
2. Link the core modules inside one `Instance`: the adapter's imports of the main module's
   exports become direct calls, and its memory import becomes the main module's memory. Needs a
   design for a module that is both a program and an importer (cross-module imports of
   functions, memories, tables and globals). Medium to large; also unblocks #185.

### F2. Dynamically linked components (E)

`componentize-py` output is built from `libpython`, `libc`, runtime and binding libraries, each a
core module, joined through shared memory and tables (`wit-component` dylib linking). The first
guard hit is `exports#init`, whose memory comes from another instance. Beyond that guard this
needs F1's linking and a lot more (shared table, indirect calls across modules, `dlopen`-style
init order). Treat as out of scope until F1 is done; the error message is already explicit.

Update (2026-11-13, after L1-L5): investigated in [f2-dynamic-linking-design.md](f2-dynamic-linking-design.md).
The linking side is small (shared memory and table, constant `__table_base`, no start functions); the
blocker is F4, since the generated source is the size of the JS component's.

### F3. A function imported twice makes the glue incomplete (D)

The JS component's program module imports the same WIT function twice (for example
`wasi:cli/environment@0.2.12#get-arguments`; why it is imported twice is not yet investigated).
`named_imports` gives the second one the name `..._81`; the generated `Imports` trait has
both methods, but the `Glue` impl only emits the first, so `rustc` fails with E0046 "missing
`wasi_cli_environment_get_arguments_81`, `wasi_io_poll_poll_85`, ..." (25 methods).

This looks like a real bug in `wit_host_glue`: the glue should implement every `Imports` method,
delegating duplicates to the same `Host` method. It is independent of F1 and small.

The same component also imports `wasi:*@0.2.12` and `wasi:http/types@0.2.10`: patch versions newer
than 0.2.0 are accepted (the first glue method names show `@0.2.12`), so they are not a problem
by themselves.

### F4. `rustc` time on large components (D)

The 13.9 MB JS component produced a 2 M-line `program.rs`; `rustc -C opt-level=1` took about 10
minutes (user 270 s, sys 332 s: the system time points at memory pressure). Not a correctness
issue, but it makes large guests impractical. Worth a separate measurement (`--split`, lower
opt-level, `-Z time-passes`) once F3 lets D build at all.

Measured in [f4-rustc-time-spike.md](f4-rustc-time-spike.md): the time is a quadratic `rustc` early-lint
cost on redundant parentheses in one huge file (587 s -> 61 s on the JS `program.rs` without them), not a large function and not LLVM.

### F5. Things that worked and need no work

- `wasm32-wasip2` std output, including arguments (A).
- Newer patch versions of the WASI 0.2 interfaces (D's imports at 0.2.10 / 0.2.12).

## Suggested order

1. F3 (glue emits every duplicate import). Small, a real bug, and it tells us whether D runs.
2. F1 step 1 (clear error for adapter-based input), then F1 step 2 (module linking) as the main
   design item. #185 (exports from several modules) should be designed together with it.
3. #184 and #186 as small follow-ups (no survey case hit them): both done with F1 step 2 (L3, L4).
4. F4 measurement after F3.
5. F2 only after F1 step 2: done as an investigation; F4 first for the end-to-end goal, see the F2 design.

Not covered yet: TinyGo, `cargo-component` with a custom world, wasi-sdk C/C++ `wasm32-wasip2`
and `wasi:http` incoming-handler guests in other languages. Add a row to the table when one is
tried.

## Reproduction

```bash
# A
rustc --target wasm32-wasip2 -O hello.rs -o rust-hello.wasm
# B (adapter = wasi_snapshot_preview1.command.wasm from the wasmtime v30.0.0 release)
rustc --target wasm32-wasip1 -O hello.rs -o p1.core.wasm
wasm-tools component new p1.core.wasm --adapt wasi_snapshot_preview1=adapter.wasm -o p1-hello.wasm
# C
GOOS=wasip1 GOARCH=wasm go build -o go.core.wasm go-hello.go   # go 1.27
wasm-tools component new go.core.wasm --adapt wasi_snapshot_preview1=adapter.wasm -o go-hello.wasm
# D (wit/ = tests/wasi_p2_wit as wit/deps, world: include wasi:cli/imports; export wasi:cli/run)
npx @bytecodealliance/jco componentize app.js --wit wit --world-name w -o js-hello.wasm
# E
uvx componentize-py -d wit -w w componentize app -o py-hello.wasm   # componentize-py 0.25.1
# run
cargo run --release --example wasip2_run -- <component>.wasm
```
