# Component Model PoC: what a wasip2 component looks like to wasm2rs

wasm2rs transpiles core modules only; a component is rejected with a clear error
(#60). This note records what a real `wasm32-wasip2` component contains, whether its
core module can already go through wasm2rs, and how far its WASI preview2 imports are
from what wasm2rs implements. No code changes; it is input to the decision on how
far to invest in the Component Model.

## Reproduce

```
rustup target add wasm32-wasip2
rustc --target wasm32-wasip2 -C opt-level=3 hello.rs -o hello.component.wasm
wasm-tools component wit hello.component.wasm            # imports/exports (WIT)
wasm-tools print hello.component.wasm                    # structure
wasm-tools component unbundle hello.component.wasm \
    --module-dir ub --output ub/component.wasm --threshold 0
wasm2rs --dump ub/unbundled-module0.wasm                 # core imports
wasm2rs ub/unbundled-module0.wasm out.rs                 # transpile
```

`hello.rs` prints its arguments and environment count with `println!`-style stdout
writes, reads the clock and writes a file (so std pulls in cli, io, clocks and
filesystem). Toolchain: rustc 1.99.1, wasm-tools 1.255.0.

## What a component contains

- 3 core modules, 22 core instances, 26 `canon lower`, 1 `canon lift`:
  - `$main` (2.8 MB): the program itself, a normal core module.
  - a small *shim* module and a *fixup* module (about 450 and 240 bytes): only wiring.
    The shim exposes the lowered imports through a funcref table (`$imports`) and the
    fixup fills that table, so `$main`'s imports resolve to the `canon lower`ed host
    functions.
- Component-level imports (WIT): `wasi:io/{poll,error,streams}`, `wasi:clocks/{monotonic-clock,wall-clock}`,
  `wasi:cli/{stdout,stderr,stdin,environment,exit,terminal-*}`, `wasi:filesystem/{types,preopens}`;
  export `wasi:cli/run`.
- `$main` exports `memory`, `cabi_realloc` (the canonical-ABI allocator the host
  calls to pass `list`/`string` into the guest), `wasi:cli/run@0.2.0#run` and
  `__main_void`.

## Result 1: the core module already transpiles

`wasm-tools component unbundle` extracts the three modules. `unbundled-module0.wasm`
(365 functions, 33 imported) goes through `wasm2rs` unchanged: 716 KB of Rust that
compiles (`rustc --crate-type lib`, opt-level 0). Its 33 imports surface as `Imports`
trait methods `import0..import32`, flagged `[host trait]`, as for any non-preview1
import. The imports are *lowered* functions: pointers into linear memory, return
values through a return pointer, resources as `i32` handles.

So the code-generation half needs nothing new; what is missing is a host that
implements those 33 imports and the glue that wires them (what the shim and fixup
modules do inside a component).

## Result 2: the imports against what wasm2rs implements

wasm2rs implements ~45 `wasi_snapshot_preview1` functions natively. The 33 preview2
imports of this program:

| preview2 imports (hello) | closest preview1 behaviour | gap |
|---|---|---|
| `cli/environment.get-arguments`, `get-environment` | `args_sizes_get`/`args_get`, `environ_*` | result is a `list<string>` written through a return pointer using `cabi_realloc`, not the preview1 two-call layout |
| `cli/exit.exit` | `proc_exit` | result-typed argument |
| `cli/stdin|stdout|stderr.get-*`, `io/streams` `output-stream.check-write/write/blocking-flush/subscribe`, `[resource-drop]` | `fd_write`/`fd_read` on fds 0–2 | preview2 hands out *resources* (handles); needs a handle table, `check-write` back-pressure, `stream-error` variant |
| `cli/terminal-*` | `fd_fdstat_get` (isatty) | option-of-resource return |
| `io/poll.poll`, `[method]pollable.block`, `subscribe` | `poll_oneoff` | different model (pollable resources, list of handles) |
| `clocks/monotonic-clock.now/subscribe-duration`, `wall-clock.now` | `clock_time_get` | `datetime` record via return pointer; subscribe needs pollables |
| `filesystem/types` `descriptor.read-via-stream/write-via-stream/append-via-stream/get-flags/stat/open-at/metadata-hash`, `preopens.get-directories` | `path_open`, `fd_read/write`, `fd_filestat_get`, `fd_prestat_*` | descriptors are resources; `open-at` flag/`result<descriptor, error-code>` layouts; `list<tuple<descriptor,string>>` |

Only the first four rows' *behaviour* exists in preview1 form; every row needs the
canonical-ABI layer (result/variant/list/record layouts, `cabi_realloc` calls, a
resource handle table) that wasm2rs does not have. `Imports` methods are also
numbered (`import{j}`), not named, so a host cannot tell which import is which
without the dump; preserving import names would be a prerequisite for any generated
or hand-written bridge.

Not exercised by this program, and harder: `wasi:sockets`, `wasi:http`, the rest of
`filesystem` (readdir, rename, ...), async/streams in WASI 0.3.

## Options for the next investment

1. **Extract core modules** (small): a `wasmparser`-based extraction (the nested
   `ModuleSection` ranges) so users do not need `wasm-tools` just for this. Does not
   by itself run a program. *Done: `extract_core_modules` / `--extract-core`.*
2. **Name-preserving imports** (small/medium): carry the import module/field name
   into the `Imports` trait (like `named_exports` did for exports). Prerequisite for
   everything below. *Done: `named_imports` / `--named-imports`.*
3. **A preview2 host shim over `Imports`** (large): implement the canonical ABI for the
   `wasi:cli`/`io`/`clocks` subset (args, env, exit, stdio, clocks) as an opt-in
   generated or companion module, with a handle table. Filesystem is a further large
   step. This is what makes a wasip2 hello-world run natively.
4. **Full component support** (very large): parse the component, resolve instances
   and `canon lift/lower` per the spec, and generate glue for arbitrary WIT worlds.
   Months of work; only worth it if general components are a goal.

## Recommendation

Do 1 and 2 (cheap, independently useful, and de-risk 3). Then decide on 3 by whether
running *wasip2 Rust programs* natively is a goal; if it is, scope it to the
cli/io/clocks subset first. Option 4 is not justified by anything seen here: for the
common case of one core module plus a WASI adapter, the unbundle recipe plus a host
shim covers the use.
