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

With it, a hand-written host (`examples/wasip2_host/host.rs`, since retired in favor of the typed host) implemented the lowered imports of a
`println!` program (stdout/stderr writes with the canonical-ABI `result` layouts, a
fixed write budget, an empty environment and argument list, `exit`, no-op resource
drops) and `examples/wasip2_run.rs` built and ran a core module with it (that is how
the programs below ran). The example now takes the component itself and runs it through
the typed host (`typed.rs`, with the generated glue); this is a
breaking change of its input (it no longer accepts an extracted core module):

```
rustc --target wasm32-wasip2 -C opt-level=3 hello.rs -o hello.component.wasm
cargo run --release --example wasip2_run -- hello.component.wasm
```

The example extracts the component's core modules itself and runs the largest (the
program); a component that imports a function `typed.rs` does not implement fails to
build with rustc's E0046 errors and a hint, and the example exits with 2.

Two programs ran: `println!("hello")` (21 imports) and one using `eprintln!`,
`print!`, `flush` and `std::env::args` (22; the extra one is `get-arguments`). A small
WAT with the same imports was covered by the retired `tests/it/wasip2_host.rs`; the typed host's tests are in `tests/it/wasip2_typed_*.rs`.

## Result 4: the host calls back into the guest

`reentrant_imports` / `--reentrant-imports` makes every `Imports` method also receive
the `Instance`, so the host reaches the guest's memory (`g.memory()`) and its exports
(`g.cabi_realloc(..)`) while an import runs. That replaces `import_memory` for this
host: the guest keeps its own memory, and the host builds the lists and strings it
returns in it, the way the canonical ABI expects (`cabi_realloc(0, 0, align, size)`,
then `(ptr, len)` at the return pointer).

The hand-written host (`host.rs`, since retired) implemented, besides the hello-world imports:

- `get-arguments` (`list<string>`, 8-byte elements) and `get-environment`
  (`list<tuple<string, string>>`, 16-byte elements), non-empty;
- `poll`, a `list<u32>` of the ready pollables' indices (at this stage every pollable
  was always ready; Result 5 adds deadlines).

The runner passes its trailing arguments and its environment to the program
(`wasip2_run -- program.component.wasm one "two words"`; the work dir is
`$WASIP2_WORK_DIR`, default a temporary directory). The `std::env::args` program above prints 3 arguments (its own
name and the two given). The retired `tests/it/wasip2_host.rs` checked the exact layouts from a
WAT module with a bump-allocator `cabi_realloc`; no real program calls `poll` from
`println!`, so `poll` was covered by that test only.

## Result 5: stdin and clocks

Two more programs ran natively: one reading stdin (`read_line`, then `read_to_string`;
23 imports) and one with `thread::sleep`, `Instant::now` and `SystemTime::now` (24).
Beyond the hello-world set they add `input-stream.read`/`subscribe`,
`monotonic-clock.now`/`subscribe-instant` and `wall-clock.now`, all optional blocks in
the host.

- Pollables are indices into a table: always ready (streams, files), ready once a
  deadline passes (clocks), or ready when stdin has data or is at its end. `poll`
  returns the ready indices and otherwise waits for the earliest deadline or for stdin
  data, whichever comes first; `pollable.block` waits for its one pollable.
- Stdin is read by a thread (started on first use) into a buffer, so `input-stream.read`
  never blocks: it returns what is buffered, an empty list if nothing has arrived yet,
  and `stream-error::closed` (result tag 1, error tag 1) at end of input. `std` waits on
  the pollable before it reads, so a program can wait for stdin and a timer together.
  (Before this, `read` blocked and stdin counted as always ready, which stalled a
  program that also waited on a deadline.)
- The monotonic clock counts from the host's creation; `wall-clock.now` writes
  `datetime { seconds: u64, nanoseconds: u32 }`.
- The retired `tests/it/wasip2_host.rs` covered all of it from WAT (piped stdin, deadline
  waits, a ready pollable returning at once next to a ten-second deadline, an idle
  stdin read, and `poll` waking for stdin data and end of input).

