# Component exports (`canon lift`): typed calls into a guest

Status: A2-1 (the decoder) and A2-2 (the `Exports` trait) are implemented; A2-3 and A2-4 are design.
Background: [wasm2rs-wasi-design.md](wasm2rs-wasi-design.md) (the host library, A1, done),
[component-model-poc.md](component-model-poc.md) (how the component analysis came about).

## Problem

`--wit-host --glue` covers the **import** side of a component (`canon lower`): the generated
`Glue<H: Host>` lifts the guest's flat arguments into typed host calls. The **export** side
(`canon lift`) is not read at all:

- `wit_functions` (`src/wit.rs`) parses the export section only for `Type` and `Instance` kinds, to
  grow the index spaces. Func exports, `canon lift` and its options (`memory`, `realloc`,
  `post-return`, string encoding) are skipped, and so are the core instance section and core-func aliases.
- `examples/wasip2_run.rs` finds `wasi:cli/run@0.2.0#run` by scanning the largest core module's
  export names and calls `inst.func{N}()`. It never checks that name against a `canon lift`, nor the
  lowered core signature.

So a host cannot call an exported WIT function with typed arguments and results, and `wasi:http`
(whose entry point is the export `wasi:http/incoming-handler#handle`) is out of reach.

## Decisions

1. **Direct exports first.** Support a `canon lift` whose core function is a direct export of a core
   instance built from a module. Anything else (a lift reached through a shim or adapter instance
   that re-exports) is a clear `Unsupported` error naming the export. Components from `wit-component`
   and `wasm32-wasip2` have the direct shape; tracing arbitrary core instance graphs is a later step.
2. **API shape:** generated methods on `Instance<Glue<H>>` through an extension trait, one typed
   method per exported WIT function.
3. **`wasi:http` is not part of A2.** A2 delivers the export machinery and typed `wasi:cli/run`.
   `wasi:http` needs a large separate host implementation (below) and gets its own design.

## Design

### Decoder (A2-1)

`wit_functions` keeps its meaning (imports). A new `wit_exports(&[u8]) -> Result<Vec<WitExport>>`
walks the same payloads and additionally records:

- the component's exported instances and funcs, with their types (reusing `Instance`, `func_sig`,
  `WitFunc`, `WitType` and `Records`; the exported instance type is no longer discarded);
- for each exported func, the `canon lift` it comes from: the core function index and options
  (`memory`, `realloc`, `post-return`, string encoding);
- the core instance and alias sections, enough to resolve that core function index to *the name of a
  core module export*, which is what `--named-exports` exposes. Non-direct shapes are rejected here.

```rust
pub struct WitExport {
    pub func: WitFunc,          // interface, name, params, result
    pub core_module: usize,     // index among the top-level core modules
    pub core_export: String,    // e.g. "wasi:cli/run@0.2.0#run" (checked against the lift)
    pub memory: Option<String>,
    pub realloc: Option<String>,
    pub post_return: Option<String>,
    pub utf8: bool,             // other encodings: Unsupported
}
```

As implemented: `wit-component` exports an interface as an instance of a small *shim component* that
only re-exports its function imports; the decoder reads that shape (a nested component with any
other content is `Unsupported`). `func.interface` is empty for a function exported at the top level.
Every resolved name (function, `memory`, `realloc`, `post-return`) is checked to be an export of the
right kind of the same core module, and all of them must come from one core instance.

### Generated methods (A2-2)

`WitOptions` gets `exports: bool` (the CLI flag follows `--glue`). With it, the output also contains

```rust
pub trait Exports {
    fn export_wasi_cli_run_0_2_0_run(&mut self) -> Result<(), ()>;
}
impl<H: Host> Exports for Instance<Glue<H>> { /* ... */ }
```

The method name is the core export name run through the `named_exports` rule
(`src/codegen/export_names.rs`). Each method does the import direction
mirrored:

- **Params are lowered by the host.** Flat values go straight into the core call; params over the
  flat limit are written to guest memory allocated with the lift's `realloc`. The allocation helper
  is shared with the import glue and calls `cabi_realloc`, so a function that allocates with another
  `realloc` name is `Unsupported` for now. The memory is `memory 0` of the instance.
- **The result is lifted by the host**, from the flat result or from the return pointer, under the
  existing per-call `Budget`: the guest is the untrusted side. Then `post-return` is called with the
  flat results, if the lift names one. It is not called after a trap.
- A guest trap stays a panic; the host decides how to contain it (`catch_unwind`, as
  `wasip2_run` does).

The generators are direction-agnostic (`wit_abi.rs`; `glue_abi`, `glue_mem`, `glue_alloc`; the `Src`
and `Case` abstractions of `Gen`). Import glue applies `lift` to params and `lower` to results;
exports apply `lower` to params and `lift` to results, with a new driver beside `glue::write`.
Export funcs are a separate list in `register()`: they are never `Host` methods.

In the external-types mode (P0), export types are registered in the same `Records`, so the types
come from `wasm2rs_wasi::generated`.

### Example (A2-3)

`wasip2_run` calls the typed `wasi:cli/run` instead of scanning export names, so the call is checked
against the component.

### Cleanup (A2-4)

The doc comment of `wit_host_glue` (`src/wit.rs`) still says that strings and lists are
unsupported; the code (`glue_alloc`) implements them. Fix it while the file is open.

## Phases

| Phase | Content |
|---|---|
| A2-1 | ✅ `wit_exports` and `WitExport` (`src/wit/exports.rs`): export section, `canon lift` options, core instance/alias resolution, `Unsupported` for non-direct shapes. |
| A2-2 | ✅ `exports` option and the generated `Exports` trait and impl; mirrored lower/lift driver (`write_exports` in `src/wit/glue.rs`); `post-return`. Tests: `tests/it/wit_glue_exports.rs`. |
| A2-3 | `wasip2_run` uses the typed `wasi:cli/run`. |
| A2-4 | Doc fixes. |

Each phase is its own PR, in this order. Tests extend `tests/it/wasip2_run_example.rs` (typed run:
ok, error result, trap), `named_exports.rs` (the mangled name) and the wit/glue round trips; new
decoder tests build components with `wit-component`.

## What `wasi:http` needs on top (separate design)

`wasm2rs-wasi/src/generated.rs` has all `wasi:http` types and methods, each defaulting to a trap;
nothing is implemented. After A2 a host can call `incoming-handler#handle`, but a guest-facing host
still needs:

- a way for the host to **mint resource handles** (`own<incoming-request>`,
  `own<response-outparam>`): tables for the http resources and a public mint API; today handles are
  created only inside `Host` methods;
- tables and behavior for `fields` (immutability, `header-error`), requests, responses, bodies,
  trailers and futures;
- body streams behind `wasi:io/streams`, tied to the existing handle ranges without clashing;
  pollables for bodies and futures (pollable slots are never freed today);
- `finish` ordering, content-length checks, `response-outparam.set` consumption, and a response sink
  that hands the result to the caller;
- for `outgoing-handler`, an injected HTTP client trait (no TLS dependency by default).

## Risks

- **Trap leaves the guest inconsistent** (memory allocated for the call, instance state). The host
  must not reuse an instance after a trap unless it knows the guest is reentrant-safe.
- **Reentrancy.** The host calls the guest, the guest calls imports with `&mut Instance`, and an
  import may call an export again. `reentrant_imports` already handles the borrows; host state in
  `Glue<H>` needs the same care.
- **Async (component model 0.3)** is not modeled.
