# WASI 0.3 spike: what an async guest looks like as core wasm (M0.5)

This is the M0.5 spike of the WASI 0.3 (p3) plan. It records what a real async guest compiles
to, so that the design of M1 (reading `async func` and generating glue) and M2 (a scheduler) does
not rest on guesses. Nothing here is implemented; the current state is in
[wasi-p2-status.md](wasi-p2-status.md).

## Method

- Guest: a `cdylib` using `wit-bindgen` 0.63.0 (`wit_bindgen::generate!`, async by default),
  built with `cargo build --release --target wasm32-wasip2` (rustc 1.99.1). The component comes
  straight out of the build; `wasm-tools validate --features all` accepts it. A `wasm32-wasip1`
  build needs a preview1 adapter to be componentized, so the `wasip2` target was used.
- WIT: one imported interface (`async func slow`, `func numbers() -> stream<u32>`,
  `func one() -> future<u32>`) and four exports: `async func double-slow(n: u32) -> u32` (awaits
  an async import twice), `async func sum-stream(s: stream<u32>) -> u32`,
  `async func await-future(f: future<u32>) -> u32`, and a synchronous `sync-add`.
- Observation: `wasm-tools print`, then reading the canonical section and the main core module
  (324 functions, 90 `call_indirect`, a 108-slot funcref table).
- Not done: running the component (no Wasmtime on this machine) and a guest built for
  `wasm32-wasip3` (see "Tooling facts"). The host side of the imports was not provided; only the
  guest's shape was examined.

## Observations

### The guest uses the callback ABI

Every async export is lifted as `canon lift ... async (callback ...)`:

```wat
(func $double-slow (canon lift (core func $"[async-lift]double-slow") async
                     (callback $"[callback][async-lift]double-slow")))
(func $sync-add (canon lift (core func $sync-add)))   ;; sync export: unchanged
```

- The entry `[async-lift]NAME` has the flat signature of the WIT parameters and returns one
  `i32`: the first callback code (what the task waits for next). For `double-slow` it is
  `(i32) -> i32`.
- The callback is `(i32, i32, i32) -> i32`: an event code and two payload words in, the next
  callback code out. wit-bindgen's three callbacks are one and the same function
  (`[callback][async-lift]await-future`, exported under three names); its body is a call to the
  runtime's `async_support::callback`, which polls the Rust futures of the task.
- The stackful ABI (`async` without `callback`) does not appear in this build. Whether a
  `wit-bindgen` option selects it was not checked, so it is only known that Rust guests built
  with the defaults use the callback ABI.
- Between events the guest returns to the host. The suspended state (the Rust `Future`) lives
  in linear memory, not on the wasm stack. **Nothing in the callback ABI needs stack switching.**
- The return-code encoding (EXIT, YIELD, WAIT with a waitable-set index) is from the
  specification and was not decoded in this spike.

### Canonical built-ins that appear

