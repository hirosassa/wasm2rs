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
- Wide **WebAssembly proposal coverage**: SIMD and relaxed SIMD, atomics and shared memory, multi-memory, tail calls, exception handling (including legacy), GC, and stack-switching continuations.
- **WASI**: preview1 calls are implemented natively; other imports go through a generated host `Imports` trait. wasi-threads, emscripten pthread builds and emscripten JS-glue builds run on native threads (see [docs/wasi-threads-e2e.md](docs/wasi-threads-e2e.md)). The WASI preopen root is configurable in the generated code (default: the current directory). Only core modules are handled: a WebAssembly *component* (preview2 / the Component Model) is rejected with a clear error.
- **Parallel code generation**: function bodies are lowered on multiple threads with byte-identical output (`--jobs`).

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
wasm2rs [--unsafe-memory] [--jobs=N] [--split-dispatch=N] [--entry-exports=a,b] [--named-exports] [--named-imports] [--import-memory] [--reentrant-imports] <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]
wasm2rs --dump <input.wasm>
wasm2rs --extract-core <component.wasm> <output dir>
wasm2rs --wit-host [--glue [--external-types=<path>]] <component.wasm> [output]
```

| Argument | Description |
|---|---|
| `<input.wasm>` | Path to the input WebAssembly binary (required). |
| `[output]` | Output file, or output directory when splitting. Omit to write to stdout. |
| `[funcs_per_file]` | Max functions per chunk file. `0` (default) = unlimited. |
| `[max_bytes_per_file]` | Max bytes per chunk file, enforced at function boundaries. `0` (default) = unlimited. |

| Flag | Description |
|---|---|
| `--unsafe-memory` | Emit linear-memory loads/stores without bounds checks. Faster, but only sound if the module never accesses memory out of bounds — an out-of-bounds access becomes undefined behavior. Off by default. Not applied to a `shared` memory. May appear anywhere in the arguments. |
| `--jobs=N` | Threads used to generate function bodies. The output is byte-identical for every value. `0` (default) = automatic (at most 8, only for modules large enough to benefit); `1` = calling thread only. |
| `--split-dispatch=N` | Split a flattened dispatch function with more than `N` arms into several smaller part functions so the Rust backend can optimize them independently. `0` (default) = keep each function whole. |
| `--entry-exports=a,b` | Emit only the functions reachable from the listed exported functions, plus the start function and the entry points the generated code itself calls (`wasi_thread_start`, the emscripten thread exports). Shortens the generated source and the Rust build; surviving functions keep their `func{N}` names. Reachable `call_indirect`/`table.*` keeps every table-element function. An empty list (`--entry-exports=`) keeps no export as a root; an unknown name is an error. Off by default. |
| `--named-exports` | Also emit a method (or free function, for a stateless module) named after each exported function, e.g. `inst.add(1, 2)`, forwarding to `func{N}`, and a getter `inst.stack_pointer()` for each exported `i32`/`i64`/`f32`/`f64` global (current value) plus a setter `inst.set_stack_pointer(v)` for each mutable one (a reference-typed or `v128` global gets neither; a setter name that clashes with an export gets a `_{index}` suffix). The name is the export name as a snake_case identifier; a clash with a Rust keyword or a generated member appends `_`, and a duplicate appends `_{index}`. Off by default. |
| `--named-imports` | Name the `Imports` trait methods after the imports — `env_log` for `(import "env" "log" …)` — instead of `import{j}`. The name is the module (without a `@version` suffix) and field as snake_case, joined with `_`; a clash with a keyword or another trait member appends `_`, a duplicate appends `_{function index}`. WASI preview1 imports handled natively get no method. Off by default, since renaming breaks existing host implementations. |
| `--import-memory` | Make the module's own (defined, non-shared) memory 0 host-owned, like an imported memory: the host lends the buffer through `Imports::memory`/`memory_mut` (at least the declared minimum number of pages), so its own methods can read and write guest memory. Active data segments are written into it at instantiation and `memory.grow` resizes it. Needed for imports that take pointers into linear memory, such as the lowered functions of a WASI preview2 component. No effect on a shared memory, a module that already imports its memory, or one without memory. |
| `--reentrant-imports` | Let the host call back into the guest while an import runs: every `Imports` method takes the `Instance` as its second parameter (`g: &mut Instance<Self>`), so it can call guest functions (`g.func{N}(..)`, e.g. a canonical-ABI `cabi_realloc`) and read or write guest memory (`g.memory()`). The host is lent out for the call, so a guest function reached from inside an import that calls an import again traps. Cannot be combined with `--import-memory`, an imported memory, table or global, or the native `wasi::thread-spawn`. No effect without a host function import. Changes every `Imports` method signature, so it is off by default. |
| `--extract-core` | Write the core modules of a WebAssembly component to `<output dir>/module{n}.wasm` (in order, nested components included) and list their sizes; each one is a plain module that wasm2rs can transpile. Takes the input and the directory (the flags that change the generated code, and `--dump`, are refused, not ignored). A component is not transpiled directly. |
| `--wit-host` | Read the types of a WebAssembly component's imports and write a typed Rust `pub trait Host` with one method per function of every imported interface (`fn wasi_cli_environment_get_arguments(&mut self) -> Vec<String>;`), named like the `--named-imports` method of the core import that lowers it. Supports integers, floats, `bool`, `char`, `string`, `list`, `option`, `result`, `tuple`, `record` (one struct per record, emitted once), `variant` (a Rust `enum`, `Case(T)` for a payload), `enum` (a fieldless `Copy` enum), `flags` (a `Copy` struct `pub struct F(pub u8|u16|u32)` by the canonical ABI width, one `F::NAME` constant per flag, `contains`/`union`) and resources (one `Copy` newtype `pub struct Pollable(pub u32);` per resource, shared by every interface that uses it; `own<r>` and `borrow<r>` are both that handle; `[method]`, `[static]` and `[constructor]` functions are methods named like the core import, and the defining interface also gets `fn <module>_resource_drop_<r>(&mut self, handle: R);` for the core import `[resource-drop]<r>`); a function using `future`, `stream` or `error-context` is an error naming the type. Writes to `[output]` or stdout. Takes only the input and the optional output (the flags that change the generated code, `--dump` and `--extract-core` are refused, not ignored). Only the trait is generated unless `--glue` is given. |
| `--glue` | Only with `--wit-host`: also append the glue that lowers the typed `Host` to the flat reentrant `Imports` of the component's core module (transpiled with `--named-imports --reentrant-imports`): `pub struct Glue<H: Host>(pub H);` and `impl<H: Host> Imports for Glue<H>`, so the guest is `Instance::new(Glue(host))`. Each `Imports` method has the name of the `Host` method it forwards to, lifts the flat core parameters into the typed ones, calls the host and lowers the result (like the canonical ABI: any non-zero `bool` is true, a narrow integer keeps its low bits, undeclared flag bits are ignored; an invalid `char` or an unknown enum/variant discriminant panics). Covers every type of the typed trait, nested to any depth: scalars, `string` (UTF-8; invalid UTF-8 panics), `list`, `option`, `result`, `tuple`, `record`, `variant`, `enum`, `flags` and resource handles, lifted from the flat parameters or from guest memory (pointers are bounds- and alignment-checked, and one call may lift at most as many bytes and elements as the guest's memory holds, so descriptors aliasing the same bytes cannot make the host copy more: a violation panics like a trap, `lifted data too large` for the budget) and lowered to the flat result or to memory. Parameters that flatten to more than 16 core values come as one pointer to a record of them, a result of more than one flat value goes through a trailing return pointer, and a returned `string`/`list` is allocated with the guest's `cabi_realloc(0, 0, align, size)`, so transpile the core module with `--named-exports` (besides `--named-imports --reentrant-imports`) when the component returns strings or lists. `examples/wasip2_run.rs` runs a `wasm32-wasip2` component through the `wasm2rs-wasi` host and this glue (generated with `--external-types=wasm2rs_wasi`) (its input is the component, not an extracted core module). |
| `--external-types=<path>` | Only with `--wit-host --glue`: the glue declares neither the types nor `trait Host` and instead refers to a library that does, at the Rust path `<path>` (e.g. `--external-types=wasm2rs_wasi`): `use <path>::Host;`, `Glue<H: Host>`, and every type as `<path>::<interface module>::<Type>`. Implies interface-qualified naming: each type lives in a `pub mod` named like the method-name prefix of its defining interface (`wasi_filesystem_types::ErrorCode`, `wasi_sockets_network::ErrorCode`) and keeps its WIT name, so the path depends only on the defining interface and the type name, never on what else the component imports; a type several interfaces use is declared once, in its definer's module. The library side is `wit_host_trait_with_options` with `WitNaming::Qualified` (the CLI does not write it). |
| `--dump` | Print a summary of the module (function/import/export counts, how each import resolves — native WASI or host trait —, memory limits, proposals in use, whether a host `Imports` implementation is needed) and generate no code. Takes only the input file (the flags that change the generated code are refused, not ignored). Works for modules the transpiler would reject. |

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

# Inspect a module without generating code
wasm2rs --dump module.wasm
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
let rust = transpile_with_options(&wasm, &TranspileOptions::default())?;
```