What it does not cover: UDP and name lookup in the flat host (random, files and loopback TCP: Results 6 and 7; the typed host serves them); the `Imports` trait
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

## Result 7: files

`std::fs::read_to_string`, `File::open`, `metadata` and `read_dir` now work. A program
that touches `std::fs` imports the whole `wasi:filesystem` surface (13 imports) even if
it only reads, so the host implements all of it: opening and reading files, metadata
and directory listings work, and writing answers an `error-code`. Run it with `WASIP2_PREOPEN=<dir> cargo run --release --example
wasip2_run -- program.component.wasm`: `<dir>` is the guest's `/` (without it the guest has no
preopens), and both `hello.txt` and `/hello.txt` open the same file.

- The preopen is read-only by default: creating, truncating and writing (`open-at`
  flags, `write-via-stream`, `append-via-stream`) return `read-only`.
- **Writing is opt-in**: `TypedHost::with_writable_preopen(dir)` (`$WASIP2_PREOPEN_WRITABLE=1`
  next to `$WASIP2_PREOPEN` in `wasip2_run`). Then `open-at` honours `create`,
  `exclusive` (`exist` if the file is there) and `truncate`, and a descriptor opened
  with `write` gives `write-via-stream(offset)` and `append-via-stream`, whose output
  streams share `check-write`/`write`/`blocking-flush` with stdout (file streams are
  handles `3..`; a failed write answers `closed`). A new file is named under an
  existing directory that must itself resolve under the preopen, and a symlink that
  leads nowhere or out (even a dangling one) is `not-permitted`, not a place to create
  a file. The path is checked when the file is opened, and a file descriptor then
  holds the open `File`: every operation on it goes through that `File`, so it stays on
  the file it opened even if the path is unlinked, renamed over or reused. A program
  that swaps a directory for a symlink between the check and the open can still escape,
  so only preopen a directory you trust the program with.
- With a writable preopen, `create-directory-at`, `unlink-file-at`,
  `remove-directory-at` and `rename-at` work too (`std::fs::create_dir`,
  `remove_file`, `remove_dir`, `rename`; `create_dir_all` and `remove_dir_all` are built
  from them). The name is resolved like a created
  file's (parent under the preopen; `..`, absolute paths and names ending in `/` or
  `/.` refused), and the entry itself is never followed: `unlink` removes a symlink,
  not its target, `remove-directory-at` on a symlink is `not-directory`, and a symlink
  cannot be renamed (`not-permitted`: a symlink moved onto the path of an open descriptor
  would lead it out of the preopen). A directory descriptor has no `File` (std cannot
  open relative to a directory), so its path is resolved again on every use and must
  still be under the preopen (`not-permitted` if not), which covers a directory holding
  a symlink that leads out being renamed over it; the check and the use are separate
  steps, so a host that must resist a racing guest needs `openat` and friends (a crate
  such as `rustix`). Errors:
  `exist`, `no-entry`, `is-directory`, `not-directory`, `not-empty`. On a read-only
  preopen all four are `read-only`.
