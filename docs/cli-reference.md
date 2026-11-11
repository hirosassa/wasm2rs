# CLI reference

Complete description of the `wasm2rs` command line. For a quick start see the [README](../README.md#usage).

```
wasm2rs [--unsafe-memory] [--simd-intrinsics] [--jobs=N] [--split-dispatch=N] [--call-depth-limit=N] [--entry-exports=a,b] [--named-exports] [--named-imports] [--import-memory] [--reentrant-imports] <input.wasm> [output] [funcs_per_file] [max_bytes_per_file]
wasm2rs --component [--unsafe-memory] [--simd-intrinsics] [--jobs=N] [--split-dispatch=N] [--call-depth-limit=N] [--external-types=<path>] <component.wasm> [output]
wasm2rs --dump <input.wasm>
wasm2rs --extract-core <component.wasm> <output dir>
wasm2rs --wit-host [--glue [--external-types=<path>]] <component.wasm> [output]
```

## Arguments

| Argument | Description |
|---|---|
| `<input.wasm>` | Path to the input WebAssembly binary (required). |
| `[output]` | Output file, or output directory when splitting. Omit to write to stdout. |
| `[funcs_per_file]` | Max functions per chunk file. `0` (default) = unlimited. |
| `[max_bytes_per_file]` | Max bytes per chunk file, enforced at function boundaries. `0` (default) = unlimited. |

## Basic flags

| Flag | Description |
|---|---|
| `--simd-intrinsics` | Emit `core::arch` bodies (NEON on aarch64, SSE2 on x86_64) for the integer SIMD lane ops — add/sub/mul/neg/abs, saturating add/sub, `avgr_u` and the integer compares — instead of the scalar per-lane loop, which remains the fallback on every other target. Results are bit-identical (a differential test checks both architectures). The generated code gains `unsafe` blocks (a same-size `transmute` and intrinsic calls whose target feature the `cfg` guarantees). Float, shift and widening lane ops are unchanged. Off by default. May appear anywhere in the arguments. |
| `--unsafe-memory` | Emit linear-memory loads/stores without bounds checks. Faster, but only sound if the module never accesses memory out of bounds — an out-of-bounds access becomes undefined behavior. Off by default. Not applied to a `shared` memory. May appear anywhere in the arguments. |
| `--jobs=N` | Threads used to generate function bodies. The output is byte-identical for every value. `0` (default) = automatic (at most 8, only for modules large enough to benefit); `1` = calling thread only. |
| `--call-depth-limit=N` | Count nested calls of defined functions and trap with `call stack exhausted` (a panic) instead of overflowing the native stack when a call would nest deeper than `N`. Per thread; `return_call*` chains do not deepen it. Pick `N` the thread stack can hold. Off by default (output unchanged). |
| `--split-dispatch=N` | Split a flattened dispatch function with more than `N` arms into several smaller part functions so the Rust backend can optimize them independently. `0` (default) = keep each function whole. |
| `--entry-exports=a,b` | Emit only the functions reachable from the listed exported functions, plus the start function and the entry points the generated code itself calls (`wasi_thread_start`, the emscripten thread exports). Shortens the generated source and the Rust build; surviving functions keep their `func{N}` names. Reachable `call_indirect`/`table.*` keeps every table-element function. An empty list (`--entry-exports=`) keeps no export as a root; an unknown name is an error. Off by default. |
| `--dump` | Print a summary of the module (function/import/export counts, how each import resolves — native WASI or host trait —, memory limits, proposals in use, whether a host `Imports` implementation is needed) and generate no code. Takes only the input file (the flags that change the generated code are refused, not ignored). Works for modules the transpiler would reject. |

## Output control flags

These change the shape of the generated `Instance` / `Imports` API.

| Flag | Description |
|---|---|
| `--named-exports` | Also emit a method (or free function, for a stateless module) named after each exported function, e.g. `inst.add(1, 2)`, forwarding to `func{N}`, and a getter `inst.stack_pointer()` for each exported `i32`/`i64`/`f32`/`f64` global (current value) plus a setter `inst.set_stack_pointer(v)` for each mutable one (a reference-typed or `v128` global gets neither; a setter name that clashes with an export gets a `_{index}` suffix). The name is the export name as a snake_case identifier; a clash with a Rust keyword or a generated member appends `_`, and a duplicate appends `_{index}`. Off by default. |
| `--named-imports` | Name the `Imports` trait methods after the imports — `env_log` for `(import "env" "log" …)` — instead of `import{j}`. The name is the module (without a `@version` suffix) and field as snake_case, joined with `_`; a clash with a keyword or another trait member appends `_`, a duplicate appends `_{function index}`. WASI preview1 imports handled natively get no method. Off by default, since renaming breaks existing host implementations. |
| `--import-memory` | Make the module's own (defined, non-shared) memory 0 host-owned, like an imported memory: the host lends the buffer through `Imports::memory`/`memory_mut` (at least the declared minimum number of pages), so its own methods can read and write guest memory. Active data segments are written into it at instantiation and `memory.grow` resizes it. Needed for imports that take pointers into linear memory, such as the lowered functions of a WASI preview2 component. No effect on a shared memory, a module that already imports its memory, or one without memory. |
| `--reentrant-imports` | Let the host call back into the guest while an import runs: every `Imports` method takes the `Instance` as its second parameter (`g: &mut Instance<Self>`), so it can call guest functions (`g.func{N}(..)`, e.g. a canonical-ABI `cabi_realloc`) and read or write guest memory (`g.memory()`). The host is lent out for the call, so a guest function reached from inside an import that calls an import again traps. Cannot be combined with `--import-memory`, an imported memory, table or global, or the native `wasi::thread-spawn`. No effect without a host function import. Changes every `Imports` method signature, so it is off by default. |

## Component and WIT flags

| Flag | Description |
|---|---|
| `--component` | Transpile a WebAssembly *component* (e.g. `rustc --target wasm32-wasip2` output) into one Rust source, like `wasm2rs::transpile_component`: its program core module (selected as `transpile_component` does) transpiled with `--named-imports --named-exports --reentrant-imports`, then `--wit-host --glue` with the typed `Exports` trait. Builds with `wasm2rs-wasi` (`--external-types=wasm2rs_wasi` is the default). Writes to `[output]` or stdout. Combines only with `--unsafe-memory`, `--simd-intrinsics`, `--jobs=N`, `--split-dispatch=N`, `--call-depth-limit=N` and `--external-types=<path>`; it refuses, naming the flag, `--entry-exports`, `--import-memory`, `--named-exports`, `--named-imports`, `--reentrant-imports` (it sets the last three itself), `--wit-host`, `--glue`, `--dump`, `--extract-core` and the split arguments (one source only). A core module as input is an error. |
| `--wit-host` | Read the types of a WebAssembly component's imports and write a typed Rust `pub trait Host` with one method per function of every imported interface (`fn wasi_cli_environment_get_arguments(&mut self) -> Vec<String>;`), named like the `--named-imports` method of the core import that lowers it. Supports integers, floats, `bool`, `char`, `string`, `list`, `option`, `result`, `tuple`, `record` (one struct per record, emitted once), `variant` (a Rust `enum`, `Case(T)` for a payload), `enum` (a fieldless `Copy` enum), `flags` (a `Copy` struct `pub struct F(pub u8|u16|u32)` by the canonical ABI width, one `F::NAME` constant per flag, `contains`/`union`) and resources (one `Copy` newtype `pub struct Pollable(pub u32);` per resource, shared by every interface that uses it; `own<r>` and `borrow<r>` are both that handle; `[method]`, `[static]` and `[constructor]` functions are methods named like the core import, and the defining interface also gets `fn <module>_resource_drop_<r>(&mut self, handle: R);` for the core import `[resource-drop]<r>`); a function using `future`, `stream` or `error-context` is an error naming the type. Writes to `[output]` or stdout. Takes only the input and the optional output (the flags that change the generated code, `--dump` and `--extract-core` are refused, not ignored). Only the trait is generated unless `--glue` is given. |
| `--glue` | Only with `--wit-host`: also append the glue that lowers the typed `Host` to the flat reentrant `Imports` of the component's core module (transpiled with `--named-imports --reentrant-imports`): `pub struct Glue<H: Host>(pub H);` and `impl<H: Host> Imports for Glue<H>`, so the guest is `Instance::new(Glue(host))`. Each `Imports` method has the name of the `Host` method it forwards to, lifts the flat core parameters into the typed ones, calls the host and lowers the result (like the canonical ABI: any non-zero `bool` is true, a narrow integer keeps its low bits, undeclared flag bits are ignored; an invalid `char` or an unknown enum/variant discriminant panics). Covers every type of the typed trait, nested to any depth: scalars, `string` (UTF-8; invalid UTF-8 panics), `list`, `option`, `result`, `tuple`, `record`, `variant`, `enum`, `flags` and resource handles, lifted from the flat parameters or from guest memory (pointers are bounds- and alignment-checked, and one call may lift at most as many bytes and elements as the guest's memory holds, so descriptors aliasing the same bytes cannot make the host copy more: a violation panics like a trap, `lifted data too large` for the budget) and lowered to the flat result or to memory. Parameters that flatten to more than 16 core values come as one pointer to a record of them, a result of more than one flat value goes through a trailing return pointer, and a returned `string`/`list` is allocated with the guest's `cabi_realloc(0, 0, align, size)`, so transpile the core module with `--named-exports` (besides `--named-imports --reentrant-imports`) when the component returns strings or lists. `examples/wasip2_run.rs` runs a `wasm32-wasip2` component through the `wasm2rs-wasi` host and this glue (generated with `--external-types=wasm2rs_wasi`) through `transpile_component` (its input is the component, not an extracted core module). |
| `--external-types=<path>` | Only with `--wit-host --glue` or `--component` (under `--component` the default is `wasm2rs_wasi`, and the CLI always uses an external library): the glue declares neither the types nor `trait Host` and instead refers to a library that does, at the Rust path `<path>` (e.g. `--external-types=wasm2rs_wasi`): `use <path>::Host;`, `Glue<H: Host>`, and every type as `<path>::<interface module>::<Type>`. Implies interface-qualified naming: each type lives in a `pub mod` named like the method-name prefix of its defining interface (`wasi_filesystem_types::ErrorCode`, `wasi_sockets_network::ErrorCode`) and keeps its WIT name, so the path depends only on the defining interface and the type name, never on what else the component imports; a type several interfaces use is declared once, in its definer's module. The library side is `wit_host_trait_with_options` with `WitNaming::Qualified` (the CLI does not write it). |
| `--extract-core` | Write the core modules of a WebAssembly component to `<output dir>/module{n}.wasm` (in order, nested components included) and list their sizes; each one is a plain module that wasm2rs can transpile. Takes the input and the directory (the flags that change the generated code, and `--dump`, are refused, not ignored). A component is not transpiled directly. |

## Examples

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

## Single file vs. split output

- **Single file** — when both `funcs_per_file` and `max_bytes_per_file` are `0` (the default), the whole module is emitted as one Rust source file (to `output`, or stdout if omitted).
- **Split output** — when either cap is positive, `output` is treated as a directory.
  wasm2rs writes a `lib.rs` crate root plus `funcs_0.rs`, `funcs_1.rs`, … chunk files, and also drops in a ready-to-use `Cargo.toml` (with a size-optimized `[profile.release]`).
  Splitting keeps very large modules compilable while bounding peak memory.
  Build it directly:

  ```bash
  cd out_dir
  cargo build --release
  ```

## Notes

- The binary uses `mimalloc` to keep peak RSS down while transpiling gigabyte-scale output (the `cli` feature; see [library-api.md](library-api.md)).
- The WASI preopen root is configurable in the generated code (default: the current directory).
- A core module given to `--component`, or a component given without it, is rejected with a clear error.
