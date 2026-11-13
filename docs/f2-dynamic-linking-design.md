# F2: dynamically linked components (componentize-py), survey and design (stage F2-0)

Status: investigation and design only, no code. Written 2026-11-13 against wasm2rs 0.4.0
(L1-L5, #307-#315, merged).

Background: [component-toolchain-survey.md](component-toolchain-survey.md) (case E, F2, F4),
[core-module-linking-design.md](core-module-linking-design.md) (the linker, decision Q8),
issues #314 (lifts with different reallocs) and #316 (rustc time on the JS component).

Legend: **[verified]** = measured or read on `py-hello.wasm` in this session; **[inferred]** =
my reading, to be confirmed by the stage that needs it.

Fixture: `py-hello.wasm`, 17.6 MB, componentize-py 0.25.1 (`uvx componentize-py -d wit -w w
componentize app -o py-hello.wasm`, see the survey's reproduction section). It is not in the
repository.

## 1. Conclusion

1. **Nothing makes F2 impossible, and the current linker is close.** With four small throwaway
   patches (20 lines added, 10 removed, not committed) `transpile_component` runs to the end on
   `py-hello.wasm`: 0.9 s, a 116 MB / 1.9 M-line `program.rs` with 13,571 functions
   **[verified]**. The four gates are shallow (section 3): a lift's memory check, several
   `realloc` functions for lowered imports, one operator family, and an element-segment offset
   that reads `__table_base`.
2. **The spike output is not correct yet and does not compile.** It picks one `realloc` for all
   lowered imports (wrong for two of three modules), skips two checks, and `rustc` rejects the
   glue for `exports#init` (non-WASI record types, section 3.5, #317). It only shows how far the
   linker gets.
3. **The real blocker is F4, not linking.** The output is the same size class as the JS
   component of #316 (116 MB vs 124 MB), for which `rustc -C opt-level=1` ran over 4 hours
   without finishing. Even a frontend-only `rustc` run on the spike output needed 241 s and
   6.7 GB before it stopped at its first errors, with system time equal to user time
   **[verified]**. End-to-end validation of `py-hello.wasm` is out of reach until F4 is solved.
4. **Recommendation: F4 first for the end-to-end goal; the link-side stages (F2-1 to F2-4) are
   independent of F4, small, and testable with WAT.** If F4 cannot bring this size class to a
   bounded build, F2 stays at the WAT level and we do not claim componentize-py support.
   Section 7 has the order and the decision rule.

## 2. Structure of `py-hello.wasm` [verified]

`wasm-tools print`, `--extract-core` and `wasm2rs`'s own `CoreWiring` (L1) show **14** core
modules, not 12: the survey's 12 leaves out `wit-component`'s shim and fixup. The core
instance number of each is in parentheses; they show the instantiation order (the shim first, the
fixup and `__init` last):

| # | Module | Role | Memory / table | Imports | Exports | dylink.0 (bytes of data, align; table slots) | Funcs |
|---|---|---|---|---|---|---|---|
| 0 | `main` (instance 2) | the dylink "host": the program everything else imports from | **defines** memory (197 pages, 12.9 MB) and table (5806) | 1 func (`wasi_snapshot_preview1`, the shim's `reset_adapter_state`) | memory, table, 395 funcs, 508 globals (see below) | none | 394 |
| 1 | `wasi_snapshot_preview1` adapter (8) | preview1 adapter | imports `env.memory` | 5 funcs | 3 funcs | none | 16 |
| 2 | `wit-component:stubs` (9) | stubs | none | none | 2 funcs | none | 2 |
| 3 | `libc.so` (37) | wasi-libc | imports memory, table | 3 `env` globals, 174 `env` funcs, WASI funcs | 1149 funcs, 32 globals | 212852 B / 2^4; table 81 | 1425 |
| 4 | `libwasi-emulated-signal.so` (41) | libc add-on | imports memory, table | 3 globals, 7 funcs, 2 `GOT.mem`, 2 `GOT.func` | 10 funcs | 1348 B / 2^4; table 4 | 13 |
| 5 | `libwasi-emulated-getpid.so` (43) | libc add-on | imports memory, table | 2 globals, 1 func | 2 funcs | 4 B / 2^2 | 3 |
| 6 | `libwasi-emulated-process-clocks.so` (46) | libc add-on | imports memory, table | 3 globals, 3 funcs, 2 `GOT.mem` | 5 funcs | 24 B / 2^3 | 6 |
| 7 | `libdl.so` (48) | dlopen stubs | imports memory, table | 2 globals, 2 funcs | 6 funcs, 1 global | 408 B / 2^2 | 9 |
| 8 | `libpython3.14.so` (52) | CPython | imports memory, table (min 4955) | 3 globals, 253 funcs, 214 `GOT.mem`, 177 `GOT.func` | 1643 funcs, 214 globals | 3163417 B / 2^6; table 4955 | 10625 |
| 9 | `libcomponentize_py_runtime.so` (58) | pyo3 runtime; **lifts `exports#init`** | imports memory, table | 3 globals, 92 funcs, WASI funcs, `GOT.*` | 106 funcs, 16 globals | 27241 B / 2^3; table 235 | 766 |
| 10 | `libcomponentize_py_bindings.so` (87) | WIT bindings; **lifts `wasi:cli/run#run`** | imports memory, table | 62 funcs, WASI funcs (several lowered with `realloc`) | 5 funcs | 14797 B / 2^2; table 136 | 126 |
| 11 | `__init` (90) | leftover of the build-time init | imports memory, table | 267 `env` globals, 206 + runtime globals, 43 runtime funcs | none | none | 1 |
| 12 | `wit-component-shim-module` (0) | indirect-call shim | **defines** table `$imports` (185) | none | 185 funcs, the table | none | 185 |
| 13 | `wit-component-fixup` (89) | fills the shim's table | imports that table | 185 funcs | none | none | 0 |

`dylink.0` also carries `needed` edges: libc <- signal, getpid, process-clocks, dl, python;
python <- runtime <- bindings.

What this means:

- **One memory, one table, shared by import.** Only `main` defines a memory and a table (the
  shim defines a second, private table of 185 entries, which L3 already handles for adapters).
  Every library imports `env.memory` and `env.__indirect_function_table`. The libraries'
  declared minimums (1 to 49 pages, 0 to 4955 entries) all fit in `main`'s 197 pages and 5806
  entries.
- **`__memory_base` and `__table_base` are constants exported by `main`.** For each library
  `main` exports an immutable `<lib>:memory_base` and `<lib>:table_base` global with an
  `i32.const` initializer, and passes it as that library's `env.__memory_base` /
  `env.__table_base`. So they are known when linking.
- **`GOT.mem` / `GOT.func` are mutable global exports** of `main` and the libraries, imported
  by the libraries. All share one global each once linked.
- **No start function in any module** (no start section; `objdump` of each extracted module).
  `__init` exports nothing and has one function (264 `global.set` of GOT entries and 14 calls
  into library functions) that nothing calls: componentize-py runs `wit-component`'s init
  transform (`component-init-transform` in the producers section), which executed the real
  initialization at build time and froze the result. Evidence: `main` carries the whole
  initialized memory as **1923 active data segments**, the libraries carry **no data
  segments**, and the mutable globals of `main` hold post-relocation addresses (for example
  `3608928`). Instantiation is therefore pure binding: the only active segments are `main`'s
  data, the libraries' element segments at their `__table_base`, `__init`'s element segment at
  5412 (394 entries), and the fixup's at 0 of the shim table. [inferred] A dylink component
  built without that transform keeps its constructors in `__init`; the linker would see them as
  a start function and run them (the L2 start synthesis), but no fixture exercises it.
- **Exports** (`canon lift`): `exports#init` from the runtime library with `memory` of `main`,
  `realloc` and `post-return` of the runtime library; `wasi:cli/run@0.2.0#run` from the
  bindings library. Lifts span two modules, so `select_program` already chooses the linker.
- **Imports lowered with `realloc`**: three different functions, `cabi_import_realloc`
  (adapter), `cabi_realloc` of `libc.so`, and `cabi_realloc` of the bindings library.

## 3. How far the current linker goes [verified]

The linker was widened one gate at a time with throwaway edits (reverted; the diff is not kept).
Each run stopped at the first remaining gate; the table lists them in the order they were hit.
Modules not named were never the reason for a stop.

| # | Gate (message) | Where | Modules involved | Class |
|---|---|---|---|---|
| G1 | `export exports#init: its memory is not an export of the same core instance as the function` | `src/wit/exports.rs` (`Memory` option, `option_export`) | 9 (lifts `init`) with the memory of 0 | lift option resolved by instance, not by linked memory |
| G2 | `the component's imports are lowered with different realloc functions` | `src/component/link.rs` `import_realloc` | adapter (1), `libc.so` (3), bindings (10) | glue has one import `realloc` |
| G3 | `operator i32_trunc_sat_f64_s (saturating_float_to_int) in a linked module` | `src/link.rs` `allowed_proposal` | 3 (38 uses), 6 (2), 8 (162) | operator outside the L2 set; index-free |
| G4 | `element segment offset of a linked module that reads an imported global` | `src/link.rs` `copy_offset` | 3, 4, 8, 9, 10 (each one segment at `__table_base`) | offset is `imports.get_globalN()`, a host expression |

Not a gate, although the issue statement lists them as candidates:

- **Memory or table imported from another module**: resolved to `main`'s (`Source::Memory`,
  `Source::Table`). Nine modules import them (the eight libraries and `__init`).
- **Globals exported by another module** (`__stack_pointer`, `__memory_base`,
  `__table_base`, hundreds of `GOT.*` imports): resolved to the provider's merged global.
- **Cross-module function imports**: about 600 `env` imports, all bound; the
  `call_indirect` types are merged by structural signature.
- **A module with its own memory**: none besides `main`. (The shim's table is the only extra.)
- **Operators outside MVP / bulk-memory / sign-extension / reference-types, other than G3**:
  none. `libpython` has no SIMD, atomics, tail calls or exceptions in its bodies.
- **Global initializers that read imported globals** (100 in `libpython`): re-encoded by
  `rewrite_const`, accepted.
- **Exception tags, GC types**: none.

After G1-G4 the output is generated. It is **not** a correct translation yet:

- G1 was skipped (`memory` taken without a check), so a lift naming a memory of an unrelated
  module would be accepted.
- G2 took the first `realloc` for every lowered import.
- G4 was folded by parsing the generated Rust expression text of the offset and reading the
  provider global's initializer string.
- `rustc` rejects the result (3.5).

### 3.5 A fifth problem, outside linking (#317)

`exports#init` takes a record (`symbols`) of types that no WASI library declares. The glue
writes them as `wasm2rs_wasi::component::Record91` and `Variant69`, which do not exist, so
`rustc` reports nine `E0433`. A tiny non-linked component with a custom record export shows
the same (5 `E0433`), so this is independent of F2; it is tracked in #317 and stage F2-4 only
depends on its resolution.

## 4. Extensions needed, with size

Sizes are lines of Rust including tests (S < 100, M 100-300, L > 300) and are estimates.

| Gate | Extension | Notes | Size |
|---|---|---|---|
| G1 | Resolve the `memory` option of a lift **through the link**: the option's memory is the same linked memory as the function module's memory when both reach the same defining module through `CoreWiring::supplier`. Keep the instance check for single-module components. | Relaxation only on the linked path, so the golden tests are unaffected. A memory option naming an unrelated memory must still fail. `realloc`/`post-return` of a different module are already handled by `name_lifts`. | S |
| G2 | **Per-import `realloc`.** `HostFunc.realloc` already records `(instance, name)` per lowered import; give each distinct one its own unit export (`cabi_import_realloc`, `_1`, ...) and let the glue call the right one per imported function. | The glue emits one `cabi_realloc` / `cabi_import_realloc` today. This is the import-side twin of #314 (lift side); `py-hello.wasm` needs only the import side (`run` does not allocate, `init` has one realloc). | M |
| G3 | Allow `saturating_float_to_int`: the opcodes (`0xFC 0-7`) have no index immediate, so `rewrite_operator`'s raw copy is enough. Other index-free numeric proposals can follow the same rule; anything with a `memarg` (SIMD, atomics) needs a rewrite and stays out. | One line in `allowed_proposal` plus a test. | S |
| G4 | **Link-time constant for an imported-global offset.** Give `SegOffset` a form the linker can resolve (the imported global index instead of only a Rust string), and fold it: follow `Source::Global` to a provider that is **immutable** with a constant initializer (`i32.const`, or a chain of `global.get`/`i32.add`), else keep today's error. Same for table initializers (`TableFill::ImportedGlobal`). | `__memory_base` for a library's data segment is the same case (not hit by `py-hello.wasm`, whose libraries have no data segments, but any dylink component not pre-initialized has them). Changes `parse.rs`, `codegen/info.rs`, `link.rs`; the generated code of single modules must stay byte-identical (host-lent imported globals keep `Runtime`). | M |
| shared table | **Several modules' element segments into the same table**: already works once the table import is resolved to `main`'s (G4 aside). Segments are appended in module order. The table's minimum is `main`'s, which covers all libraries here; a library whose declared table minimum exceeded the provider's would need the merged minimum to be the maximum. | Test to pin it: two libraries with disjoint `__table_base` ranges. | S |
| `__table_base` / `__memory_base` | No new mechanism beyond G4: they are constants of `main` passed as `env.*`. | | included in G4 |
| init order | Nothing for `py-hello.wasm` (no start function, section 2). For a dylink component that keeps constructors, the L2 start synthesis runs module starts in instantiation order, which matches `wit-component`'s. | A test with `__init` as the start function pins it. | S |
| glue types | F2-4 / #317. | | S-M |
| memory/table growth | `memory.grow` and `table.grow` act on the shared memory and table as they do for one module. | No work expected. | - |

Total, excluding F4 and #317: about 500-900 lines with tests across G1-G4 and the pinning
tests.

## 5. Build time and size estimate

Spike output for `py-hello.wasm` [verified]:

| | Value |
|---|---|
| `program.rs` | 116.1 MB, 1,903,981 lines |
| functions | 13,571 (`libpython` 10,625, `libc.so` 1,425, runtime 766, `main` 394, others) |
| lines inside function bodies | 1,273,657 |
| functions over 1,000 / 10,000 lines | 85 / 6 (largest 44,360 lines; five more over 10,000) |
| data | 1,923 `static DATAn: [u8; N] = *b"..."` byte strings (the 12.9 MB snapshot) |
| tables | `table` (5806 `u32`) and the shim's `table1` |

Comparison:

| Component | `program.rs` | `rustc -C opt-level=1` | Peak RSS |
|---|---|---|---|
| B: Rust + adapter (survey) | small | 1.6 s | - |
| C: Go + adapter (survey) | 458 K lines | 168 s | 4.3 GB |
| D: JS, jco (#316) | 124 MB, about 2 M lines | **no result after 4 h**, one huge codegen unit stuck | 7-9 GB |
| E: Python, this spike | 116 MB, 1.9 M lines | not run (see below) | - |

A frontend-only probe (`rustc --emit=metadata -C opt-level=0` on the spike output, against
`wasm2rs-wasi`) was bounded to 7 minutes. It stopped by itself at its first (resolution)
errors after **241 s real (126 s user, 115 s system), 6.7 GB peak RSS**. That is a floor for
the frontend only: it did not reach type checking, MIR or LLVM.

Estimate (unmeasured, stated as a range):

- **`opt-level=1`, single source: hours, 8 GB or more.** Go to JS is 4.4x the lines and at
  least 86x the time (168 s to over 14,400 s), so cost is far from linear; the Python output is
  JS's size class with six functions over 10,000 lines (JS had one unit stuck in LLVM), so
  expect the same outcome. Treat it as not finishing.
- **Frontend alone: minutes, several GB**, with system time as large as user time (memory
  pressure), as for JS.
- Levers to measure before any claim (F4, #316): `-C opt-level=0`, `-C codegen-units`,
  `--split-dispatch=N` on the six large functions (`func5287` at 44 K lines is likely
  `_PyEval_EvalFrameDefault`), `-Z time-passes` to see whether the frontend or LLVM dominates,
  and a split into several crates or files (`--component` refuses split output today; one
  source only). `--entry-exports` reachability will not help much: `libpython`'s functions are
  reachable through the table.

## 6. Stages and minimum tests

Each stage is its own PR (with its own Todo approval), after the code-review and simplify
rule. WAT tests build components with `wat` and `wit-component` (dev-dependencies), as the L
stages did. "G3" below is the byte-identical rule of the linking design (single modules do not
change).

| Stage | Content | Minimum test (WAT) |
|---|---|---|
| F2-1 | G1: memory option resolved through the link | Component with core module `main` (defines memory, table; exports memory) and `lib` (imports `env.memory`, defines and exports `f`); lift `f` with `(memory $main-memory)`. Expect `transpile_component` to succeed and the glue to read memory 0. Negative: a memory option naming a memory that `f`'s module does not share is still `Unsupported`. |
| F2-2 | G2: per-import `realloc` | Two lowered imports that allocate (for example `wasi:cli/environment` `get-environment` and `get-arguments`), each lowered with the `realloc` of a different module that returns a different fixed region. Expect each result written through its own function. Existing single-`realloc` golden output unchanged. |
| F2-3 | G3 and G4 (and the pinning tests) | (a) `lib` with `i32.trunc_sat_f64_s` called from `main`: links and returns the saturated value. (b) `lib` with `(elem (global.get $__table_base) $g)` and `main` doing `call_indirect` at `table_base`, where `main` exports `lib:table_base` as `i32.const 7`: result of `g`. (c) the same for an active data segment at `__memory_base`, read by `main`. (d) two libraries with disjoint `__table_base`. (e) a mutable provider global is refused with a clear error. (f) `__init` as the start function. (g) a three-module dylink-shaped component (`main`, `lib`, a `run` lift in `lib`) executes `wasi:cli/run#run` through `wasip2_run`-style harness and returns the expected exit code. |
| F2-4 | #317: glue for non-WASI types | A component exporting `swap(point) -> point` builds with default options and round-trips a value. (Independent of linking; may land first.) |
| F2-5 | `py-hello.wasm` end to end | `#[ignore]`d test (`WASM2RS_PY_HELLO_COMPONENT`), like L5: transpile, build, run, print `hello from python`; update the survey table. **Blocked on F4**; also needs a decision on whether `run` can be called without first calling `exports#init` ([inferred] it can, because the init transform already ran, but nothing has run it). |

## 7. Feasibility and recommendation

- **Is F2 possible?** Yes as far as linking goes: no fundamental obstacle was found. The shared
  memory, shared table, constant bases and the absence of start functions make this the simple
  case of dynamic linking.
- **Is it worth pursuing?** The link-side work is small (F2-1 to F2-4, a few hundred lines),
  independent of the build-time problem, and also benefits other dylink-style or adapter-based
  components (the `__table_base` fold and the per-import `realloc` are general). The payoff
  that a user sees, a running Python guest, needs F4.
- **F4 first.** `py-hello.wasm` cannot be validated, and any claim about it would be
  untested, until a 116 MB single source builds in bounded time and memory. F4 is already a
  tracked problem (#316) with the same size class. Proposed order:
  1. F4 spike on the spike output above and on the JS component: `-Z time-passes` at
     `opt-level=0`, `--split-dispatch`, codegen units, and what a multi-file or multi-crate
     output would need. Decision rule: if one of these brings a 1.9 M-line source under, say,
     30 minutes and 16 GB, continue; if not, document that componentize-py-sized guests are out
     of reach.
  2. F2-1 to F2-4 (can start in parallel with step 1, since none of them needs the big
     build). F2-4 is also a defect on its own.
  3. F2-5 once F4 gives a bounded build.
- **If F4 fails**, F2-1 to F2-4 are still worth landing as general linker support, but the
  survey keeps case E as "transpiles, not buildable" and nothing promises Python.

## 8. Open points

- The throwaway patches are not kept. Their effect, in order: skip the instance check for the
  memory option; take the first `realloc`; add one proposal; fold the offset by reading the
  provider's initializer text.
- Whether `run` works without `exports#init` is untested (F2-5).
- The correct count of core modules is 14; the survey's 12 is corrected there.
- Not investigated: the speed of the resulting program, and the shape of the host-import glue
  duplicates (`..._13782`: libc and the adapter import the same WIT functions), which already
  work for the adapter case.

## 9. Result of F2-1 to F2-5 (2026-11-13)

All four link-side stages landed (#322 memory option, #324 per-import `realloc`, #326
`trunc_sat` and imported-global segment offsets, #327 glue for export-only types and for
modules without host imports) together with F4-1 (#321). `py-hello.wasm` now goes all the way
[verified]:

| Step | Result |
|---|---|
| `transpile_component` | 1.1 s, 122 MB `program.rs`, no linker error |
| `rustc -C opt-level=1` + link (via `examples/wasip2_run`, release) | 337 s wall in total, 7.5 GB peak RSS |
| run | prints `hello from python`, exit code 0 |

- `wasi:cli/run#run` runs **without** a prior `exports#init` call, as inferred in section 8:
  the init transform had already frozen the initialized memory into `main`'s data segments.
- The ignored test `a_real_componentize_py_component_prints_hello`
  (`WASM2RS_PY_HELLO_COMPONENT`, `tests/it/wasip2_run_example.rs`) pins this; it takes about 330 s.
- Not done: `TableFill::ImportedGlobal` (a table initializer reading an imported global) is still
  refused in a linked module; the fixture does not need it. A mutable provider global cannot be
  named by a constant expression (invalid wasm), so G4's refusal is only reachable for an
  immutable global whose initializer is not a constant (tested in `link::tests`).
- Found on the way: #325 (instantiated modules are dropped when every lift comes from one
  module), #323 and #317 (fixed in #327).
