# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking (example):** `examples/wasip2_run.rs` now takes a `wasm32-wasip2` component
  (`cargo run --release --example wasip2_run -- <component.wasm> [args...]`), not an
  extracted core module. It runs the component's largest core module through the typed host
  `examples/wasip2_host/typed.rs` and the generated glue (`wit_host_glue`) instead of
  `examples/wasip2_host/host.rs` (which stays in the repository). A component importing
  something `typed.rs` does not implement fails to build with rustc's E0046 errors and a hint
  (exit code 2). Tested end to end in `tests/it/wasip2_run_example.rs`.

### Added

- `examples/wasip2_host/typed.rs`: a WASI preview2 host written against the generated typed `Host`
  trait (no canonical-ABI code): stdout/stderr, the environment, exit, output streams,
  pollables, `poll`, and the terminal queries, enough for a real
  `wasm32-wasip2` `println!` program (checked by hand with one, and by tests that declare the
  imports in a component). It also serves the arguments (`TypedHost::with_arguments`), the
  clocks (monotonic `now`, `subscribe-duration` and `subscribe-instant`, wall-clock `now`) and the
  real process stdin (a reader thread fills a buffer, `read` never blocks and ends with `closed`,
  the stdin pollable wakes on data and at the end of the input; as in `host.rs`; tests in
  `tests/it/wasip2_typed_cli.rs`, `tests/it/wasip2_typed_clocks.rs` and
  `tests/it/wasip2_typed_stdin.rs`). It also serves `wasi:random` and `wasi:filesystem`
  (`TypedHost::with_preopen`, read-only: `get-directories`, `open-at`, `read-via-stream`, `stat`,
  `stat-at`, `get-flags`, `read-directory`, `readlink-at`, `metadata-hash`, file
  `input-stream.read`; `TypedHost::with_writable_preopen` adds `open-at` with create/exclusive/
  truncate/write, `write-via-stream`, `append-via-stream`, file `output-stream` writes,
  `create-directory-at`, `remove-directory-at`, `unlink-file-at`, `rename-at`, `symlink-at`,
  `link-at`, `set-size`, `sync`, `sync-data`, `set-times` and `set-times-at`; all with the
  semantics of `host.rs`; tests in `tests/it/wasip2_typed_fs.rs` and
  `tests/it/wasip2_typed_fs_write.rs`). TCP over loopback too (`create-tcp-socket`, bind, listen, connect, accept, addresses,
  `subscribe`, `shutdown` and the connection streams; `TypedHost::with_external_network` lifts the
  loopback limit; tests in `tests/it/wasip2_typed_sockets.rs`). A component may import both
  filesystem and socket interfaces: the second of their two `error-code` enums gets a numeric suffix
  (`ErrorCode2`) in the generated trait, depending on import order, so the host names them through `FsErrorCode` / `NetErrorCode` aliases that
  `type_aliases` (`filter.rs`) emits (test in `tests/it/wasip2_typed_both.rs`).
- The `// optional: a | b` marker of the host sources (`filter.rs`) keeps a block if the generated
  trait declares any of the listed methods.
- `wit_host_glue` and `--wit-host --glue`: the glue from the typed `Host` trait to the flat
  reentrant `Imports` trait of the core module (`pub struct Glue<H: Host>(pub H)` with
  `impl<H: Host> Imports for Glue<H>`; each method has the name of the `Host` method it
  forwards to, lifts the core parameters, calls the host and lowers the result). New
  canonical ABI model (size, alignment, flattened types with `join`, `MAX_FLAT_PARAMS` 16 /
  `MAX_FLAT_RESULTS` 1). Covers every type of the typed `Host` trait: integers, floats,
  `bool`, `char`, `string` (UTF-8), `list`, `option`, `result`, `tuple`, `record`, `variant`,
  `enum`, `flags` and resource handles (with `[resource-drop]`), nested to any depth, lifted
  from the flat parameters or from guest memory and lowered to the flat result or to memory.
  Parameters of more than 16 flat values arrive as one pointer to a record, a result of more
  than one flat value is written through a trailing return pointer, and a `string`/`list`
  result is allocated with the guest's `cabi_realloc(0, 0, align, size)` (transpile with
  `named_exports`). Lifting is lenient like the canonical ABI (non-zero `bool` is true, narrow
  integers keep their low bits, undeclared flag bits are ignored); an invalid `char`,
  discriminant or UTF-8 string, an out-of-range pointer or a misaligned pointer panics (a
  trap), and so does a call that lifts more data (string/list bytes plus element counts) than
  the guest's memory length (`lifted data too large`), which stops aliased descriptors from
  amplifying host allocation. A real `wasm32-wasip2` hello world gets its glue and runs (`hello` prints).
