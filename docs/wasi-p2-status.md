# WASI 0.2 support: status summary

This page is the entry point to wasm2rs's WASI preview2 (0.2.0) support: what works, which PR
brought it, how to use it, and what is still missing. The design details live in the documents
linked from each section.

## Where it stands

- The `wasm2rs-wasi` crate's `WasiHost` implements **all 199 methods of the official WASI 0.2.0
  WIT** (`cli`, `clocks`, `filesystem`, `http`, `io`, `random`, `sockets`). `wasm2rs-wasi/src/generated.rs` is
  generated from the official WIT in `tests/wasi_p2_wit/` and a test fails on any difference.
- A `wasm32-wasip2` **component** can be turned into Rust source from the CLI
  (`wasm2rs --component`), from the library (`transpile_component`,
  `transpile_component_with_exports`), from `build.rs` (`transpile_component_to_out_dir`) and from
  a macro (`wasm2rs_macros::include_component!`). The output holds the program core module, the
  typed import `Glue` over `wasm2rs_wasi::Host`, and the typed `Exports` trait.
- Both directions work: a guest that exports `wasi:cli/run` is run with `Exports::...run`, and a
  guest that exports `wasi:http/incoming-handler` can be served over TCP.

## Features and the PRs that added them

| Feature | PRs | Design doc |
|---|---|---|
| Audit of the surface against the official WIT, and the regression test | #171 | [typed-host-design.md](typed-host-design.md) (`tests/it/wasip2_wit_regression.rs`) |
| Missing non-http methods (filesystem gaps and small gaps) | #170, #172 | [typed-host-design.md](typed-host-design.md) |
| A1: the `wasm2rs-wasi` library (`Host` trait, `--external-types`, `WasiHost` and its builder) | #175 to #180 | [wasm2rs-wasi-design.md](wasm2rs-wasi-design.md) |
| A2: component exports (`wit_exports`, the typed `Exports` trait, `wasip2_run` calls `run`) | #181 to #183, #187, #188 | [component-exports-design.md](component-exports-design.md) |
| `wasi:http` client and server methods (H1 to H5) | #189 to #194 | [wasi-http-design.md](wasi-http-design.md) |
| Serving a component over TCP (`wasip2_serve`) | #196, #210 | [wasi-http-design.md](wasi-http-design.md) |
| Core module linking (adapter components, exports from several modules, dylink-style components such as `componentize-py`) | L1 to L5 (#307 to #310, #312, #315), F2-1 to F2-5 (#322, #324, #326 to #328), #329 | [core-module-linking-design.md](core-module-linking-design.md), [f2-dynamic-linking-design.md](f2-dynamic-linking-design.md), [component-toolchain-survey.md](component-toolchain-survey.md) |
| A3: component input (`transpile_component`, the examples use it, `--component`, `transpile_component_to_out_dir`, `include_component!`, `transpile_component_with_exports`) | #198 to #201, #203, #212 | [component-input-design.md](component-input-design.md) |
| P3: stdin pollable lifecycle, preopen TOCTOU-safe file access | #202, #204 | [typed-host-design.md](typed-host-design.md) |
| `wasi:http` follow-ups (incoming side, streaming, client trailers) | #206, #210, #211 | [wasi-http-design.md](wasi-http-design.md) |
| `wit_exports` module index counts an imported core module | #208 | [component-exports-design.md](component-exports-design.md) |

## Entry points

| You want to | Use |
|---|---|
| Transpile a component from the shell | `wasm2rs --component component.wasm out.rs` (see the README for the flags it accepts) |
| Transpile in Rust code | `wasm2rs::transpile_component(&bytes, &ComponentOptions::default())`; `transpile_component_with_exports` also returns the lifted `Vec<WitExport>` |
| Transpile from `build.rs` | `wasm2rs::build::transpile_component_to_out_dir(path, &ComponentOptions::default())` (`transpile_component_to_dir` for an explicit directory) |
| Include the result | `wasm2rs_macros::include_component!("stem")`: the name is the input's file stem, without `.wasm` |
| Configure the host | `wasm2rs_wasi::WasiHost::builder()` (arguments, environment, preopens, `NetworkPolicy`, clock, random, stdio, `http_client`) |
| Make HTTP requests from a guest | `WasiHost::builder().http_client(UreqClient::new())` with the `http-client` feature |
| Run a `wasi:cli/run` component | `cargo run --release --example wasip2_run -- component.wasm` (`rustc` runs at `-C opt-level=1`; `WASM2RS_EXAMPLE_OPT_LEVEL` selects another level; `0` is needed for the JS guests) |
| Serve a `wasi:http/incoming-handler` component | `cargo run --release --example wasip2_serve -- component.wasm --listen 127.0.0.1:8080` (the handler may be exported at any `0.2.x` patch version) |

## Known limitations

Host behavior:

- A reader given as stdin is read on a thread that starts at the guest's first use of stdin. The
  thread stays until the reader returns, so a reader that blocks keeps it alive.
- `UreqClient` cannot send or receive trailers: a request with non-empty trailers is refused with
  an explicit error, and a response never has a trailers reader. A custom `HttpClient` can.
- If the guest finishes an incoming body before it has been read to its end, `future-trailers.get`
  answers `none`. wasmtime drains the body and delivers the trailers.
- A trailers reader that never returns holds back the end of the response body for the guest: the
  host has no timeout on it.
- `between-bytes-timeout` is approximated with a limit on the whole body in `UreqClient` (`ureq`
  has no per-read timeout), which is stricter than the specification.
- `HttpClientError` carries only a string, so the guest sees `internal-error` with a message
  instead of a structured `error-code` for connection failures.
- File access is confined with `cap-std` (`openat` with `O_NOFOLLOW`). CI runs it on Unix and
  Windows (#205, see [windows.md](windows.md)); only the Unix runs check the symlink escape cases.
- The confinement above is for `wasm2rs-wasi` only. The native WASI preview1 shim of the generated code uses
  `std::fs` after a canonicalize check, which a sibling wasi-threads guest thread or another host
  process can race with a symlink swap (#225, see the notes in
  [cli-reference.md](cli-reference.md)).

Transpilation of a component (errors that say why):

- `InvalidInput`: a core module as the input, a component with no core module, or an export-less
  component with several candidate modules that nothing tells apart.
- `Unsupported`: an export of a nested component, and a string encoding other than UTF-8; a linked
  component (below) whose lifts allocate through different `realloc` functions of several core
  modules (#314, open); a linked core module other than the program that defines its own memory.
- Components whose core modules are wired to each other (a preview1 adapter with `wit-component`'s
  shim and fixup, exports lifted from several core modules, dylink-style libraries) are linked
  into one `Instance` instead of being refused; every other component keeps the single-module
  path, byte-identical. See [core-module-linking-design.md](core-module-linking-design.md) and
  [f2-dynamic-linking-design.md](f2-dynamic-linking-design.md). Which real toolchains work
  (Rust, Rust + preview1 adapter, Go, `jco`, `componentize-py`, `wasi:http` handlers) is in
  [component-toolchain-survey.md](component-toolchain-survey.md); JS guests need
  `-C opt-level=0` (#331) and Python ones a long `rustc` run.
- The CLI cannot turn `external_types` off (`--component` always uses a library, `wasm2rs_wasi`
  by default); only the library API can emit the types into the output.
- The component bytes are parsed several times inside one transpilation (layout scan,
  `wit_exports`, the glue); this only costs time.

## WASI 0.3 (p3): not supported

WASI 0.3.0 (released 2026-06-11) replaces `wasi:io` with the component model's async features.
wasm2rs targets 0.2 only: a component that uses any of the features below is refused with an
`Unsupported` error that names the feature (the error says "WASI 0.3"). Nothing is silently
mis-translated.

| Feature | Where it is refused | Why it is not there |
|---|---|---|
| `async func` types | `src/wit/read.rs` (`P3_ASYNC_FUNC`) | needs the async canonical ABI and a single-threaded task scheduler |
| `future<T>`, `stream<T>`, `error-context` | `src/wit/read.rs` (`P3_FUTURE`, `P3_STREAM`, `P3_ERROR_CONTEXT`) | values the scheduler owns; no `future.*`/`stream.*` built-ins |
| `canon lift ... async` (stackful ABI) | `src/wit/exports.rs` | a guest that blocks inside the host's scheduler; needs suspension across `call_indirect`, which the `cont` backend does not support |
| `canon lift ... async (callback ...)` (callback ABI) | `src/wit/exports.rs` | needs `task.return`, `subtask.*`, `waitable-set.*` and an event loop |
| `core-type` / `gc` canonical options | `src/wit/exports.rs` | component-model GC, not modelled |

The `canon` built-ins of the async ABI (`task.*`, `subtask.*`, `waitable-set.*`, `stream.*`,
`future.*`) are not refused on their own: a core module that imports one gets it as a plain
method of the flat `Imports` trait, which the embedder would have to implement. Real 0.3 guests
also have async types or lifts, so they are refused by the rows above. The `wasm2rs-wasi` host is
built on `wasi:io` pollables and streams, so a 0.3 host would be a separate surface. The
milestone plan (M0 this refusal, M0.5 a spike on what real async guests look like, then reading
`async func` and generating glue, a callback-ABI scheduler, and a 0.3 host) is a proposal, not a
commitment.

## Not done yet

- Escape attempts through symlinks and junctions on Windows (everything else of #205 is done, see [windows.md](windows.md)).
- A4: `resource.new` / `resource.rep` for resources a component defines and exports.
- A structured `HttpClientError`, to map failures onto the specific `error-code` variants.
- An `HttpClient` that really sends and receives trailers (the trait supports it; `UreqClient`
  does not).
- Lifts that allocate through different `realloc` functions of several linked core modules (#314).
- `rustc` at `-C opt-level=1` does not finish on the 125 MB source of a `jco` JS component (#331);
  `-C opt-level=0` works.
