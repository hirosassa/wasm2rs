# Component exports (`canon lift`): typed calls into a guest

Status: A4 (resources the component exports) is below. A2-1 (the decoder), A2-2 (the `Exports` trait), A2-3 (`wasip2_run` calls the typed `run`) and A2-4 (cleanup) are implemented. The `wasi:http` host this document defers is implemented too (see [wasi-http-design.md](wasi-http-design.md)).
Update: the single-instance limits below were lifted by core module linking. Exports lifted from
several core modules, a lift whose `memory` option is a memory the module imports from the program,
and lifts from a preview1 adapter module (a direct export of the instantiated adapter) are supported for `transpile_component` (the
modules are linked into one `Instance`, see [core-module-linking-design.md](core-module-linking-design.md)).
Still refused: lifts that allocate through different `realloc` functions of several modules (#314).
`wit_exports` itself still reads only a lift that is a direct export of an instantiated core module.
Background: [wasm2rs-wasi-design.md](wasm2rs-wasi-design.md) (the host library, A1, done).

## Problem

`--wit-host --glue` covers the **import** side of a component (`canon lower`): the generated
`Glue<H: Host>` lifts the guest's flat arguments into typed host calls. The **export** side
(`canon lift`) is not read at all:

- `wit_functions` (`src/wit.rs`) parses the export section only for `Type` and `Instance` kinds, to
  grow the index spaces. Func exports, `canon lift` and its options (`memory`, `realloc`,
  `post-return`, string encoding) are skipped, and so are the core instance section and core-func aliases.
- (Before A3-2) `examples/wasip2_run.rs` found `wasi:cli/run@0.2.0#run` by scanning the largest core module's
  export names and called `inst.func{N}()`; it never checked that name against a `canon lift`, nor the
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
    pub encoding: WitStringEncoding, // Utf8 | Utf16; compact-utf16: Unsupported
}
```

As implemented: `wit-component` exports an interface as an instance of a small *shim component* that
only re-exports its function imports; the decoder reads that shape (a nested component with any
other content is `Unsupported`). `func.interface` is empty for a function exported at the top level.
Every resolved name (function, `memory`, `realloc`, `post-return`) is checked to be an export of the
right kind of the same core module, and all of them must come from one core instance.

### Generated methods (A2-2)

`WitOptions` gets `exports: bool` (there is no CLI flag: `--wit-host --glue` leaves it off, `--component` turns it on). With it, the output also contains

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

`wasip2_run` generates its glue with `WitOptions { exports: true, .. }` and `main` calls the
generated `Exports::export_wasi_cli_run_0_2_0_run`; the `Result<(), ()>` gives the exit code (ok 0,
err 1). The call is checked against the component, and the example no longer scans core export
names (`run_export`, `wasmparser`).

- **No `run`.** Before the build, `wit_exports` must contain `wasi:cli/run@0.2.0` `run`. If it does
  not, the example says so (`the component does not export ...`) and exits 2, without a panic.
  A shape `wit_exports` does not read (an adapter or a nested component that does more than
  re-export) is `Unsupported`; its message is printed as is, and the exit code is 2. The same holds for a glue
  error caused by another export (the glue has a method for every export, not only `run`), so
  every function the component exports must be supported for the example to run it.
- **Core module.** (As of A2-3 the largest one.) `WitExport::core_module` indexes the *top-level* core
  modules, while `extract_core_modules` also returns the modules of nested components, so the two
  index spaces differ. Since A3-1, `wasm2rs::transpile_component` selects the module by
  `core_module` (exports from more than one module are `Unsupported`), and `wasip2_run` uses it.
- **Unchanged.** Trap = 134, `wasi:cli/exit` as before. A real `wasm32-wasip2` component lifts
  `run` directly, so it takes this path.

### Cleanup (A2-4)

The doc comment of `wit_host_glue` (`src/wit.rs`) claimed that strings and lists were
unsupported although `glue_alloc` implements them; A2-2 rewrote it. A2-4 only fixed what was left:
a broken sentence in the README's `--glue` row and the stale "example" wording below.

## Phases

| Phase | Content |
|---|---|
| A2-1 | ✅ `wit_exports` and `WitExport` (`src/wit/exports.rs`): export section, `canon lift` options, core instance/alias resolution, `Unsupported` for non-direct shapes. |
| A2-2 | ✅ `exports` option and the generated `Exports` trait and impl; mirrored lower/lift driver (`write_exports` in `src/wit/glue.rs`); `post-return`. Tests: `tests/it/wit_glue_exports.rs`. |
| A2-3 | ✅ `wasip2_run` uses the typed `wasi:cli/run` (`exports: true`); no-`run` and non-direct shapes exit 2 with a message. Module selection by `core_module` is A3. Tests: `tests/it/wasip2_run_example.rs`. |
| A2-4 | ✅ Doc fixes (README `--glue` row, `wasm2rs-wasi-design.md` background). |

Each phase is its own PR, in this order. Tests extend `tests/it/wasip2_run_example.rs` (typed run:
ok, error result, trap), `named_exports.rs` (the mangled name) and the wit/glue round trips; new
decoder tests build components with `wit-component`.

## Resources a component defines and exports (A4)

Status: implemented (`tests/it/wit_glue_resource_exports.rs`). Before it, a component that exports
a `resource` transpiled and produced a typed `Exports` trait, but the program did not build: the core
module imports `[export]<interface>` `[resource-new]<r>`, `[resource-rep]<r>` and
`[resource-drop]<r>` (what `wit-component` makes of `canon resource.new`, `resource.rep` and
`resource.drop`), and the glue had no `Imports` method for them.

### Decisions

1. **Where the table lives.** The handle table of the guest's own resources is state of the guest
   instance, so it is the glue's: `Glue<H>` becomes `Glue<H>(pub H, pub ResourceTable)` for a
   component that defines a resource (every other component keeps `Glue<H>(pub H)`), and the
   glue adds `Glue::new(host)`. `resource.new(rep)` makes a handle (a slot's position plus one, so
   0 is never valid; freed slots are reused), `resource.rep(handle)` reads the representation back,
   and `resource.drop(handle)` ends it. A handle of another resource or a handle that is not live
   traps (`Wasm2RsGlueTrap::InvalidResourceHandle`).
2. **What the host holds.** The host holds a resource as its **representation**: `ResourceN(rep)`.
   This is what the canonical ABI gives a caller that is not the defining component: an `own<r>`
   result leaves the guest's table (the lift takes the handle out and keeps the `rep`), and a
   `borrow<r>` argument reaches a callee that defines `r` as the representation itself, with no
   table at all. An `own<r>` argument is the only direction that makes a handle: the glue calls
   `resource.new`'s equivalent (`rt.new_handle`) for it and passes the handle.
   A `rep` the host invents is a value the guest dereferences, as in any component-model host; it
   is memory safe (the generated code is safe Rust) but not checked.
3. **`own` and `borrow` in the typed signatures.** Both are the same `ResourceN` type, as for
   imported resources (see `typed-host-design.md`). `[constructor]r`, `[method]r.f` (the `self`
   parameter is `self_`) and `[static]r.f` are ordinary `Exports` methods named like their core
   exports (`t_p_i_constructor_counter`, `t_p_i_method_counter_get`, ...). Handles that sit in a
   result read from memory (`result<r, string>`, a record) are taken out of the table too: the glue
   moves the table out of `Glue` around the lift (`glue_take_resources` / `glue_put_resources`),
   because the lift holds a borrow of guest memory.
4. **Destructors.** The guest's `resource.drop` ends a handle, and the destructor runs after the
   guest call that dropped it has returned, not inside the import: the host is lent out while an
   import runs, so a destructor that dropped another handle (a field of `r` that is a resource)
   would panic with "re-entrant import call". The host drops an owned value it holds with the new
   `Exports::drop_<resource>(&mut self, handle)`, which calls the destructor with the
   representation (and nothing else, if the resource has none). The destructor is the core
   export `<interface>#[dtor]<resource>`: `wit-component` reaches it through its shim table, so the
   export is named by the convention of `wit-component` and `wit-bindgen`, and only for a resource
   whose `canon` type definition has a `dtor`.