`wasm2rs::dump(&wasm)` returns the same summary as `--dump` (a `ModuleSummary` that implements `Display`).
`wasm2rs::extract_core_modules(&component)` returns a component's core modules as `Vec<Vec<u8>>`.
`wasm2rs::wit_host_trait(&component)` returns the Rust source of the typed `Host` trait that `--wit-host` writes, `wasm2rs::wit_host_glue(&component)` that text plus the `Glue` of `--wit-host --glue`, and `wasm2rs::wit_host_trait_with_options` / `wit_host_glue_with_options` take a `WitOptions { naming: WitNaming::Default | Qualified, external_types: Option<String> }` (see `--external-types`; the plain functions are the default options), and `wasm2rs::wit_functions(&component)` the decoded `Vec<WitFunc>` (interface, function name, parameter and result `WitType`s, with `WitType::Own`/`Borrow` of a `WitResource`; every record, variant, enum, flags and resource records the `interface` that defines it; one synthetic `[resource-drop]<r>` function per defined resource whose `resource.drop` the component lowers; every drop if the component has no top-level core module or has a nested component that lowers a drop) behind it. `WitOptions::exports` (library only, no CLI flag; glue only, off by default so the output is unchanged) adds the typed export side to the glue: `pub trait Exports` with one method per exported function, named like the `--named-exports` method of the core export it lifts (so `wasi:cli/run@0.2.0#run` is `export_wasi_cli_run_0_2_0_run(&mut self) -> Result<(), ()>`), and `impl<H: Host> Exports for Instance<Glue<H>>`. The host lowers the arguments (flat, or one record in guest memory past 16 flat values; a `string`/`list` is written into memory from the lift's `cabi_realloc`) and lifts the result under the same checks and per-call budget as the import glue, then calls the lift's `post-return` with the core result. A guest trap stays a panic. Call it as `Exports::name(&mut inst, ..)`, because the transpiled `Instance` has an inherent method of the same name (the flat core export) that `inst.name(..)` would pick. Needs the same shapes as `wit_exports`; a `realloc` other than `cabi_realloc` (when the function allocates), exports of more than one core module, and a method name that depends on the other exports are `Unsupported` errors; transpile the core module with `--named-exports` (a module without the method fails to build). `wasm2rs::wit_exports(&component)` is the export side: one `WitExport` per exported function (`func`, the `core_module` index among the top-level core modules, the `core_export` name, and the `memory`, `realloc` and `post_return` export names of its `canon lift`, `utf8`). Only a lift that is a direct export of an instantiated top-level core module with UTF-8 strings is read; a shim or adapter instance, a `canon lower`, another string encoding or a nested component's export is an `Unsupported` error naming the export.

Code generation is configured through `TranspileOptions` (`unsafe_memory`, `split_dispatch`, `entry_exports`, `jobs`). It is `Clone` and `Default` but not `Copy`, so pass it by reference.
`transpile(&wasm)` is the shortcut for the default options.

Streaming/split transpilation is available via `transpile_split` / `transpile_split_with_options`, which take a `SplitOptions` (`SplitOptions::single_file()` for one file) and call a sink with each `SourceFile`. Errors are reported as `TranspileError`. `cargo_manifest` produces the recommended `Cargo.toml` for split output.

### Using from `build.rs`

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

### Using the proc macro

For a one-line embed without a build script, use the `wasm2rs-macros` crate:

```rust
mod foo {
    wasm2rs_macros::include_wasm!("assets/foo.wasm", named_exports);
}
```

The path (a plain or raw string literal) is relative to `CARGO_MANIFEST_DIR` (a leading `$OUT_DIR/` refers to a file a build script wrote). The options are `named_exports`, `unsafe_memory`, `named_imports`, `import_memory` and `reentrant_imports`, with the meaning of the CLI flags of the same name (an invalid combination is a compile error). The generated source is parsed into tokens at compile time, so for very large modules prefer `build.rs` with `include!`.

The `mimalloc` allocator is behind the default `cli` feature. Depend on the crate with `default-features = false` to use it as a library without compiling mimalloc.

## Testing

```bash
cargo test --release                 # whole suite (a single integration-test binary, tests/it)
cargo test --release --test it <name> # a subset, by module name
```

The typed WASI preview2 host is the `wasm2rs-wasi` crate (`WasiHost` and the `Host` trait for the whole WASI 0.2.0 surface; `examples/wasip2_run.rs` runs a component with it). Its design (streams, the TCP send queue, error reporting, limitations) is described in [docs/typed-host-design.md](docs/typed-host-design.md). Of `wasi:http`, `fields`, `request-options` and the parts of `outgoing-request` are implemented and an `HttpClient` can be set on the builder (`UreqClient`, an `ureq` one, behind the `http-client` feature: no redirects, `ureq`'s default TLS); the rest still traps ([docs/wasi-http-design.md](docs/wasi-http-design.md)).

See [docs/wasi-threads-e2e.md](docs/wasi-threads-e2e.md) for the end-to-end tests that run real wasi-sdk and Rust `wasm32-wasip1-threads` binaries (they need extra tools and skip when those are missing).

## License

Licensed under the [MIT License](LICENSE).
