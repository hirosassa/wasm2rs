# Component input: `transpile_component`

Status: A3-1 (the library API), A3-2 (the examples use it), A3-3 (the CLI flag), A3-4 (`build.rs` and the macro) and A3-5 (the `.wasm` extension guard) are implemented.
Update: two statements below are out of date. "Exports from more than one module are `InvalidInput`"
(decision 4.1) and the last bullet of decision 5 (a program module that imports state from another
core module, as a preview1 adapter does) no longer apply: such components are linked into one
`Instance`, see [core-module-linking-design.md](core-module-linking-design.md) and the CHANGELOG.
Every component that took the single-module path before keeps it, with byte-identical output.
`--component` also accepts `--simd-intrinsics` (A3-3 below lists the flags as of that phase).
Background: [component-exports-design.md](component-exports-design.md) (`wit_exports`, the `Exports`
trait, A2), [typed-host-design.md](typed-host-design.md) (the import glue),
[wasm2rs-wasi-design.md](wasm2rs-wasi-design.md) (the host library).

## Problem

To run a `wasm32-wasip2` component, a caller today has to assemble the pieces by hand
(`examples/wasip2_run.rs`, `examples/wasip2_serve.rs`, `examples/common`):

1. `extract_core_modules`, then pick *the* program module (provisionally "the largest");
2. `transpile_with_options` with `named_imports`, `named_exports` and `reentrant_imports` on (the
   glue and the `Exports` trait call the module by those names);
3. `wit_host_glue_with_options` with `external_types: Some("wasm2rs_wasi")` and `exports: true`;
4. concatenate the two outputs.

Every consumer repeats this, and "the largest module" is a guess: `wit_exports` already knows which
core module lifts each export (`WitExport::core_module`). A3 turns the recipe into one API, then
moves the examples, the CLI and the build-script story onto it.

## Decisions

1. **One function, one source.** `transpile_component(component, &ComponentOptions)` returns a single
   Rust source: the transpiled core module, then the `Glue` (and `Exports`). No file splitting.
2. **The options that the glue depends on are forced.** `named_imports`, `named_exports` and
   `reentrant_imports` are switched on whatever the caller put in `ComponentOptions::transpile`: the
   generated glue does not build without them, so letting a caller turn them off could only produce
   broken output. Every other `TranspileOptions` field is honoured. This is stated in the rustdoc.
3. **Defaults match the example recipe**: `external_types: Some("wasm2rs_wasi")`, `exports: true`.
   `external_types: None` makes the glue declare its own `Host` trait (the `--wit-host --glue`
   shape).
4. **Module selection, in this order.**
   1. If the component exports functions, the module is the one `wit_exports` names
      (`core_module`). Any other core module (shim, fixup, adapter) is ignored: the glue reaches
      the program through the lifted exports, and the lowered imports are bound by name.
      Exports from more than one module are `InvalidInput`.
   2. Otherwise the module is the one instantiated with the core functions the component lowers
      (`canon lower`, resource drops, aliased shim functions). A module that exports no function
      and has no start function is wiring (the fixup module of `wit-component`) and is not a
      candidate. A component with exactly one core module needs no analysis.
   3. If no single module results, the error is `InvalidInput` and says why (how many candidates, or
      that there is no core module at all).
5. **Out of scope (`Unsupported`, with a message):**
   - components that link several core modules through a shim or adapter *and* export nothing, so
     the program cannot be told from the plumbing;
   - an export of a nested component (inherited from `wit_exports`);
   - string encodings other than UTF-8 (inherited from `wit_exports`);
   - input that is a core module rather than a component: the error says to use `transpile`;
   - a program module (the one that lifts the exports) that imports a memory, table or global from
     another core module, as the adapter of a preview1-adapter component does: the error says that
     linking several core modules is not supported (it used to surface as a `reentrant_imports`
     options error).
6. **A broken input is an `Err`, never a panic.**
7. **Index spaces.** `WitExport::core_module` counts *top-level* core modules only, while
   `extract_core_modules` also lists the modules of nested components, in appearance order. The two
   differ as soon as a nested component precedes the program, so `transpile_component` does its own
   top-level scan instead of indexing into `extract_core_modules`.

## API