| Built-in | Seen in this guest | Core signature seen |
|---|---|---|
| `task.return` (one per async export, with the result type) | yes | `(i32) -> ()` for `u32` |
| `task.cancel` | yes | |
| `context.get 0` / `context.set 0` | yes (the runtime's per-task state) | `() -> i32`, `(i32) -> ()` |
| `waitable-set.new` / `.poll` / `.drop`, `waitable.join` | yes (`.poll` takes the memory) | |
| `subtask.drop` / `subtask.cancel` | yes | |
| `canon lower ... async` of an async import | yes | `(param, result pointer) -> i32` status |
| `stream.new` / `stream.read` / `stream.write` / `.cancel-*` / `.drop-*` | yes, per stream type | `stream.new: () -> i64`; `read`/`write`: `(handle, ptr, len) -> i32` |
| `future.new` / `.read` / `.write` / `.cancel-*` / `.drop-*` | yes, per future type | |
| `waitable-set.wait`, `thread.yield`, `backpressure.set`, `thread.*` | no | |

`stream.*` and `future.*` are instantiated per type index (`stream.new 25` for `stream<u32>`);
the guest imports them under names like `[stream-read-0]numbers` and
`[async-lower][stream-read-0]numbers`.

### Wiring is the same shape as for p2

The async built-ins that need the memory (`waitable-set.poll`, `canon lower async`, `stream.read`,
`future.read`, ...) are reached through `wit-component`'s shim module and its table
(`$imports`, 14 slots here) filled by a fixup module. This is the indirection wasm2rs already
links for preview1 adapters and lowered host functions, so the wiring needs no new mechanism.
What is new is that the canonical section carries many more built-ins and that `canon lower`
and `canon lift` carry the `async`/`callback` options.

### `call_indirect` is inside the async runtime

`async_support::callback`, `CabiTask::new`, and the waitable operations each contain a
`call_indirect` (the executor stores futures as trait objects). So a design that suspends
*inside* core wasm, across `call_indirect`, would be needed only for the stackful ABI; the
callback ABI never suspends there.

### Core-wasm facts relevant to wasm2rs

- Size: the printed guest is about 29.6k lines for a few lines of Rust.
- `context.get 0/set 0` is the wit-bindgen executor's task-local pointer. The `wasm32-wasip3`
  target will also keep the shadow stack pointer in a context slot instead of a `global`
  (rustc platform-support page), so a scheduler needs one context slot set per task and
  must swap it when it resumes another task.

## Tooling facts (the "unconfirmed" items of the plan)

| Item | Result |
|---|---|
| `wit-bindgen` (Rust) async | 0.63.0 generates `async fn` guests (used here) |
| `wasm-tools` | CLI 1.255.0 validates and prints these components (`--features all`); the 1.263.0 crates wasm2rs uses parse the types (M0 tests build them) |
| `wasip3` crate | 0.10.0+wasi-0.3.2 on crates.io |
| `wasm32-wasip3` | Tier 2, first on stable in Rust 1.100.0 (not installable on 1.99.1) |
| Wasmtime | 46.0.0 and later releases exist (v46, v47, v48 to v51 rc series); its default support for the final 0.3.0 was not run here |

## Recommendations

1. **Target the callback ABI first (M2); drop "cont-based stackful async" from the critical
   path.** The guests of the main Rust toolchain use it, it needs no suspension inside core wasm,
   and so the `cont` limit on `call_indirect` and recursion does not apply. Stackful would be a
   later, separate decision.
2. **M1 is small and independent of the scheduler.** Read `async` function types and the
   `async`/`callback` lift and lower options into the model, and emit glue that is not yet run.
   The built-ins already pass through as flat `Imports` methods (see
   [wasi-p2-status.md](wasi-p2-status.md)), but they are not typed.
3. **The scheduler is host-side state plus a few built-ins.** It must own: tasks (one per async
   export call), subtasks (one per async import call), waitable sets, stream and future
   endpoints with a copy between memories, and the per-task context slot. The generated code
   only has to expose "call entry, then call callback with an event until EXIT".
4. **The host surface is a separate crate surface.** `wasm2rs-wasi` is built on `wasi:io`
   pollables and streams; a 0.3 host replaces these with `stream<u8>` and `future` values, and
   must be driven by the same scheduler.
5. **Test with Rust guests built by `wit-bindgen`**, with Wasmtime as the oracle (differential
   testing, as for p2). Guests from other toolchains were not examined and are reported to be
   less mature.

## Open questions

- The exact callback-code and event encodings and the waitable-set protocol need to be read from
  the specification (or decoded from this guest) when M2 starts.
- A guest built for `wasm32-wasip3` with `std` has not been examined; it will also import 0.3 WASI
  interfaces (`wasi:cli`, `wasi:clocks`, ...) as `async`/`stream`/`future` and may differ in shape.
- `thread.*` (cooperative threads) is not part of 0.3.0 and is not covered.