- `wit_host_trait` and `--wit-host <component.wasm> [output]`: read the types of a WebAssembly
  component's imports (type section, instance types, aliases, imports; `wasmparser` only) and
  generate a typed `pub trait Host` with one method per function of every imported interface,
  named like the `named_imports` method of the core import that lowers it. Integers, floats,
  `bool`, `char`, `string`, `list`, `option`, `result`, `tuple` and `record` map to std types
  (a record is one struct, emitted once); resources become one `Copy` newtype each
  (`pub struct Pollable(pub u32);`, shared across interfaces; `own` and `borrow` are both the
  handle) with `[method]`/`[static]`/`[constructor]` functions as methods and a
  `<module>_resource_drop_<r>` method for the core import `[resource-drop]<r>`; a `variant` is a
  Rust `enum` with payload cases (`Case(T)`), an `enum` a fieldless `Copy` enum, and `flags` a
  `pub struct F(pub u8|u16|u32)` (the canonical ABI width; flag `i` is bit `i`) with one associated
  constant per flag plus `contains`/`union` (all emitted once, in the namespace shared with records
  and resources; `WitType::Variant`/`Enum`/`Flags`). `future`, `stream` and `error-context`
  are an `Unsupported` error naming the type. `wit_functions` returns the decoded `WitFunc`/`WitType` list. No lowering glue yet.
- `ref.test`, `ref.cast` and `br_on_cast` against a concrete function type: a non-null funcref
  matches when its function's declared type is the target or a declared subtype of it
  (`sub` chains, final vs. open types and equivalent types across rec groups included; the
  relation comes from validating the type section). Null follows the target's nullability.
  A module with a host is rejected (an external funcref carries no declared type).
  `call_indirect`/`call_ref` dispatch follows the same
  relation, so a function of a declared subtype is callable through its supertype.
- `ref.test`, `ref.cast` and `br_on_cast` against a concrete continuation type: the instance
  records each continuation's declared type (`cont.new`, `cont.bind`, and the type a `resume`
  handler block gives a suspended continuation, and the self-reference type a `switch` gives
  the parked switcher) and a test matches the target or a declared subtype of it. In such a
  module `cont.bind`, a `resume` handler and `switch` move the continuation to a new handle, so
  a reference kept from before keeps its own type.
- The abstract `contref` type and `ref.null cont`: a `u32` handle like a concrete continuation
  reference.
- `ref.null nocont` / `nullcontref`: the bottom of the continuation hierarchy lowers to the same
  `u32` handle as a concrete continuation reference.
- `ref.test`, `ref.cast` and `br_on_cast` accept the func/extern/exn hierarchies (and their bottom
  types) as targets: a top type matches every non-null value, a bottom type only null. A test
  against a concrete function type is covered separately below.
- A module may import several tables: each is host-owned and lent through its own accessor pair
  (`table`/`table_mut` for table 0, `table1`/`table1_mut`, ... for the others). Active element
  segments are copied in module order after construction. An imported `table i64` is supported.
  Only an imported *shared* table remains unsupported.
- The bottom reference types `nullfuncref`, `nullexternref` and `nullexnref` (and `ref.null nofunc`/`noextern`/`noexn`)
  are supported as value, global and table-element types; they lower like their top types.
- A module may define tables next to an imported table: the imported table 0 stays host-owned
  (`Imports::table`/`table_mut`), the defined ones are `table1`, `table2`, ... fields of the
  instance.