5. **What is not covered.** An imported function that takes or returns a resource the component
   defines itself is `Unsupported` (the guest would hand its own handle to the host). The linked path
   (`Program::Linked`, several core modules) is not tested with an exported resource. Resource
   exports with `async` lifts are not covered (WASI 0.3).

### How it is read

`Reader` (`src/wit/exports.rs`) records `canon resource.new/rep/drop` of a resource that the
component defines (`(type (resource (rep i32) (dtor ..)))`, tracked by `read.rs` as
`Scope::local_dtors`) as `Supplier::LocalResource`, and `local_resource_imports` lists, from the
core instance wiring, the core imports that these are (`[export]t:p/i` `[resource-new]counter`).
Imported resources keep their existing `[resource-drop]` handling (a `Host` method).

### Reproduction of the real guest

The ignored test `a_real_wit_bindgen_guest_exporting_a_resource_is_driven_through_the_typed_exports`
(`WASM2RS_RESOURCE_GUEST`) drives a `wit-bindgen` 0.63 Rust guest built for `wasm32-wasip2`:

```wit
package test:counter;
interface counters {
  resource counter {
    constructor(start: u32);
    get: func() -> u32;
    add: func(n: u32);
    make: static func(n: u32) -> counter;
    sum: static func(a: borrow<counter>, b: borrow<counter>) -> u32;
    consume: static func(c: counter) -> u32;
  }
}
world w { export counters; }
```

A `cdylib` crate with `wit-bindgen = "0.63.0"`, `wit_bindgen::generate!({ world: "w", path: "wit" })`,
`struct MyCounter(Cell<u32>)` implementing `GuestCounter` (`new`, `get`, `add`, `make`, `sum` and
`consume` over the `Cell`), then `cargo build --release --target wasm32-wasip2`. The program
prints `42`, `47` and `5` and survives 1000 construct/drop cycles (checked 2026-11-16).

## What `wasi:http` needs on top (separate design)

This section is the original requirements list, written when `wasm2rs-wasi/src/generated.rs` had all
`wasi:http` methods defaulting to a trap. `WasiHost` now implements them (see
[wasi-http-design.md](wasi-http-design.md) for the design and status). After A2 a host can call
`incoming-handler#handle`, but a guest-facing host needed:

- a way for the host to **mint resource handles** (`own<incoming-request>`,
  `own<response-outparam>`): tables for the http resources and a public mint API; today handles are
  created only inside `Host` methods;
- tables and behavior for `fields` (immutability, `header-error`), requests, responses, bodies,
  trailers and futures;
- body streams behind `wasi:io/streams`, tied to the existing handle ranges without clashing;
  pollables for bodies and futures (pollable slots are freed on drop and reused);
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
