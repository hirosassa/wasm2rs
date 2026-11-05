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

## Result 3: a minimal preview2 host runs hello world natively

First finding: an `Imports` method receives only its integer arguments, and a module's
own memory is private to the `Instance`, so a host could not implement any lowered
import that takes or returns a pointer (`write`, `check-write`, `get-environment`, ...).
`TranspileOptions::import_memory` / `--import-memory` fixes that: the module's memory 0
becomes host-owned, like an imported memory, so the host's methods read and write guest
memory directly.

With it, `examples/wasip2_host/host.rs` implements the lowered imports of a
`println!` program (stdout/stderr writes with the canonical-ABI `result` layouts, a
fixed write budget, an empty environment and argument list, `exit`, no-op resource
drops) and `examples/wasip2_run.rs` builds and runs a core module with it:

```
rustc --target wasm32-wasip2 -C opt-level=3 hello.rs -o hello.component.wasm
wasm2rs --extract-core hello.component.wasm dir       # the program is the largest module
cargo run --release --example wasip2_run -- dir/module0.wasm
```

Two programs ran: `println!("hello")` (21 imports) and one using `eprintln!`,
`print!`, `flush` and `std::env::args` (22; the extra one is `get-arguments`). A small
WAT with the same imports is covered by `tests/it/wasip2_host.rs`.

## Result 4: the host calls back into the guest

`reentrant_imports` / `--reentrant-imports` makes every `Imports` method also receive
the `Instance`, so the host reaches the guest's memory (`g.memory()`) and its exports
(`g.cabi_realloc(..)`) while an import runs. That replaces `import_memory` for this
host: the guest keeps its own memory, and the host builds the lists and strings it
returns in it, the way the canonical ABI expects (`cabi_realloc(0, 0, align, size)`,
then `(ptr, len)` at the return pointer).

`examples/wasip2_host/host.rs` now implements, besides the hello-world imports:

- `get-arguments` (`list<string>`, 8-byte elements) and `get-environment`
  (`list<tuple<string, string>>`, 16-byte elements), non-empty;
- `poll`, a `list<u32>` of the ready pollables' indices (at this stage every pollable
  was always ready; Result 5 adds deadlines).

The runner passes its trailing arguments and its environment to the program
(`wasip2_run -- module0.wasm one "two words"`; the work dir is
`$WASIP2_WORK_DIR`, default a temporary directory). The `std::env::args` program above prints 3 arguments (its own
name and the two given). `tests/it/wasip2_host.rs` checks the exact layouts from a
WAT module with a bump-allocator `cabi_realloc`; no real program calls `poll` from
`println!`, so `poll` is covered by that test only.

## Result 5: stdin and clocks

Two more programs ran natively: one reading stdin (`read_line`, then `read_to_string`;
23 imports) and one with `thread::sleep`, `Instant::now` and `SystemTime::now` (24).
Beyond the hello-world set they add `input-stream.read`/`subscribe`,
`monotonic-clock.now`/`subscribe-instant` and `wall-clock.now`, all optional blocks in
the host.

- Pollables are indices into a table: `None` is always ready (streams), `Some(deadline)`
  is ready once the deadline passes. `poll` returns the ready indices and otherwise
  sleeps until the earliest deadline; `pollable.block` sleeps until its deadline.
- `input-stream.read` blocks on the real stdin until at least one byte or end of input
  (`stream-error::closed`: result tag 1, error tag 1), so an input stream counts as
  always ready. A non-blocking `read` that returns an empty list would need a reader
  thread; not done.
- The monotonic clock counts from the host's creation; `wall-clock.now` writes
  `datetime { seconds: u64, nanoseconds: u32 }`.
- `tests/it/wasip2_host.rs` covers all of it from WAT (piped stdin, deadline waits, a
  ready pollable returning at once next to a ten-second deadline).

What it does not cover: sockets, and file metadata and directory listings (random: see Result 6; reading files: Result 7); the `Imports` trait
is generated per module, so the host has to match the module's import set (the example
marks the methods only some programs need).

## Result 6: random

A program using a `HashMap` needed one more import, `wasi:random/insecure-seed`
(`tuple<u64, u64>` at the return pointer and 8 bytes after it), and ran natively once
the host had it. The host also implements the rest of `wasi:random`
(`get-random-bytes`/`get-random-u64`, `get-insecure-random-bytes`/`-u64`), which
`rand`/`getrandom`-style programs call; those are covered by the WAT test only. All
five are optional blocks.

The numbers come from a small non-cryptographic generator (splitmix64) seeded from the
OS-seeded `std` hasher. That is right for `HashMap` seeds and examples, and wrong for
keys or tokens: do not treat `get-random-bytes` here as cryptographically secure.

## Result 7: reading files

`std::fs::read_to_string` (and `File::open`) now works. A program that touches
`std::fs` imports the whole `wasi:filesystem` surface (13 imports) even if it only
reads, so the host implements all of it: `get-directories`, `open-at`,
`read-via-stream` and the descriptor/stream drops do the work, and the rest answers an
`error-code`. Run it with `WASIP2_PREOPEN=<dir> cargo run --release --example
wasip2_run -- module0.wasm`: `<dir>` is the guest's `/` (without it the guest has no
preopens), and both `hello.txt` and `/hello.txt` open the same file.

- The preopen is read-only: creating, truncating and writing (`open-at` flags,
  `write-via-stream`, `append-via-stream`) return `read-only`.
- Paths must stay under the preopen: an absolute path or any `..` is `not-permitted`
  (stricter than WASI, which allows `..` that stays inside), and so is a symlink that
  leads out (the resolved path is checked against the preopen). Path flags are
  ignored; links are always followed.
- A file's input stream shares `input-stream.read` with stdin (handle 0). Streams are
  freed on `resource-drop`; descriptors are not.
- Not done yet: `stat`, `stat-at`, `get-flags`, `metadata-hash` and `read-directory`
  return `unsupported`. `std::fs::metadata` and `read_dir` therefore fail.

## Options for the next investment

1. **Extract core modules** (small): a `wasmparser`-based extraction (the nested
   `ModuleSection` ranges) so users do not need `wasm-tools` just for this. Does not
   by itself run a program. *Done: `extract_core_modules` / `--extract-core`.*
2. **Name-preserving imports** (small/medium): carry the import module/field name
   into the `Imports` trait (like `named_exports` did for exports). Prerequisite for
   everything below. *Done: `named_imports` / `--named-imports`.*
3. **A preview2 host shim over `Imports`** (large; hello world, arguments, environment, `poll`, stdin, clocks, random and reading files are done, see Results 3 to 7): implement the canonical ABI for the
   `wasi:cli`/`io`/`clocks`/`random` subset (args, env, exit, stdio, clocks, random) as an opt-in
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