- `named_exports` / `--named-exports` also emits a getter `pub fn <name>(&self) -> T` for each
  exported numeric (`i32`/`i64`/`f32`/`f64`) global, e.g. `inst.stack_pointer()`, returning its
  current value (an imported global is read through the host). Reference-typed and `v128`
  globals get none. A mutable one also gets a setter `pub fn set_<name>(&mut self, v: T)`, e.g.
  `inst.set_stack_pointer(1024)` (an imported global is written through the host's `set_global{k}`).
  Names share one namespace with the function wrappers; a setter yields to an existing name.
- The WASI preview2 host example can let a program write files: `P2Host::with_writable_preopen`
  (`$WASIP2_PREOPEN_WRITABLE=1` in `wasip2_run`) supports `open-at` with `create`,
  `exclusive` and `truncate`, `write-via-stream` and `append-via-stream`; the preopen is
  read-only by default. It also supports `create-directory-at`, `unlink-file-at`,
  `remove-directory-at` and `rename-at` (a symlink is removed or moved itself, never
  followed). `set-size`, `sync`, `sync-data` and `set-times` work too (`std::fs`'s
  `set_len`, `sync_all`, `sync_data` and `set_times`). `set-times-at` follows a symlink only
  with `symlink-follow` (`unsupported` for a symlink itself, which `std` cannot set), and
  `readlink-at` returns the target of a symlink unless it is absolute or has a `..`
  (`not-permitted`, so no host path leaks). `symlink-at` makes only such links, and `link-at`
  makes hard links (a symlink is followed with `symlink-follow`, else `unsupported`; a
  directory is `not-permitted`).
  A file descriptor now holds the open file, so it stays on the file it opened when
  the path is unlinked, renamed over or reused; a directory descriptor's path is checked
  again on every use.
- The WASI preview2 host example supports TCP sockets over loopback: `create-tcp-socket`,
  `start-bind`/`finish-bind`, `start-listen`/`finish-listen`, `start-connect`/`finish-connect`,
  `accept`, `local-address`, `remote-address`, `subscribe` and `shutdown`, with the connection's
  streams and pollables working like the others. An address outside 127.0.0.0/8 and `::1` is
  `access-denied` unless the host calls `P2Host::with_external_network`. UDP, `ip-name-lookup`
  and the socket options (keep-alive, buffer sizes, hop limit, backlog) are not implemented.
- `wasm2rs_macros::include_wasm!` accepts `named_imports`, `import_memory` and
  `reentrant_imports` (the `TranspileOptions` of the same name), so a host such as the
  WASI preview2 example can embed a module without a build script.
- `TranspileOptions::reentrant_imports` / `--reentrant-imports`: every `Imports` method also
  receives the `Instance`, so a host can call guest functions and read or write guest
  memory while an import runs (needed for `cabi_realloc` in a WASI preview2 host). The host
  is lent out for the call; a nested import call traps. Off by default.
- `TranspileOptions::import_memory` / `--import-memory`: treat the module's own memory 0 as
  host-owned, like an imported memory, so the host's `Imports` methods can read and
  write guest memory (needed for the lowered imports of a WASI preview2 component).
  Off by default.
- Support for `try_table` with `catch $tag $label` and `catch_all $label` clauses
  (the exception-handling proposal's structured form), lowered through the legacy
  `try`/`catch` machinery. The spec harness replays `assert_exception`, and
  `throw.wast` and `try_table.wast` are vendored.
- Support for `exnref`: the `catch_ref` / `catch_all_ref` clauses of `try_table`,
  `throw_ref`, `ref.null exn`, and `exnref` locals, parameters, results, globals and
  tables. An `exnref` is a `u32` handle into a per-thread store of captured
  exceptions (entries are never freed, and a handle is only valid on the thread
  that captured it); `throw_ref` of a null reference traps. `throw_ref.wast` is
  vendored.
- Exception payloads may hold a `funcref`, `externref`, `exnref` or typed function
  reference. A GC-reference or `v128` payload is still rejected.
- A `br_table` inside a legacy `try` body may have arms that leave the body (it was
  rejected with "br_table out of a try region"). Such a table is lowered to a chain of
  conditional branches; tables that stay inside the body are unchanged.
- Support for `ref.null` items in element segments (funcref and externref) and for a
  table initializer expression (`(table 2 funcref (ref.func $f))` fills the initial
  entries with `$f`; `(ref.null func)` is the null default).
- `global.get` of an earlier immutable *defined* global in a constant expression: a
  global initializer, a table initializer or an element segment item (for a
  reference global). A *global* initializer may also read an immutable *imported*
  global: the value is the host's (`get_global{k}`), read when the instance is
  built. A table initializer (`(table 2 funcref (global.get $imported))`) and an
  item of an *active* element segment may read an immutable imported reference
  global the same way. An item of a passive segment (its items live in a static)
  is still rejected.
- A data or element segment offset may `global.get` an earlier immutable defined
  integer global (also inside extended-const arithmetic, and `i64` for memory64 and
  table64). An *immutable imported* global may be read too: the offset is then
  evaluated from the host when the instance is built, and a segment that does not
  fit traps there, as with a constant offset.
- Support for 64-bit tables (`table i64`): indices, sizes and lengths are `i64` in
  `call_indirect` and the `table.*` instructions, and an element segment offset may
  be an `i64`. An index above `u32::MAX` traps. An imported or shared table64 is
  still rejected.
- Support for a `v128.const` global initializer. Previously, transpiling a module
  that declared a `v128` global failed with an `Unsupported` error.
- `extract_core_modules` and `--extract-core`: pull the core modules out of a
  WebAssembly component as plain module binaries (nested components included), so
  `wasm-tools component unbundle` is no longer needed for that step.
- `named_imports` option (`--named-imports`): the `Imports` trait methods are named
  after the import (`env_log`) instead of its index (`import0`). Off by default.
- `examples/bench_memory.rs` and `benches/memory/*.wat`: a benchmark of the cost of
  bounds checks (default vs `unsafe_memory`) on memory-heavy workloads; results in
  `docs/bounds-check-bench.md`. The harness keeps the memory length opaque to the
  optimizer; otherwise LLVM removes every check and the comparison is vacuous. The doc also records a survey of how much static check removal could reach on real modules (and the decision not to pursue it for now).
- New workspace member `wasm2rs-macros` with the `include_wasm!` proc macro, which
  transpiles a `.wasm` file at compile time and expands to the generated Rust
  (options `named_exports` and `unsafe_memory`).
- `wasm2rs::build`: `transpile_to_out_dir` (and `_with_options`, `transpile_to_dir`)
  transpile a `.wasm` file into `OUT_DIR` from a `build.rs`, to be pulled in with
  `include!`. It emits `cargo:rerun-if-changed` for the input.
- Extended constant expressions: `i32`/`i64` `add`, `sub` and `mul` (wrapping, like the
  runtime operators) are now accepted in global initializers and in active data and
  element segment offsets.

### Changed

- `wasm2rs --dump` and `--extract-core` refuse the flags that only change the generated code
  (`--unsafe-memory`, `--named-exports`, `--named-imports`, `--import-memory`,
  `--reentrant-imports`, `--split-dispatch`, `--jobs`, `--entry-exports`) instead of
  silently ignoring them; `--extract-core` also refuses `--dump`.
- `docs/component-model-poc.md`: what a wasip2 component contains, that its core module
  already transpiles, and how far its preview2 imports are from the preview1 support.

### Fixed

- A function reachable only through `array.new_elem`/`array.init_elem` was treated as dead when
  `entry_exports` limited the roots.

- A module whose only GC reference values come from types (an `anyref`/`nullref` global, parameter, result,
  local or table element, with no body creating one) now gets the managed `GcRef` model instead of failing
  to compile.
- With an imported memory 0 next to a defined memory, the active data segments are applied after
  construction in module order, and after the active elements, so a trapping segment leaves the
  earlier writes (and the elements) in the host storage as the spec says.
- A module with an imported table and a shared memory never copied its active element
  segments into the host's table (calls through it trapped on null entries). `new` and
  `with_memory` now copy them, before the start function runs.
- Instantiation with a host-owned (imported) memory and table now applies active element
  segments before data segments, as the spec orders them. An element that does not fit
  the host table used to trap only after the data had already been written into the
  host's memory.
- `table.grow` now honors the table's declared maximum: growing past it returns -1 and
  leaves the table unchanged, as the spec requires. It used to grow up to the `u32::MAX`
  implementation limit regardless of the declaration.
- A module with a passive data or element segment but no memory, table or import
  failed to compile when it used `data.drop` or `elem.drop`: the segment is held by the `Instance`, which was not generated.
- `memory.init`, `table.init`, `data.drop` and `elem.drop` on an active or declared segment
  no longer fail to transpile: such a segment is dropped at instantiation, so `*.init` reads
  it as empty and `*.drop` does nothing. An `i32` segment offset is now unsigned, so a
  "negative" one is a huge address, so the segment traps at instantiation when it lies past
  the end of its memory or table, instead of being rejected. An empty active element
  segment placed past the end of its table now traps at instantiation too.
- A "negative" `i64` data offset in a 64-bit memory is likewise a huge address and traps at
  instantiation, instead of being rejected as `negative data offset`.
- A `try_table` inside unreachable code no longer fails the transpile with `operand stack
  underflow`: it is now counted as a nested region, so its `end` no longer closes the
  enclosing live frame. (A reachable `try_table` is still rejected as unsupported.)
- A trapping operand that a `br`, `br_if` or `return` discards (for example an out-of-bounds
  load left on the stack) is now evaluated instead of dropped, so the trap is no longer lost.
- A WebAssembly component (Component Model binary) is now rejected with a clear
  `Unsupported` error by `transpile` and `--dump`. Previously the parser descended
  into its nested core modules and silently emitted code for one of them.
- `return` is now supported inside a continuation body. It is lowered like a branch
  to the function's own label, so such a body goes through the flat state machine
  (and a cross-call checkpoint in it is rejected explicitly).
- A continuation body (`cont.new`/`resume`/`suspend`) can now `br`/`br_if`/`br_table`
  to the function's own label, which returns its results. Such a body is lowered
  through the flat state machine; a cross-call checkpoint in it is rejected with an
  explicit `Unsupported`.
- `ref.as_non_null`, `br_on_null` and `br_on_non_null` on a function or extern
  reference no longer generate code that assumes a managed GC handle.
- `floor` / `ceil` / `trunc` / `nearest` now return a quiet NaN for a NaN input on
  every host. On baseline x86_64 Linux the host's rounding returned a signaling NaN
  unchanged, which the spec forbids (found by the spec testsuite). The results are
  routed through small `quiet_f32` / `quiet_f64` helpers, emitted only when used;
  ordinary values are unaffected.
- A `br` / `br_if` / `br_table` that targets the function's own outermost label
  (equivalent to a `return`) is now supported instead of failing with
  `branch depth out of range`. Only functions that contain such a branch are wrapped
  in an extra region, so other output is unchanged. (Not covered: continuation
  bodies, which have their own lowering.)
- Generated functions that call themselves unconditionally no longer trigger
  rustc's `unconditional_recursion` warning (found by the spec testsuite).
- Constant operands such as `i32.const MIN; i32.const -1; i32.div_s` or a constant
  divisor of zero no longer make rustc reject the generated program
  (`unconditional_panic`); they still trap at run time (found by the spec testsuite).

### Added

- `TranspileOptions::named_exports` / `--named-exports`: emit a method or free function
  named after each exported function, forwarding to `func{N}`, so callers no longer
  need to know function indices. Off by default; output is unchanged unless set.
- Modules may define several tables. Table 0 keeps the `table` field and accessors
  (single-table output is unchanged); table `i > 0` gets `table{i}`. `table.get/set/
  size/grow/fill/copy/init`, `call_indirect` and active element segments address the
  named table. An imported table combined with other tables is still unsupported.
- Tests: a harness that replays vendored WebAssembly spec testsuite `.wast` files
  (`tests/spec/`, `tests/it/spec_wast.rs`). Modules whose only imports are
  `spectest.print*` run against a no-op host; `return_call` and `return_call_indirect`
  are replayed too.
- Tables of a typed function reference (`(table 3 (ref null $ft))`) are supported;
  they are stored like a `funcref` table. Element segments may use `ref.func`
  expression items.

## [0.2.0] - 2026-11-01

### Fixed

- A trapping load whose value is discarded by `drop` or `select` is still
  evaluated, so the trap is no longer lost.

### Changed

- Updated `wasmparser` / `wat` to 0.263.
- Smaller generated code and fewer allocations while transpiling (no offset-literal
  type suffixes, fewer temporary allocations, cheaper single-file memory).
- **A `shared` memory no longer sits behind a `Mutex`.** Its bytes are reserved once
  (up to the declared maximum) and accessed without a lock, so threads that touch
  disjoint memory now scale instead of serialising (8 threads of load/store work:
  6.4 s -> 19 ms; one thread: 261 ms -> 18 ms). Plain loads/stores are ordinary
  non-atomic accesses (racing on the same bytes gives unspecified values, as in
  wasm); atomic loads/stores/RMW/cmpxchg and `atomic.fence` are real sequentially
  consistent atomics and now trap on a misaligned address; `memory.grow` on a shared
  memory stops at its declared maximum. `SharedMemory::with_max(min, max)` is new;
  `SharedMemory::new(min)` still works and reserves the wasm32 limit.
- **`TranspileOptions` is no longer `Copy`** (it now owns `entry_exports`); it is
  still `Clone` and `Default`. Pass it by reference or `.clone()` it.

### Added

- **Parallel code generation: `--jobs=N` CLI flag and `TranspileOptions::jobs`.**
  Function bodies are generated on several threads and joined in function-index
  order, so the output is byte-identical for every value. `0` (default) picks the
  available parallelism (at most 8); `1` stays on the calling thread.
- **Emscripten pthread and emscripten JS-glue builds** run on native threads
  (see `docs/wasi-threads-e2e.md`).
- **`--entry-exports=a,b` CLI flag and `TranspileOptions::entry_exports`** emit only the
  functions reachable from the chosen exports (plus the start function and the
  entry points the generated code calls itself). Surviving functions keep their
  wasm-index names. Off by default, so existing output is unchanged. On a 40k-function
  C++ module (26k exports) keeping ~25 exports drops 37% of the functions and 24%
  of the source; reachable table reads keep every element-segment function, which
  bounds the saving.
- **`--dump` CLI flag and `wasm2rs::dump` API** print a summary of a module without
  generating code: function counts, each import and how it resolves (native WASI
  or host trait), exports, memory limits, the proposals in use, and whether the
  generated module needs a host `Imports` implementation. It reads the binary
  directly, so it also works for modules the transpiler rejects.
- **Twelve more WASI preview1 functions are generated natively** instead of being
  left to the host trait: `fd_tell`, `fd_sync`, `fd_datasync`, `fd_advise`,
  `fd_allocate`, `fd_filestat_set_size`, `fd_filestat_set_times`,
  `path_filestat_set_times`, `path_readlink`, `fd_fdstat_set_flags`,
  `fd_fdstat_set_rights` and `fd_renumber`. Requests `std` cannot honour (changing
  descriptor flags, renumbering stdio, setting times on a symlink itself) return
  `ENOTSUP` rather than being silently ignored.
- **`poll_oneoff`, `sock_*` and `proc_raise` are generated natively.**
  `poll_oneoff` supports clock subscriptions (relative and absolute, sleeping until
  the earliest) and `fd_read`/`fd_write` subscriptions (stdio and open files are
  always ready, an unopened fd is an `EBADF` event); no memory or fd-table lock is
  held while sleeping. `sock_accept/recv/send/shutdown` return `EBADF` or
  `ENOTSOCK` (no socket descriptors exist), and `proc_raise` returns `EINVAL` or
  `ENOSYS`. A host that implemented any of these through the `Imports` trait no
  longer receives those calls.

## [0.1.1] - 2026-08-17

### Added

- **Opt-in flattened-dispatch splitting** via the `--split-dispatch=<n>` CLI flag
  (and the `TranspileOptions::split_dispatch` library option). A flattened
  `loop { match pc { … } }` dispatch whose surviving arm count exceeds the cap is
  split into several sibling *part* functions over a shared state struct, so a
  pathologically large function is emitted as many smaller ones the Rust backend
  can optimise and codegen-parallelise independently — without changing what it
  computes. `0` (the default) keeps each flattened function whole.
- The generated WASI runtime's **preopen root is now configurable**, so the
  directory advertised to the module no longer has to be hard-coded.

### Changed

- `unreachable` and indirect-call type-mismatch **traps are now emitted as cold,
  out-of-line helpers** instead of being inlined at every trap site, shrinking the
  generated code on the hot path.

### Documentation

- Added crates.io / docs.rs / CI / coverage badges and prebuilt-binary install
  instructions to the README.

## [0.1.0] - 2026-08-08

Initial release. wasm2rs transpiles a WebAssembly binary into standalone, native
Rust source code — no interpreter and no runtime dependency on a WebAssembly
engine, while preserving WebAssembly semantics (wrapping arithmetic, linear-memory
bounds checks, and trap behaviour).

### Added

- **Core transpiler and CLI** producing standalone Rust from a `.wasm` module,
  with output to a single file, to stdout, or split across many chunk files for
  very large modules (with a ready-to-use, size-optimised `Cargo.toml`). The
  binary uses `mimalloc` to keep peak memory down on gigabyte-scale output.
- **Library API**: `transpile_with_options`, `transpile_split_with_options`,
  `TranspileOptions`, and `cargo_manifest`.
- **MVP WebAssembly coverage**: the full numeric instruction set (i32/i64/f32/f64
  arithmetic, division/remainder, shifts/rotates, comparisons, conversions,
  float min/max, trapping float-to-int truncation), linear-memory loads/stores,
  globals, direct and indirect calls, and the `unreachable` operator.
- **Control flow and multi-value**: typed block signatures, block/if parameters,
  parameterized (loop-carried) loops, value-carrying `br_table` targets, and
  multi-value function results.
- **Memory and segments**: active and passive data/element segments (with
  `init`/`drop`), bulk memory operations (`memory.fill`/`memory.copy`,
  `table.copy`), imported memory, and imported tables.
- **Reference types**: `ref.null`/`ref.func`/`ref.is_null`, table instructions
  (`get`/`set`/`size`/`grow`/`fill`), `externref` values and tables.
- **Imports via host traits**: imported functions, globals, memory, and tables
  injected through generated Rust traits, plus static and dynamic cross-instance
  module linking.
- **Native WASI (preview 1)** subset implemented directly in Rust — `proc_exit`,
  `fd_write`/`fd_read`, argv/environ, clocks, `random_get`, stdio `fd` operations,
  and a real-filesystem subset (`path_open`, `fd_readdir`, `path_link`, and other
  path-mutating calls).
- **Advanced proposals**:
  - Multi-threaded **atomics** over shared memory.
  - **Multi-memory** (multiple linear memories).
  - **Tail calls** (`return_call` / `return_call_indirect`).
  - **Garbage collection**: `call_ref`/`return_call_ref`, `i31`, typed funcrefs,
    heap-allocated struct and array objects, `ref.test`/`ref.cast`/`br_on_cast`
    subtyping, `ref.eq`/`ref.as_non_null`/`br_on_null`/`br_on_non_null`, and
    aggregate constructors with bulk array operations.
  - **Typed continuations**: `cont.new`, `resume`, `suspend`, `cont.bind`,
    `resume_throw`, and `switch`.
  - Legacy **exception handling**.
  - **SIMD/v128**: lane arithmetic, width-changing lanes, widening
    multiply/add lanes, and relaxed-SIMD fused-multiply-add and dot products.
- **Opt-in `--unsafe-memory` mode** that emits unchecked linear-memory access for
  faster code on modules trusted to stay in bounds (an out-of-bounds access
  becomes undefined behaviour instead of a wasm trap).

[Unreleased]: https://github.com/hirosassa/wasm2rs/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/hirosassa/wasm2rs/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/hirosassa/wasm2rs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hirosassa/wasm2rs/releases/tag/v0.1.0
