# Linking several core modules (F1 step 2)

Status: design only, nothing implemented.

Background: [component-toolchain-survey.md](component-toolchain-survey.md) (F1/F2),
[component-input-design.md](component-input-design.md),
[component-exports-design.md](component-exports-design.md), issues #184, #185, #186
and the F1 step 1 error from #305.

Legend: **[verified]** = read in the code or measured on a real component in this
session; **[inferred]** = my reading, to be confirmed by the first PR of the
stage that depends on it.

## 1. Problem

`transpile_component` picks **one** core module (the one that lifts the exports)
and ignores every other core module of the component (`src/component.rs`,
`select_module`; the doc says other modules are not emitted) **[verified]**.

Two real shapes break because of that:

- **Adapter components** (`wasm-tools component new --adapt`; Go, wasi-sdk C/C++,
  old cargo-component). The lifted `wasi:cli/run#run` lives in the adapter, so the
  adapter is chosen, and it imports `env.memory` from the user's module. Since #305
  this is a clear `Unsupported`; before it was a misleading `reentrant_imports`
  error.
- **Exports spread over several modules** (#185). `Exports` is generated for one
  `Instance`, so `WitExport::core_module` must be unique.

### What an adapter component looks like (measured on `p1-hello.wasm`)

`wasm-tools print` of the fixture built from a wasip1 Rust program **[verified]**:

| core module | role | notable content |
|---|---|---|
| 0 `$main` | the user program (2.2 MB) | imports 6 `wasi_snapshot_preview1.*` functions; exports `memory`, `_start` |
| 1 adapter | preview1 -> preview2 (13 KB) | imports `env.memory`, `__main_module__._start` and ~20 lowered `wasi:*@0.2.3` functions; exports the preview1 functions, `wasi:cli/run@0.2.3#run`, `cabi_import_realloc`; 3 mutable globals, a **`(start $allocate_stack)`**, no data, no table, no elem, no own memory |
| 2 shim | trampolines | functions `"0"`..`"17"` that `call_indirect` through an exported table `$imports` |
| 3 fixup | table filler | imports `""."0"`..`"17"` plus the table, one active `elem` filling slots 0..17 |

Instantiation order in the component: shim, then `$main` (its
`wasi_snapshot_preview1` import is the **shim** instance), then the adapter
(its `env.memory` / `__main_module__._start` come from `$main`'s exports, the
`wasi:*` imports from shim trampolines), then fixup (fills the shim table with the
adapter's preview1 exports and with the `canon lower`ed host functions).
The shim/fixup pair exists only to break the main <-> adapter cycle, because core
instances cannot be cyclic. The lowered functions use `$main`'s memory and the
adapter's `cabi_import_realloc` as their realloc **[verified]**.

Consequences for any design:

1. The program is **main + adapter (+ shim + fixup)**, and main's calls reach the
   adapter only through a table.
2. The memory is main's; the adapter has none of its own.
3. The adapter has a start function that must run after main is instantiated.
4. Host calls (`wasi:*`) enter through `canon lower` functions that are reached
   via shim/fixup, so the name the glue needs (`wasi:cli/environment@0.2.3` /
   `get-arguments`) is on the **adapter's** import and in the shim export name, not
   on the fixup's `""."6"` import.
5. Lowering with `realloc = cabi_import_realloc` conflicts with the glue's fixed
   `cabi_realloc` (#184).

## 2. Goals and non-goals

Goals

- G1. A wasip1-derived adapter component (`p1-hello.wasm`, `go-hello.wasm`) runs
  through `transpile_component` with the existing `wasm2rs-wasi` host.
- G2. Resolve #185 as a consequence (lifts from several modules of one linked
  program), and settle the naming question of #186 for linked programs.
- G3. A **single-module component or core module produces byte-identical output**
  (the golden tests in `tests/it/golden.rs`, `layout.rs`, `single_file_pieces.rs`,
  `parallel_codegen.rs` stay unchanged).

Non-goals (for this work)

- Dynamic linking (F2, componentize-py, 12 modules). Not a non-goal of the *design*:
  the linker below is written for N modules and an arbitrary wiring graph, so F2 is
  a later stage of the same mechanism (shared imported table/globals as
  `__table_base`, `dlopen`-style init order, indirect calls across modules). It is
  explicitly **not** promised here.
- Separate compilation (one Rust crate per core module).
- A general-purpose Wasm linker or optimiser.
- Nested components as inputs (stays `Unsupported`, input design decision 5).

## 3. Where "1 module = 1 Instance" is baked in **[verified]**

| Area | Where | Single-module assumption |
|---|---|---|
| Entry points | `src/lib.rs` `transpile_impl` (L563), `transpile_with_host_imports` (L444) | one wasm -> one `ModuleIr` -> one `ModuleParts` |
| IR / codegen context | `src/parse.rs` `ModuleIr` (L61); `src/codegen/driver.rs` `ModuleParts` (L277), `ModuleCtx` (L49) | func/global/memory/table/tag/type index spaces are module-local; `funcN`, `g{N}`, `DATA{d}` carry no module id |
| Generated names | `render.rs` | fixed `Instance`, `Imports`, `SharedMemory`, `GcRef`, `Wasm2RsException`; `export_names.rs` `FIXED`/`is_reserved` |
| State | `render.rs` `instance_field_lines` | memory/table/globals/data/elem are **fields of the one `Instance`** |
| Memory sharing | `render.rs`, `parse.rs` | imported memory is memory 0 only, lent by the host via `Imports::memory()`; the only thing shareable between `Instance`s is `SharedMemory` (Arc); no way to share a non-shared `Vec<u8>` |
| Imports | `render.rs` `import_trait_lines` | one `Imports` trait; `reentrant_imports` takes `&mut Instance<Self>` of the *caller* |
| Names | `export_names.rs` | import/export identifiers are assigned over **the whole module's** export list (this is why `glue_text` rejects a lift whose name differs from the standalone name; #186) |
| Component selection | `component.rs` `select_index` (L166), `Layout`/`scan` (L207/L218) | picks one module; `scan` keeps only a 1-bit "holds state" per core instance and **drops the instantiate argument graph** |
| Lifts | `src/wit/exports.rs` `WitExport::core_module`; `emit.rs` `glue_text` (L880) | one core module; the check exists twice |
| Glue | `src/wit/glue.rs` | hard-wired to `Instance<Glue<H>>`, `g.memory()`, `g.cabi_realloc(..)` (`REALLOC = "cabi_realloc"`) |

Dependencies are `wasmparser` only (plus optional `mimalloc` for the CLI binary);
`wit-component`/`wit-parser`/`wat`/`wast` are dev-dependencies. That is the
constraint for option (a).

## 4. Options

All options must handle the four-module structure above; they differ in *where*
the join happens.

### (a) Fuse at the Wasm binary level, then transpile the result

Produce one core module (walrus / `wasm-tools`-like), feed it to the unchanged
`transpile_with_host_imports`.

- Pro: codegen, glue and the Instance model are untouched; byte-identical output is
  trivially kept; it is the conceptually simplest pipeline.
- Con: needs a real re-encoder. Resolving main's six `wasi_snapshot_preview1`
  imports removes imports from the front of main's function index space, so
  **every function index inside main (2 MB, any proposal) is renumbered**.
  That means rewriting every index-bearing operator across the proposals we
  support (`call`, `ref.func`, `global.*`, `table.*`, `call_indirect`, block-type
  type indices, `return_call*`, GC/EH/cont types...). Doing that by hand is large
  and fragile; `wasm-encoder` (`reencode`) would make it easy but adds a
  dependency, against the "wasmparser only" selling point.
- Effort: large without a dependency, small with one. Risk: miscompiles from a
  missed operator; mitigated by failing loudly on unknown operators.

### (b) One `Instance` per module, wired through Rust calls

Generate each module in its own Rust `mod`, make the adapter's `Imports` call
main's `Instance`.

- Memory: the adapter's `env.memory` is main's `Vec<u8>` owned by main's
  `Instance`. The adapter is entered from main (`fd_write`) while main is
  mutably borrowed, and the adapter calls main's `_start` back. That is mutual
  `&mut` recursion between two instances; it needs either `SharedMemory`
  (atomic-backed `Arc`, a cost for every access and a different memory type for
  non-shared programs) or `memory` passed as a parameter through every adapter
  function (a new calling convention in codegen).
- Needs name-spacing of all fixed runtime names (`Instance`, `Imports`, `GcRef`,
  `DATA{d}`...), a cross-module import trait, and a cross-module table story
  (shim/fixup share one table between *three* modules).
- Glue keeps `Instance<Glue<H>>` for exactly one instance, so lifts/lowers
  (memory + realloc from different modules) need a selector (#185's "one Instance
  per module or a module selector").
- Pro: modules stay separately generated and a future per-module cache is
  possible. Con: largest change, slowest generated code (indirection on each
  cross-module call), highest risk. Effort: large.

### (c) Link at the `ModuleIr` level into **one** `Instance` (recommended)

A new pass takes the component's core modules plus the wiring read from the
component and produces **one `ModuleIr`/`ModuleParts`**; everything downstream
(codegen, glue, `Exports`) already handles "one program module".

Core idea: do not renumber the big module. Main keeps all its indices.

- Merge order: modules in instantiation order, the **first instantiated module
  that owns the program state (main) keeps its index spaces unchanged**; the
  others are appended with an offset per index space (types, funcs, globals,
  tables, tags, data/elem segments).
- A resolved import (main's `wasi_snapshot_preview1.fd_write` -> shim export
  -> table slot -> adapter export) is **not removed from the index space**.
  It stays as an import entry with an *alias* "this import is function `K` of the
  merged module", and `ModuleCtx::call_expr` / `ref.func` emit a direct
  `self.funcK(..)` instead of a host call. That avoids shifting main's indices.
- Memory: every `memory` import of a linked module that is satisfied by another
  module's memory export is bound to that memory index (memory 0). The adapter's
  `env.memory` becomes main's memory 0; no `Vec<u8>` is shared across instances
  because there is only one.
- Globals/tables/elem/data of appended modules become ordinary extra entries
  (`g{global_base+i}` already exists as a concept); the shim table and fixup's
  `elem` merge naturally: shim's table is defined, fixup's imported table is bound
  to it, and its active `elem` runs at instantiation in order.
- Start functions: run in component instantiation order (main's, then the
  adapter's `allocate_stack`, then none for shim/fixup).
- Appended modules' function bodies need their own index operands translated.
  That is a *small* renumbering (the adapter is 13 KB, Rust-compiled MVP+bulk) over
  a restricted operator subset that **fails loudly on anything else**; or, if
  `ModuleIr` bodies are lazily decoded (to be checked), a per-body index bias in
  codegen with no rewriting at all.
- Output: one `Instance`, one `Imports` trait that only contains the *unresolved*
  imports (the `wasi:*` lowered functions + any host import), so the glue and the
  `wasm2rs-wasi` host work as they do for a wasip2-native component.
- Pro: no new dependency; no borrow problem; direct calls (the trampoline
  `call_indirect` through the shim table remains but is one table lookup);
  solves #185 (a lift into any merged module is just an export of the one
  `Instance`); F2 extends it.
- Con: needs a trustworthy mapping from "a shim/fixup-routed lowered function" to
  its WIT name (see Section 6, Q2); size grows by the adapter (13-23 KB of Wasm,
  small against 2.2-2.5 MB main); compile time is dominated by main anyway.
- Effort: medium-large (the linker plus component wiring), risk medium and
  localised: the single-module path is not touched.

### Comparison

| | (a) binary fuse | (b) N instances | (c) IR link, 1 instance |
|---|---|---|---|
| New dependency | `wasm-encoder` or hand re-encoder of all operators | none | none |
| Renumbering of the big module | yes (or a dependency) | no | **no** (alias imports) |
| Memory/table/global sharing | natural | hard (borrows, `SharedMemory`, params) | natural |
| Generated code speed | best | worst | good |
| Output size | + adapter | + adapter + dup. runtime per module | + adapter |
| Default single-module output | byte-identical | byte-identical if gated | byte-identical if gated |
| Touches glue/Exports | no | yes (selector) | no (names only) |
| #185 | yes | needs selector | yes |
| #184 | still open | still open | still open (see 5) |
| #186 | merged module's exports, same as today | per-module, easier | naming over merged exports, see Q4 |
| Effort / risk | large (or dep.) / medium | large / high | medium-large / medium |

## 5. Recommendation: (c), staged

Gate: the linked path is used **only** when `select_index` today would fail
(program module imports state from another core module, or lifts span several
modules, i.e. exactly the cases #305 and #185 reject). Everything that succeeds
today takes the current path, so existing output is byte-identical and the golden
tests are the regression guard (G3).

Honest linking of all four modules (including shim and fixup) is preferred over
special-casing `wit-component`'s pattern: it also covers adapters that differ from
today's and #185, and it needs no "is this really a trampoline" detection. The one
non-trivial part is naming the host imports (Q2). #184 must be solved on the way
(lowered functions use `cabi_import_realloc`), so it is part of stage L3.

### Stages (one PR each; first failing test given)

Tests are written as WAT components inside the test function (no fixtures). The
dev-dependency `wit-component` can assemble an adapter component from a WAT main
module and a WAT adapter via `ComponentEncoder::adapter`, which gets the exact
shim/fixup wiring for free; a hand-written WAT with the same shape is the
fallback.

- **L0 (spike inside L1's PR, no merge on its own)**: confirm `ModuleIr` keeps
  operators as `wasmparser::Operator`/ranges and whether a per-body index bias or
  a restricted rewrite is cheaper. Decides the "Appended modules" line above.
- **L1 `scan` keeps the instantiate graph** (refactor, no behaviour change).
  `Layout` records, per core instance, which module it instantiates and, per
  import name, which (instance, export) feeds it; `CoreInstance::FromExports`
  keeps its member list.
  *First failing test*: unit test on the adapter-shaped WAT component asserting
  the graph (`$main.wasi_snapshot_preview1` <- shim exports; adapter `env.memory`
  <- `$main` memory; fixup table <- shim table). Existing #305 test stays green.
- **L2 `src/link.rs`: link N core modules into one `ModuleIr`** with alias
  imports, memory/table/global binding and start order; pure core level.
  *First failing test*: a two-module WAT pair (module B imports `a.f` and a
  memory exported by A; A's `_start` calls B) linked and executed through the
  existing build helper returns the expected value; and a test that linking a
  single module returns the input unchanged. A direct Rust-level test on the
  linker with the shim/fixup-style cycle (table + active elem).
- **L3 Component integration + glue**: use the linker when the gate fires;
  resolve host-import names through shim/fixup (Q2); lowering with
  `cabi_import_realloc` (#184: pass the realloc export name to the shared
  helper); remove the duplicated single-module check in `glue_text`.
  *First failing test*: the #305 WAT test becomes a success test: the
  adapter-shaped component transpiles, builds, and `run` executes against a test
  `Host` that records `get-stdout`/`write` (replacing today's `Unsupported`
  assertion).
- **L4 #185 and #186 for linked programs**: lifts in different modules become
  exports of one instance; name assignment over the linked export list with the
  rule from Q4.
  *First failing test*: a WAT component whose two lifted exports come from two
  different core modules (the shape #185 describes) returns both results.
- **L5 real components**: `#[ignore]`d tests (fixtures not committed) for
  `p1-hello.wasm` and `go-hello.wasm` run through `examples/wasip2_run`;
  update the survey table (B and C pass). Reproduction is at the end of the survey.
- **F2 (later, not scheduled)**: dylink-style modules; needs L2's imported
  table/global binding with `__table_base`/`__memory_base`, init order, and
  libpython-scale compile time work (F4).

## 6. Open questions (need your decision)

1. **Scope of "linking"**: accept the recommendation to link all modules of the
   instantiation graph honestly (shim + fixup included, one extra `call_indirect`
   per preview1 call), or special-case the `wit-component` shim/fixup pattern and
   bind main's preview1 imports directly to the adapter exports (faster, but
   pattern-dependent)?
2. **Naming host imports behind shim/fixup**: the lowered functions arrive at
   fixup as `""."6"`, with no WIT name. Options: (i) derive the name from the
   component's `canon lower` definition (needs the component reader to retain the
   core-func -> component-func mapping; accurate), or (ii) use the adapter's
   import names (`wasi:cli/environment@0.2.3`/`get-arguments`, which is what the
   current by-name resolver already understands; pattern-dependent). Preference?
3. **Gate**: link only when today's code would reject (recommended, keeps every
   current output byte-identical) vs. always link when a component has more than
   one *instantiated* module (cleaner, but changes existing successful outputs
   for components with ignorable shims).
4. **#186 naming for a linked program**: names are assigned over the exports of
   the linked unit. Which exports does the unit have: all exports of all
   merged modules (collision-prone, `_{func_index}` suffixes are not stable) or
   only those the component re-exports / lifts plus `memory`/`cabi_realloc`
   (recommended; internal wiring exports are dropped)?
5. **Dependency policy**: confirm "no new runtime dependency" is a hard rule. If
   not, (a) with `wasm-encoder` becomes the smallest-effort option and the
   alias-import machinery in (c) is unnecessary.
6. **Appended-module operator subset**: acceptable to fail loudly when an appended
   (non-main) module uses operators outside MVP + bulk-memory + sign-ext +
   reference types, given that the only known adapters are Rust-compiled? Or must
   it support every proposal (then the per-body-bias route of L0 is required)?
7. **Component-level state**: the adapter's `__stack_pointer`, `internal_state_ptr`,
   `allocation_state` are plain mutable globals initialised in `allocate_stack`.
   Confirm that the merged `Instance` may expose them as ordinary private fields
   (no host visibility needed).
8. **F2**: keep as a documented follow-up of the same linker (this design), or
   drop it from the roadmap until there is demand?