- `set-size` (needs a descriptor opened for `write`; a directory is `is-directory`),
  `sync`, `sync-data` and `set-times` (`no-change`, `now` or a `timestamp`, on a directory or a file
  opened for `write`; nanoseconds of a second or more are `invalid`) are done. On a read-only
  preopen `set-size` and `set-times` are `read-only`, while `sync` succeeds since it
  changes nothing. `set-times-at` and `readlink-at` are done too (a symlink's own times cannot be set: `unsupported`; a link target that is absolute or has `..` is `not-permitted`); `symlink-at` (a target that is absolute or has `..` is `not-permitted`) and `link-at` are done too.
- Sockets: TCP over loopback is done (`tcp-create-socket`, `start-bind`/`finish-bind`, `start-listen`/`finish-listen`, `start-connect`/`finish-connect`, `accept`, `local-address`, `remote-address`, `subscribe`, `shutdown`, `instance-network`); the streams are the ordinary `input-stream`/`output-stream` handles. An address outside 127.0.0.0/8 and `::1` is `access-denied` unless the host calls `TypedHost::with_external_network`. `std` binds and listens in one call, so `finish-bind` does both, a bound socket cannot connect (`not-supported`), and `start-connect` connects at once. UDP and `ip-name-lookup` are done in the typed host only (below); the TCP socket options (keep-alive, buffer sizes, hop limit, backlog) are not implemented.
- Paths must stay under the preopen: an absolute path or any `..` is `not-permitted`
  (stricter than WASI, which allows `..` that stays inside), and so is a symlink that
  leads out (the resolved path is checked against the preopen). Without
  `symlink-follow`, `open-at` on a symlink as the last component is `loop` (also when
  creating; `exist` with create+exclusive), and `stat-at`/`metadata-hash-at` describe the link
  itself; links earlier in the path are still followed.
- A file's input stream shares `input-stream.read` with stdin (handle 0).
- Handles are reused: `resource-drop` of a pollable, descriptor, file input stream or
  directory-entry-stream frees its slot, and the next resource of that kind takes the
  most recently freed one, so a program that opens and drops in a loop does not grow
  the host's tables. The preopen (descriptor 0) is never freed.
- **Contract:** a guest must not use a handle after dropping it (the Component Model
  forbids it). The host does not promise to detect that. Until the slot is reused, a
  stale handle may fail (`bad-descriptor` or a panic) or quietly succeed, depending on
  the call; once it is reused, the stale handle silently names the newer resource.
- Metadata and directories: `stat`, `stat-at` (type, link count, size, access and
  modification times; the status-change time is `none`), `get-flags` (always `read`),
  `metadata-hash` (a hash of the canonical path) and `read-directory` /
  `read-directory-entry` (a snapshot taken when the directory is opened, without `.`
  and `..`, in the OS's order). `std::fs::metadata`, `File::metadata` and `read_dir`
  work; the same path checks apply to `stat-at` and `metadata-hash-at`.

## Options for the next investment

1. **Extract core modules** (small): a `wasmparser`-based extraction (the nested
   `ModuleSection` ranges) so users do not need `wasm-tools` just for this. Does not
   by itself run a program. *Done: `extract_core_modules` / `--extract-core`.*
2. **Name-preserving imports** (small/medium): carry the import module/field name
   into the `Imports` trait (like `named_exports` did for exports). Prerequisite for
   everything below. *Done: `named_imports` / `--named-imports`.*
   *Typed host trait generation started: `wit_host_trait` / `--wit-host` read the component's
   import types and generate a `Host` trait with the same method names (plain-data types and resources, as `Copy` handle newtypes with a
   per-resource drop method, variants as Rust enums, enums, and flags as `Copy` integer newtypes with a constant per flag; futures, streams and error-context are rejected by name; a real `wasm32-wasip2` hello world now yields a trait). The glue between it and the core `Imports`
   is done for every type the trait supports: `wit_host_glue` / `--wit-host --glue` generate `Glue<H: Host>` implementing `Imports`, with a canonical ABI model (layout, flattening, `join`), lift/lower from flat values or memory (strings as UTF-8, lists, options, results, records, variants, flags, handles, nested), parameters spilled to one pointer past 16 flat values, results through a return pointer, allocation through the guest's `cabi_realloc` and bounds, alignment, UTF-8 and discriminant checks that panic like traps, plus a per-call lift budget (traps: invalid UTF-8, bad pointers, lifted data larger than the guest's memory) so that many `(ptr, len)` descriptors aliasing the same bytes cannot make the host copy quadratically more than the guest holds. `examples/wasip2_host` used to hand-write the same ABI against the flat `Imports`; that host was retired in favor of `typed.rs`, so a host only implements the typed `Host`. A manual check: a `wasm32-wasip2` hello world's `--wit-host --glue` text plus a std-based `impl Host` (stdout `write`, `check-write`, `get-stdout`, `poll`, ...) appended to the transpiled main core module (`--extract-core`, `module0`, transpiled with `named_imports`, `named_exports` and `reentrant_imports`; its imports are already the lowered functions, so the shim and fixup modules are not needed) prints `hello`. Checked against a real `wasm32-wasip2` hello world: all 21 core import names
   (`wasi:io/poll@0.2.0` `[resource-drop]pollable`, ...) equal the `Host`/`Imports` method names, because the core module's
   version (`@0.2.0`) differs from the component import's (`@0.2.12`) and `named_imports` drops the version suffix.*
3. **A preview2 host shim over `Imports`** (large; hello world, arguments, environment, `poll`, stdin, clocks, random and reading files are done, see Results 3 to 7): implement the canonical ABI for the
   `wasi:cli`/`io`/`clocks`/`random` subset (args, env, exit, stdio, clocks, random) as an opt-in
   generated or companion module, with a handle table. Writing, removing and renaming files are done (Result 7); `set-times-at` and `readlink-at` are done too; `symlink-at` and `link-at` are done too; so are TCP sockets over loopback (UDP and `ip-name-lookup` only in the typed host, below). This is what makes a wasip2 hello-world run natively. `examples/wasip2_host/typed.rs` redoes the `println!` subset (stdio, environment, exit, streams, pollables, `poll`, terminal queries) against the generated typed `Host` and glue, without canonical-ABI code; it also covers the arguments (`with_arguments`), the clocks (monotonic `now`, `subscribe-duration`, `subscribe-instant`, wall-clock `now`) and the real process stdin (reader thread, non-blocking `read`, pollable that wakes on data and at the end of input; `tests/it/wasip2_typed_cli.rs`, `tests/it/wasip2_typed_clocks.rs`, `tests/it/wasip2_typed_stdin.rs`), the other stream methods (`blocking-read`, `skip`, `blocking-skip`, `flush`, `blocking-write-and-flush`, `write-zeroes`, `blocking-write-zeroes-and-flush`, `splice`, `blocking-splice`; a write of over 4096 bytes traps, a blocking write to a connection waits for the peer to read; `tests/it/wasip2_typed_streams.rs`, `tests/it/wasip2_typed_splice.rs`), and `wasi:random` and files (`with_preopen`, read-only: `get-directories`, `open-at`, `read-via-stream`, `stat`, `stat-at`, `get-flags`, `read-directory`, `readlink-at`, `metadata-hash`, file `input-stream.read`; `with_writable_preopen` adds writing, appending, creating, truncating, removing, renaming, linking and `set-times`; `tests/it/wasip2_typed_fs.rs`, `tests/it/wasip2_typed_fs_write.rs`); TCP sockets over loopback are covered too (`tests/it/wasip2_typed_sockets.rs`; a connection's output stream has a 64 KiB host send queue, so `write` never waits and `check-write`/`subscribe` report back-pressure, `tests/it/wasip2_typed_send_queue.rs`), also together with files in one component (`tests/it/wasip2_typed_both.rs`; the host source is preceded by `type_aliases(&glue)` from `filter.rs`). UDP over loopback (`udp-create-socket`, bind, `stream` with or without a remote address, `receive`, `check-send`/`send`, which never block, addresses, `subscribe`; `tests/it/wasip2_typed_udp.rs`) and `ip-name-lookup` (`localhost` and numeric addresses locally, other names only with `with_external_network`, else `permanent-resolver-failure`; `tests/it/wasip2_typed_name_lookup.rs`) are covered too; the socket options of UDP (hop limit, buffer sizes, `ipv6-only`) answer `not-supported`.
4. **Full component support** (very large): parse the component, resolve instances
   and `canon lift/lower` per the spec, and generate glue for arbitrary WIT worlds.
   Months of work; only worth it if general components are a goal.

## Recommendation

Do 1 and 2 (cheap, independently useful, and de-risk 3). Then decide on 3 by whether
running *wasip2 Rust programs* natively is a goal; if it is, scope it to the
cli/io/clocks subset first. Option 4 is not justified by anything seen here: for the
common case of one core module plus a WASI adapter, the unbundle recipe plus a host
shim covers the use.
