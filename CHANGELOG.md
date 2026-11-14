# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `--component` supports the `utf16` canonical string encoding for lifted exports (MoonBit components with string exports, #344): the glue transcodes between a Rust `String` and UTF-16 guest memory at the boundary. An unpaired surrogate or an unaligned pointer traps (a panic) instead of being replaced.

- `--component` supports the `utf16` canonical string encoding for lowered imports too (#351): the strings a guest passes to a host import are read as UTF-16, and the strings the host returns are written as UTF-16 code units. An unpaired surrogate or an unaligned pointer traps instead of being replaced.

### Changed
- Breaking (library): `WitExport::utf8: bool` is replaced by `WitExport::encoding: WitStringEncoding` (`Utf8` or `Utf16`).
- An import lowered with `compact-utf16`, or with both `utf8` and `utf16`, whose signature holds a string is now an `Unsupported` error naming it; its strings used to be read as UTF-8 without any error. `compact-utf16` lifts stay unsupported, with a message naming the encoding.

## [0.5.0] - 2026-11-14

`wasm2rs` and `wasm2rs-macros` 0.5.0, `wasm2rs-wasi` 0.1.1 (a `Host` method with a default implementation was added; compatible with 0.1.0). `wasm2rs-macros` has no code change; its version follows `wasm2rs`.

### Highlights
- Component support now covers real toolchain output: components made with a preview1 adapter (Rust, wasi-sdk C/C++; Go transpiles but runs only with an empty environment) and dynamically linked ones (`componentize-py`, CPython as 14 linked core modules) are transpiled by linking their core modules into one `Instance`.
- Type-checking (`rustc` metadata) of large components is about 5 to 10 times faster: dropping redundant parentheses from generated code removes `rustc`'s quadratic `unused_parens` pass (124 MB JS component: about 587 s to about 55 s; Go component: 66 s to 12 s).
- Verified end to end with real toolchains: Rust, Python (`componentize-py`), JavaScript, TinyGo, wasi-sdk C and C++, and `cargo-component`; real `wasi:http` handlers (Rust, JS, Python) are served.
- `wasm2rs-wasi` gains `wasi:cli/exit`'s `exit-with-code` through a WIT overlay on 0.2.0.

### Changed
- The generated source no longer wraps call/method arguments, assigned values and block tails in redundant parentheses (`self.w32((a + b), v)` is now `self.w32(a + b, v)`). `rustc`'s `unused_parens` early lint copies the rest of the file for every such site, which is quadratic on very large modules: the 124 MB JS component core goes from about 587 s to about 55 s (metadata) and the Go component from 66 s to 12 s. Behaviour and machine code are unchanged (F4-1, #316).

### Added
- Real-toolchain components are pinned by ignored end-to-end tests: JavaScript (`js-hello`, #330), `wasi:http` handlers from Rust, JS and Python served through `wasip2_serve` (#332), TinyGo, wasi-sdk C and `cargo-component` (#340), plus a test pinning the linker's refusals of unfoldable library shapes (#338). The `wasm2rs-wasi` sockets test no longer flakes on Windows from loopback port reuse (#337). Codecov reports per-crate coverage targets (#319).
- wasi-sdk C++ components (`clang++ --target=wasm32-wasip2`, `std::cout`) build and run. They import `wasi:cli/exit@0.2.12`'s `exit-with-code`, which the 0.2.0 WIT `wasm2rs-wasi`'s `Host` trait is generated from lacks (`rustc` failed with E0599). `tests/wasi_p2_wit/overlay/` now holds the additions to the 0.2.0 WIT that toolchains import (only `exit-with-code`; the other 0.2.1 to 0.2.12 differences are listed in `docs/wasi-post-0.2.0-additions.md`), `Host` gains `wasi_cli_exit_exit_with_code` and `WasiHost` exits with `status-code`. Output for components that do not import it is unchanged. Pinned by the ignored test `a_real_wasi_sdk_cpp_component_prints_hello` (`WASM2RS_WASISDK_CPP_HELLO_COMPONENT`) (#339).
- `WASM2RS_EXAMPLE_OPT_LEVEL` selects the `rustc -C opt-level` of `examples/wasip2_run` and `wasip2_serve` (default 1): the 125 MB JS component builds at 0 in about 90 s but LLVM did not finish at 1 in 80 minutes (#331).
- `componentize-py` components (CPython as dynamically linked libraries, 14 core modules) transpile, build and run: `py-hello.wasm` prints `hello from python` (122 MB source, about 5.5 minutes and 7.5 GB for `rustc -C opt-level=1`). Pinned by the ignored test `a_real_componentize_py_component_prints_hello` (`WASM2RS_PY_HELLO_COMPONENT`) (F2-5).
- Linked core modules (dylink-style components): an appended module may use the saturating float-to-int operators (`i32.trunc_sat_f64_s`, ...), and an element or data segment whose offset reads an imported global (`__table_base`, `__memory_base`) is placed at the constant the providing module defines (an immutable integer global with a constant initializer; any other provider is refused with an error naming the offset). Single-module output is unchanged (F2-3, #318).
- A linked component whose lowered imports allocate through different `realloc` functions (an adapter's, `libc`'s and a binding library's, as in a dylink component) is no longer refused: each lowered import allocates its string and list results through its own `realloc`, exported from the unit as `cabi_import_realloc`, `cabi_import_realloc_1`, ... A component with one import `realloc` generates the same code as before (F2-2, #318).
- A lift's `memory` option may name the memory a linked library imports from the program (a dylink-style component: `main` defines and exports the memory, the lifted module imports `env.memory`). It was refused as "not an export of the same core instance". A memory the lifted module does not use is still refused. Single-module components are unchanged (F2-1, #318).
- `transpile_component` / `--component` now transpile components made with a preview1 adapter (`wasm-tools component new --adapt`, wasi-sdk, Go): the program, the adapter and `wit-component`'s shim and fixup modules are linked into one `Instance`, the adapter's `wasi_snapshot_preview1` functions become internal calls, and the host sees only the WIT functions the adapter calls (`wasi:cli/*`, `wasi:io/*`, ...). Exports lifted from several core modules are linked the same way instead of being refused. Every component that transpiled before takes the unchanged single-module path, so its output is byte-identical.

### Fixed
- `transpile_component`: a component whose exports are all lifted from one core module no longer drops the other instantiated modules' initialization. An instantiated sibling with a start function, or with an active data or element segment in a memory or table it shares with the lifted module (a dylink library filling the table `main` calls through), now takes the linked path; before, its segments and start function were silently ignored. Components without such a sibling, including `wit-component`'s shim/fixup (whose table is its own), keep the unchanged single-module path (#325).
- `transpile_component` with the default options (`external_types = wasm2rs_wasi`): a record, variant, enum, flags or resource that no WASI interface defines (a component's own export types) that only the component's exports use (so the library cannot have declared it) is now declared in the glue instead of being named as `wasm2rs_wasi::component::RecordN`, which does not exist (`E0433`). The types the imports use are still taken from the library (a WASI type that only an export uses is therefore a separate, local type). Output for components whose exports use only the imports' types is unchanged (F2-4, #317).
- `transpile_component`: a component whose program has no host import at all (so `Instance` takes no host and there is no `Imports` trait) now gets glue over plain `Instance` (`Exports` for `Instance`, no `Glue`), instead of an `impl Imports` for a trait that does not exist (#323).
- Exports lifted from several core modules no longer fail when the modules use the same core export name (`run`, `cabi_realloc`, `post-return`, ...) (#185, #186). A linked unit exports only what the component lifts plus the `realloc` and `post-return` they name, under names taken from the lifts' WIT names (`interface#name`, `cabi_post_...`), so the `Exports` method names do not depend on the core modules' own names (a linked component's method is named after the lift, e.g. `ea` for `(export "ea" ...)`, not the core export, which was never released); the modules' other (internal wiring) exports are dropped. Limit: lifts that allocate through *different* `realloc` functions of several modules are refused with an error naming the lift and module (#314). Single-module output is unchanged.
- Linked modules that declare the same function signature now share one canonical type, so a `call_indirect` through a table filled by another appended module (the shim/fixup trampolines) reaches its target instead of trapping with `indirect call type mismatch`.
- A string or list that a lowered import returns is allocated through the `realloc` the component names for that lowering (an adapter's `cabi_import_realloc`) instead of the lifts' `cabi_realloc` (#184).
- `transpile_component`: the generated `Glue` now implements exactly the `Imports` methods of the program module, one per core import in import order, delegating to the `Host` method of the WIT function it lowers. A module that names an interface by another compatible version than the component's own import (`@0.2.0` vs `@0.2.12`), or imports one function under several versions, no longer fails to build for the missing `Imports` methods. `wit_host_glue` (no module at hand) is unchanged.
- `examples/wasip2_run` accepts `wasi:cli/run` exported at a newer patch version than `0.2.0`: a component made with the preview1 adapter exports `wasi:cli/run@0.2.3`, which was reported as "does not export `wasi:cli/run@0.2.0#run`". A real Rust `wasm32-wasip1` + adapter component now runs.
- `examples/wasip2_serve` serves a `wasi:http/incoming-handler` exported at any `0.2.x` patch version (the `wasi` 0.14 crate exports `@0.2.12`): the generated `Exports` method name follows the exported version instead of being fixed to `@0.2.0`. Real Rust, `jco` (JS) and `componentize-py` handlers now answer over HTTP (#332).

## [0.4.0] - 2026-11-12

`wasm2rs`, `wasm2rs-macros` and (first publication) `wasm2rs-wasi` 0.1.0.

### Added
- Opt-in `TranspileOptions::simd_intrinsics` (CLI `--simd-intrinsics`, `include_wasm!` option `simd_intrinsics`) — improvement item A10: the integer SIMD lane helpers (add/sub/mul/neg/abs, saturating add/sub, `avgr_u`, integer compares) emit `core::arch` NEON (aarch64) / SSE2 (x86_64) bodies with the scalar loop kept as the `cfg` fallback. About 1.6x on a mixed integer-lane kernel at `-O3` (up to ~9x on `u8` saturating add in a microbenchmark). Bit-identical to the scalar loop (differential test `tests/it/simd_intrinsics.rs`); adds `unsafe` to the generated code, so it is off by default and default output is byte-identical.
- Opt-in call-depth limit (`TranspileOptions::call_depth_limit`, CLI `--call-depth-limit=N`): unbounded guest recursion traps with `panic!("call stack exhausted")` instead of exhausting the native stack and aborting the host. A per-thread RAII counter; `return_call*` chains stay at one activation. Off by default, so existing output is byte-identical. The spec harness now replays `assert_exhaustion` as a trap.

### Changed
- Generated continuation code no longer threads the unused continuation type argument: `cont_new`/`cont_alloc`/`cont_handle`/`cont_bind` lose their `__ty` parameter and `StepResult::Switch` its `ty` field (#284). Only continuation modules' output changes, and only by this removal.

### Fixed
- The `Cargo.toml` emitted with split output (`cargo_manifest`) no longer sets `panic = "abort"`, which made every Wasm exception and host-caught trap abort the process in release builds.

### Changed
- **Breaking:** `TranspileOptions` is now `#[non_exhaustive]`, so adding an option no longer breaks callers (same release as the `TranspileError` change below). A struct literal (even with `..Default::default()`) no longer compiles outside the crate: start from `TranspileOptions::default()` and assign fields or chain the new `with_*` methods (`TranspileOptions::default().with_named_exports(true)`). Generated code is byte-identical. Internally, the `reentrant_imports` contradiction checks run right after parsing, before any function body is generated (a module that is wrong both that way and in `entry_exports` now reports the `reentrant_imports` error first), and codegen reads the options directly instead of a hand-copied subset. The `transpile*`/`*_with_options` entry points are kept as they are; folding them together is left for a later breaking release.
- **Breaking:** `TranspileError` is now `#[non_exhaustive]` and gained `InvalidOptions` (contradictory
  options, e.g. `reentrant_imports` with an imported memory, or an unknown `entry_exports` name),
  `InvalidInput` (a component given to `transpile`, or a component with no usable core module) and
  `Internal` (transpiler invariants, formerly `Unsupported`), plus `InFunction { index, source }`,
  which wraps any error raised while generating a function body. Matching `Unsupported` directly no
  longer sees those cases: add a wildcard arm and use the new `TranspileError::root()` to look
  through `InFunction`. `Unsupported` messages are unchanged and generated code is byte-identical.
- `transpile` validates the input wasm first (`wasmparser::Validator::validate_all`, all features) and rejects an invalid module with `TranspileError::Parse` instead of generating wrong Rust for it. The spec harness now replays `assert_invalid` and binary `assert_malformed` as "must be rejected".

### Added
- `transpile_component_with_exports`: `transpile_component` that also returns the lifted `Vec<WitExport>`, so a caller checking the exports no longer runs a second `wit_exports` pass (`transpile_component` is now a wrapper). `examples/common::transpile_exporting` uses it.
- `transpile_component` and `ComponentOptions` (#198): a component in, one Rust source out (the program core module picked by the exports' `canon lift`, then the `Glue` for `wasm2rs-wasi` and the typed `Exports`); `named_imports`, `named_exports` and `reentrant_imports` are forced on. The CLI gets `--component` (#200), `wasm2rs::build::transpile_component_to_out_dir` / `transpile_component_to_dir` write `$OUT_DIR/{stem}.rs` for a build script (#201), and `wasm2rs_macros::include_component!("stem")` includes it (a name ending in `.wasm` is a compile error, #203).
- `wit_exports` and `WitExport` decode the `canon lift` side of a component (#182), and `WitOptions::exports` generates a typed `Exports` trait with one method per exported WIT function, implemented for `Instance<Glue<H>>` (#183). `examples/wasip2_run.rs` calls the typed `wasi:cli/run` through it (#187).
- `WitNaming::Qualified` (types in one module per defining interface), `WitOptions::external_types` and the CLI flag `--wit-host --glue --external-types=<path>`: the glue refers to a library's `Host` and types instead of declaring them (#175).
- `wasm2rs-wasi`: `WasiHost::builder()` (`WasiHostBuilder`) with several preopens (`Access`), a `NetworkPolicy`, injectable `Clock` and `RandomSource` (the OS CSPRNG by default, feature `os-random`) and injectable stdin, stdout and stderr (#178 to #180).
- `wasm2rs-wasi`: the six `wasi:filesystem/types` items that were missing (`get-type`, `read`, `write`, `advise`, `is-same-object`, `filesystem-error-code`, #170) and small gaps in io/poll, clocks, cli/environment and TCP (`address-family`) (#172); `tests/it/wasip2_wit_regression.rs` checks the host against the official 0.2.0 WIT (#171).
- `wasm2rs-wasi`: `wasi:http` client side: `outgoing-handler.handle`, request and response bodies, `future-trailers`, and the `HttpClient` trait with `UreqClient` behind the feature `http-client` (#190 to #193).
- `examples/wasip2_serve.rs`: serves a component exporting `wasi:http/incoming-handler` over TCP (#196).
- `wasm2rs-wasi`: response streaming and incoming-side trailers. **Breaking:** `ServedResponse::body` is a `ServedBody` (`io::Read`, follows the guest's `outgoing-body`; an open body no longer fails `take_response`) and `IncomingRequestParts` has a `trailers` field (delivered by `future-trailers.get`); a response body may finish with trailers (`ServedBody::trailers`). `examples/wasip2_serve` sends such bodies chunked (`--body-timeout`).
- `wasm2rs-wasi`: client-side trailers (#211). **Breaking:** `HttpRequest` has a `trailers` field (what `outgoing-body.finish` was given, checked like headers) and `HttpResponse` has a `trailers: Option<ResponseTrailers>` reader called after the body, delivered by `future-trailers.get`. `UreqClient` refuses a request with trailers (explicit `HttpClientError`) and never receives any.
- `wasm2rs-wasi`: `response-outparam` handles are never reused, so a stale one can no longer write into another call's answer; pollables (`Wait::Tcp*`/`Http*`/`Udp*`) carry the slot's epoch and no longer follow a resource that took the slot of a dropped one; `outgoing-response.set-status-code` accepts `100..=999` (wasmtime's range).
- `wasm2rs-wasi` (new crate, version 0.1.0): the typed WASI 0.2.0 host as a library. `Host`
  declares all 199 methods of the official 0.2.0 WIT, each defaulting to a trap, with the types in
  one module per interface; `WasiHost` is the former `examples/wasip2_host/typed.rs` (`TypedHost`),
  behaviour unchanged. A component's glue is generated with `--wit-host --glue
  --external-types=wasm2rs_wasi` and linked against the library; no host source is spliced and
  compiled per program any more.
- `WitOptions::default_trap`: every method of the generated `Host` trait gets a default body that
  panics naming the method.

- `wasm2rs-wasi`: `wasi:http/types` `fields`, `request-options` and the parts of `outgoing-request`
  (method, path-with-query, scheme, authority, headers), and the `HttpClient` trait with
  `WasiHostBuilder::http_client` (the first step; `outgoing-handler.handle` and bodies followed, see above).
- `wasm2rs-wasi`: the incoming side of `wasi:http` (a guest exporting `incoming-handler#handle`):
  `incoming-request`, `outgoing-response` and `response-outparam`, with `WasiHost::new_incoming_request`,
  `new_response_outparam` and `take_response` (the answer as `ServedResponse`, read once `handle`
  returns; streaming and trailers followed, see above). `WasiHost` now overrides every method of the WASI 0.2.0 surface.

### Changed
- `examples/wasip2_run.rs` links the `wasm2rs-wasi` rlib instead of embedding the host source; a
  component importing a function the host lacks (`wasi:http`) now builds and traps at the call
  instead of failing with E0046.
- The generated doc comments of the `Host` trait escape `[` and `]` (no rustdoc link warnings).

### Fixed
- `wasm2rs-wasi`: a preopen can no longer be escaped by swapping a path component for a symlink
  after a check: paths are resolved below directory handles (`cap-std`, `openat` with `O_NOFOLLOW`
  per component) in one step (#204).
- `wit_exports`: an imported core module takes a slot in the module index space (as in
  `transpile_component`), so the modules after it are no longer off by one.
- `wasm2rs-wasi`: `pollable.drop` frees the pollable's slot for reuse (a subscribe/drop loop no
  longer grows the table), and the stdin thread ends after the host is dropped (once its pending
  read returns).

### Removed
- `examples/wasip2_host/typed.rs` and `filter.rs` (`keep_declared_methods`, `type_aliases`, the
  `// optional:` markers) and the allowlist of `tests/it/wasip2_wit_regression.rs`.

## [0.3.1] - 2026-11-08

Only `wasm2rs-macros` is released as 0.3.1; `wasm2rs` stays at 0.3.0.

### Added

- `wasm2rs-macros` has a README, so its crates.io page is no longer empty.

## [0.3.0] - 2026-11-08

### Migrating from 0.2.0

- `TranspileOptions` gained the public fields `named_exports`, `named_imports`, `import_memory` and
  `reentrant_imports` (all `false` by default, so the default output is unchanged). A struct literal
  that lists every field no longer compiles: add `..Default::default()`.
- A Component Model binary is rejected with `TranspileError::InvalidInput` (`Unsupported` before the error classification) instead of generating
  its inner modules. Use `--extract-core` / `extract_core_modules` first.
- `memory.grow` now fails (returns -1) past the declared maximum of a memory.
- `--dump` rejects code-generation flags and extra arguments instead of ignoring them; `--glue` is
  only valid with `--wit-host`.
- `examples/wasip2_run.rs` takes a `wasm32-wasip2` component (see Changed); the hand-written
  `P2Host` is removed (see Removed).
- Several code-generation semantics were corrected towards the specification (segment init/drop,
  negative `i64` offsets, data/element application order, imported and shared memory/table
  combinations); see Fixed.
- No public API was removed. The new `wasm2rs-macros` crate provides `include_wasm!`.

### Added

- `CONTRIBUTING.md` (build, test and release procedure, changelog rule).
- `fuzz-diff/`, a differential fuzzer that compares generated code with wasmtime, with feature
  profiles (simd, tail-call, extended-const, exceptions); see `docs/differential-fuzzing.md`.
- `examples/wasip2_host/typed.rs`: a TCP output stream has a host-side send queue (64 KiB). `write`
  appends to it and moves what the non-blocking socket takes, never waiting; `check-write` drains
  the queue first and returns `min(4096, room)` (0 when full), and a `write` or `write-zeroes` over
  that traps; the output-stream `subscribe` is ready while the queue has room (or the connection
  failed). `blocking-write-and-flush`, `blocking-write-zeroes-and-flush`, and `blocking-flush` empty
  the queue first (waiting for the peer, so the bytes stay in order); `splice` and `blocking-splice`
  stay within the permit (see Fixed); `flush` only drains as far as the socket takes. Dropping the
  output stream or `shutdown` of the send side flushes the queue first (waiting for a peer that is
  alive but not reading; a dead peer discards it); `exit` and dropping the host (`wasip2_run` does
  so before it exits) do the same for every connection, waiting up to 5 seconds in all. After a
  `flush` that left bytes queued, `check-write` is 0 and the pollable not ready until they are sent.
  stdout, stderr and files still permit 4096. Tests in `tests/it/wasip2_typed_send_queue.rs`.
- `examples/wasip2_host/typed.rs`: `output-stream.splice` and `blocking-splice`, the last
  `wasi:io/streams` methods: they move at most `min(len, 4096)` bytes from an input stream
  (a file, a connection or stdin) to the output stream and return the count. `splice` does not
  wait for the source (`ok(0)` while an open source has nothing yet, `closed` at its end; a
  zero `len` is ok while both streams are open); `blocking-splice` waits for at least one
  byte. A dropped output is `closed` before anything is read. Known limitation:
  the bytes already read from the source are lost if the write then fails. Tests in
  `tests/it/wasip2_typed_splice.rs`.
- `examples/wasip2_host/typed.rs`: the TCP socket options (`is-listening`, `set-listen-backlog-size`,
  keep-alive enabled/idle-time/interval/count, `hop-limit`, `receive-buffer-size`,
  `send-buffer-size`) are recorded per socket with default values, validated (zero is
  `invalid-argument`), read back and inherited by accepted sockets, but not applied to the OS
  socket. The backlog size is not recorded: it is only validated (`invalid-argument` for zero,
  `invalid-state` once a connection is started or made) and ignored. A real `std::net` TCP program now builds.

- `examples/wasip2_host/typed.rs`: a WASI preview2 host written against the generated typed `Host`
  trait (no canonical-ABI code): stdout/stderr, the environment, exit, output streams,
  pollables, `poll`, and the terminal queries, enough for a real
  `wasm32-wasip2` `println!` program (checked by hand with one, and by tests that declare the
  imports in a component). It also serves the arguments (`TypedHost::with_arguments`), the
  clocks (monotonic `now`, `subscribe-duration` and `subscribe-instant`, wall-clock `now`) and the
  real process stdin (a reader thread fills a buffer, `read` never blocks and ends with `closed`,
  the stdin pollable wakes on data and at the end of the input; tests in
  `tests/it/wasip2_typed_cli.rs`, `tests/it/wasip2_typed_clocks.rs` and
  `tests/it/wasip2_typed_stdin.rs`). It also serves `wasi:random` and `wasi:filesystem`
  (`TypedHost::with_preopen`, read-only: `get-directories`, `open-at`, `read-via-stream`, `stat`,
  `stat-at`, `get-flags`, `read-directory`, `readlink-at`, `metadata-hash`, file
  `input-stream.read`; `TypedHost::with_writable_preopen` adds `open-at` with create/exclusive/
  truncate/write, `write-via-stream`, `append-via-stream`, file `output-stream` writes,
  `create-directory-at`, `remove-directory-at`, `unlink-file-at`, `rename-at`, `symlink-at`,
  `link-at`, `set-size`, `sync`, `sync-data`, `set-times` and `set-times-at`; tests in `tests/it/wasip2_typed_fs.rs` and
  `tests/it/wasip2_typed_fs_write.rs`). TCP over loopback too (`create-tcp-socket`, bind, listen, connect, accept, addresses,
  `subscribe`, `shutdown` and the connection streams; `TypedHost::with_external_network` lifts the
  loopback limit; tests in `tests/it/wasip2_typed_sockets.rs`). A component may import both
  filesystem and socket interfaces: the second of their two `error-code` enums gets a numeric suffix
  (`ErrorCode2`) in the generated trait, depending on import order, so the host names them through `FsErrorCode` / `NetErrorCode` aliases that
  `type_aliases` (`filter.rs`) emits (test in `tests/it/wasip2_typed_both.rs`).
- `examples/wasip2_host/typed.rs`: UDP over loopback (`udp-create-socket`, `udp-socket`
  bind, `stream`, addresses, `subscribe`; `incoming-datagram-stream.receive` and
  `outgoing-datagram-stream.check-send`/`send`, which never block; a stream made with a remote
  address drops other senders' datagrams) and `ip-name-lookup` (`localhost` and numeric
  addresses resolve locally; other names only with `with_external_network`, else
  `permanent-resolver-failure`). The UDP socket options answer `not-supported`. `type_aliases`
  (`filter.rs`) now finds `NetErrorCode` from udp or ip-name-lookup too. Tests in
  `tests/it/wasip2_typed_udp.rs` and `tests/it/wasip2_typed_name_lookup.rs`.
- `examples/wasip2_host/typed.rs`: the remaining `wasi:io/streams` methods except `splice` and
  `blocking-splice`: `input-stream.blocking-read`, `skip` and `blocking-skip` (the blocking ones
  wait on stdin or a connection like the stream's `subscribe` pollable; a file never waits; at
  most 4096 bytes per call), and `output-stream.flush`, `blocking-write-and-flush`,
  `write-zeroes` and `blocking-write-zeroes-and-flush` (more than 4096 bytes, what `check-write`
  returns, traps). (At this point a write to a connection waited for room in the socket's send
  buffer; the send queue entry above replaced that: `write` never waits; the blocking
  writes and flushes, dropping the output stream, `shutdown` of the send side and
  `blocking-splice` to a connection still wait for the peer to read.) Tests in `tests/it/wasip2_typed_stdin.rs`,
  `tests/it/wasip2_typed_sockets.rs`, `tests/it/wasip2_typed_fs_write.rs` and
  `tests/it/wasip2_typed_streams.rs`.
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
  are an `Unsupported` error naming the type. `wit_functions` returns the decoded `WitFunc`/`WitType` list. (The glue came with `wit_host_glue`, above.)
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
- The WASI preview2 host example can let a program write files (first through `P2Host::with_writable_preopen`, since removed; the typed host provides the same):
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
  and the socket options (keep-alive, buffer sizes, hop limit, backlog) were not implemented then (they are recorded or validated now, see above).
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

### Changed

- `examples/wasip2_host/typed.rs`: a failed stream operation is `stream-error::last-operation-failed`
  with a live `wasi:io/error` resource, as the spec says, instead of `closed` (guest-visible
  change). It is reported once, by the first operation that finds the failure; after that the
  stream is `closed` for every operation (a zero-length read and `subscribe` readiness included:
  a failed stream stays ready). A connection whose socket failed (a reset, or a send queue that
  could not be flushed), a read error on a connection or a file, and a failed write to stdout,
  stderr or a file are `last-operation-failed`; the end of a stream and a dropped one stay
  `closed`. Every such answer carries a fresh `error` handle that the guest drops
  (`resource-drop`), which frees the host's slot; `[method]error.to-debug-string` (when the world
  imports it) returns the OS message. A failed peer in the send-queue test now answers
  `last-operation-failed` once, then `closed`. A stdin read error is reported once (after the bytes
  read before it) as `last-operation-failed` instead of a plain `closed`; the input is `closed`
  after that.
  Tests in `tests/it/wasip2_typed_io_error.rs`, `wasip2_typed_send_queue.rs`,
  `wasip2_typed_fs_edges.rs` and `wasip2_typed_stdin.rs`.

- **Breaking (example):** `examples/wasip2_run.rs` now takes a `wasm32-wasip2` component
  (`cargo run --release --example wasip2_run -- <component.wasm> [args...]`), not an
  extracted core module. It runs the component's largest core module through the typed host
  `examples/wasip2_host/typed.rs` and the generated glue (`wit_host_glue`) instead of
  the hand-written `examples/wasip2_host/host.rs` (since removed). A component importing
  something `typed.rs` does not implement fails to build with rustc's E0046 errors and a hint
  (exit code 2). Tested end to end in `tests/it/wasip2_run_example.rs`.

- `wasm2rs --dump` and `--extract-core` refuse the flags that only change the generated code
  (`--unsafe-memory`, `--named-exports`, `--named-imports`, `--import-memory`,
  `--reentrant-imports`, `--split-dispatch`, `--jobs`, `--entry-exports`) instead of
  silently ignoring them; `--extract-core` also refuses `--dump`.
- `docs/component-model-poc.md`: what a wasip2 component contains, that its core module
  already transpiles, and how far its preview2 imports are from the preview1 support.

### Removed

- The hand-written WASI preview2 host `examples/wasip2_host/host.rs` (`P2Host`, written against
  the flat `Imports`) and its tests `tests/it/wasip2_host.rs` are removed in favor of the typed
  host `examples/wasip2_host/typed.rs`, generated from WIT. Both hosts map `wasi:cli/exit` to
  the exit codes 0 and 1 (`exit` takes only ok/err).

### Fixed

- `memory.grow` now enforces the memory's declared maximum, not only the wasm32 limit of 65536
  pages: `(memory 1 2)` could grow to three pages; the grow now returns -1 and leaves the size
  unchanged. Each memory uses its own bound (multi-memory, `--import-memory`, an imported memory,
  and memory64); `unsafe_memory` output is unaffected. A shared memory the module defines already
  honoured its maximum through its reservation (an imported shared memory relies on the host's
  reservation). A memory without a declared maximum generates the same code
  as before. Not covered: a maximum the host sets on the `Vec<u8>` it lends, validation of an
  import's limits at instantiation, and allocation failure on a 32-bit memory. Follows up the
  closed PR #7 by Nicolas0315, which reported the bug. Tests in `tests/it/state.rs`,
  `imported_memory.rs`, `multi_memory.rs`, `atomics_threads.rs`, `memory64.rs`, `unsafe_memory.rs`.

- `examples/wasip2_host/typed.rs`: `read-via-stream` on a descriptor opened for writing only
  returns `bad-descriptor` (as wasmtime does) instead of a stream whose reads fail; a descriptor
  with `read`, or with no flags at all, still reads. `get-flags` reports what the descriptor was
  opened for (a write-only one no longer reports `read`). Tests in
  `tests/it/wasip2_typed_fs_edges.rs` and `tests/it/wasip2_typed_fs_write.rs`.

- `examples/wasip2_host/typed.rs`: `open-at` with `create` + `exclusive` on a symlink name returns
  `exist` also with `symlink-follow` (it used to be `not-permitted` for a dangling link, as POSIX
  `O_CREAT | O_EXCL` never follows a link). `shutdown` finds the connection's send queue
  through a slot (and a unique id, so a slot reused by another connection is not mistaken for
  it) kept in `NetState::Connected`, not by matching socket addresses; it no longer flushes (and
  waits on) another connection's queue after the guest dropped the output stream. Tests in
  `tests/it/wasip2_typed_fs_symlink.rs` and `tests/it/wasip2_typed_send_queue.rs`.

- `examples/wasip2_host/typed.rs`: `splice` and `blocking-splice` to a TCP output stream respect
  `check-write`. They used to read `min(len, 4096)` from the source and then wait for the peer
  to read the whole send queue, so a stalled peer hung the guest and the bytes could not be
  taken back. Now a connection takes at most the permit: with a full queue (permit 0) `splice`
  returns `ok(0)` and leaves the source untouched; `blocking-splice` first waits for the queue
  to empty (the peer to read), then moves up to the permit. The bytes are queued like `write`'s
  and neither call waits for the peer afterwards. stdout, stderr and files are unchanged.
  Tests in `tests/it/wasip2_typed_splice_permit.rs`.

- `examples/wasip2_host/typed.rs`: `output-stream.write` of more than 4096 bytes (what `check-write`
  returns) traps, like `blocking-write-and-flush`, `write-zeroes` and
  `blocking-write-zeroes-and-flush` (`wasi:io` 0.2: writing more than `check-write` allows is a trap).
- `examples/wasip2_host/typed.rs`: a zero-length `read`/`blocking-read`/`skip`/`blocking-skip` on
  stdin, a connection or a file succeeds (empty) only while the stream is open and is `closed` at its
  end (as `wasi:io` 0.2 specifies); the host probes without consuming data. The blocking variants
  wait until the stream is readable (data or its end) first, as wasmtime does, then answer the same.
- `examples/wasip2_host/typed.rs`: `descriptor.open-at` honors `symlink-follow`. Without it a symlink
  as the last component is `loop` (like `O_NOFOLLOW`), also with `create` (`exist` with `create` +
  `exclusive`); links earlier in the path
  and paths naming a directory itself resolve as before, and a link out of the preopen is `loop`
  without the flag and `not-permitted` with it.
- `examples/wasip2_host/typed.rs`: `descriptor.stat-at` and `descriptor.metadata-hash-at` honor
  `symlink-follow`. Without it they describe the symlink itself (type `symbolic-link`, the link's own
  metadata hash) instead of its target, and a link that leads out of the preopen is not an error then.
- `wit_host_trait` / `wit_host_glue` / `wit_functions` (and `--wit-host`, `--glue`) declare
  `[resource-drop]<r>` only for resources whose `canon resource.drop` the component lowers (with no
  top-level core module, a nested component that lowers a drop, or an unresolvable drop, every drop is kept). A `wasm32-wasip2` program using
  `std::net` never drops `network`, so its glue no longer fails with E0407 (a method that is
  not a member of `Imports`).
- `examples/wasip2_run.rs`: a panic during the `run` call (a guest trap, or a panic inside a
  host import it calls) exits with code 134 and `wasip2_run: the program trapped` after its
  panic message, instead of a second host-side panic (101); panics outside the call (host
  setup) stay 101. The guest's `argv[0]` is the component path as given on the command
  line (it was `<work dir>/program`).

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

[Unreleased]: https://github.com/hirosassa/wasm2rs/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/hirosassa/wasm2rs/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/hirosassa/wasm2rs/compare/wasm2rs-macros-v0.3.1...v0.4.0
[0.3.1]: https://github.com/hirosassa/wasm2rs/compare/v0.3.0...wasm2rs-macros-v0.3.1
[0.3.0]: https://github.com/hirosassa/wasm2rs/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/hirosassa/wasm2rs/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/hirosassa/wasm2rs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hirosassa/wasm2rs/releases/tag/v0.1.0