```rust
pub struct ComponentOptions {
    /// `named_imports`, `named_exports` and `reentrant_imports` are forced on.
    pub transpile: TranspileOptions,
    /// Rust path of the library that declares `Host` and the types (default `Some("wasm2rs_wasi")`).
    pub external_types: Option<String>,
    /// Also generate `Exports` (default `true`).
    pub exports: bool,
}

pub fn transpile_component(component: &[u8], options: &ComponentOptions)
    -> Result<String, TranspileError>;

/// The same, also returning the lifted `Vec<WitExport>` (#212).
pub fn transpile_component_with_exports(component: &[u8], options: &ComponentOptions)
    -> Result<(String, Vec<WitExport>), TranspileError>;
```

## Phases

Each phase is its own PR, in this order.

| Phase | Content |
|---|---|
| A3-1 | ✅ `transpile_component` and `ComponentOptions` (`src/component.rs`), the selection above. Tests: `tests/it/component_input.rs`. |
| A3-2 | ✅ `examples/wasip2_run.rs` and `wasip2_serve.rs` call `transpile_component`; the duplicated assembly in `examples/common` goes away (the `rustc` build helper stays). `common::transpile_exporting` calls `transpile_component_with_exports` and checks that the wanted export is among the returned ones (the caller no longer runs its own `wit_exports` pass before `transpile_component`). |
| A3-3 | ✅ CLI `--component`: an explicit flag (a component is still rejected without it, as today). It switches on `--named-imports --named-exports --reentrant-imports` and the external-types glue with the typed `Exports` (`WitOptions::exports`, which has no CLI flag) together and writes one source (stdout without an output). Combines with `--unsafe-memory`, `--jobs=N`, `--split-dispatch=N`, `--call-depth-limit=N` and `--external-types=<path>` (all reach `ComponentOptions`); everything else, including `--entry-exports`, `--import-memory` and the split arguments, is refused by name. The CLI always uses an external types library (default `wasm2rs_wasi`); a self-declared `Host` is library-only (`external_types: None`). Tests: `tests/it/component_input.rs`. |
| A3-4 | ✅ `wasm2rs::build::transpile_component_to_out_dir(path, &ComponentOptions)` writes `$OUT_DIR/{file stem}.rs` (and prints `cargo:rerun-if-changed`); `transpile_component_to_dir` takes the directory. `wasm2rs_macros::include_component!("{file stem}")` is `include!` of that file (a proc macro next to `include_wasm!`: a `macro_rules!` in `wasm2rs` would make a build-dependency-only crate a runtime dependency). A core module or a missing file is a `BuildError` and writes nothing. Tests: `tests/it/build_helper.rs`, `wasm2rs-macros/tests/include_component.rs`. |
| A3-5 | ✅ `include_component!("run.wasm")` (a name ending in `.wasm`, any case) is a `compile_error!` that names the fix (`include_component!("run")`) instead of a "file not found" for `run.wasm.rs`; `foo.bar` stays a valid name (the stem of `foo.bar.wasm`). Tests: unit tests in `wasm2rs-macros/src/lib.rs`, and `tests/it/include_component_project.rs`, a real Cargo project (`build.rs` → `transpile_component_to_out_dir` → `include_component!` → `wasm2rs-wasi` → run the component) plus the extension error through `cargo build`. |

**Known constraint (A3-4/5):** the macro cannot see the build script's input path, so the name must match the file stem by hand; a name that matches no `{name}.rs` in `$OUT_DIR` is rustc's `include!` error naming the missing file.

## Findings on real components

Checked with `rustc --target wasm32-wasip2 -O` output (three core modules each: the program,
`wit-component`'s shim and fixup; the program is the largest and first):

| Input | `wit_exports` | Selected |
|---|---|---|
| `fn main() { println!(..) }` (exports `wasi:cli/run@0.2.0`) | `run`, `core_module` 0 | module 0; glue and `Exports` generated |
| `cdylib` with only a `#[no_mangle]` core function (no component export) | empty | module 0 by wiring: the program and the fixup both receive lowered functions, the fixup exports nothing and is skipped |

Nothing needed to be `Unsupported` beyond the cases in Decision 5. The export-less rule depends on
the fixup having no exports; a differently shaped adapter that exports functions would make the
candidates ambiguous, and the error then names the module positions.

## Risks

- **Selection by wiring is a heuristic for export-less components.** It is exact for what
  `wit-component` emits (the program receives lowered and aliased functions, the fixup does not),
  and fails loudly (`InvalidInput`) when it cannot decide, rather than guessing.
- **Forced options change the module's API** relative to a plain `transpile`. That is the point;
  callers who want a different shape use the lower-level functions.
