# `wasi:http` host (0.2.0)

Status: H1 is implemented (29 of the 63 `wasi_http_*` methods in `wasm2rs-wasi/src/generated.rs`:
fields, request-options, the parts of outgoing-request, their drops); the others keep the default
trap.
Background: [wasm2rs-wasi-design.md](wasm2rs-wasi-design.md) (the host library),
[component-exports-design.md](component-exports-design.md) (the export side that `incoming-handler`
needs).

## Starting point

- Every resource handle is a `u32` newtype (`Fields`, `OutgoingRequest`, `IncomingBody`, ...); own and
  borrow are the same type. No associated types.
- `WasiHost` keeps each resource kind in a `Slab<T>`; streams share one `u32` space split by range
  (`0..3` stdio, files from `FIRST_FILE_STREAM`, TCP from `TCP_STREAM = 1<<30`), dispatched in
  `read_stream` / `write_stream`. Pollables are `Wait` values (`Copy`, slot indexes into host tables)
  waited in `wait_any`; stdin is fed by a thread through shared state.
- Pluggable parts (`Clock`, `RandomSource`) are boxed trait objects set through the builder.
- There are no HTTP dependencies. `tests/it/wasip2_wit_regression.rs` requires every non-http method to
  be overridden and every http method to still trap; each phase below shrinks that exemption, which
  is the progress measure.

## Decisions

1. **Outgoing first, incoming after.** A guest that is a client (`outgoing-handler.handle`) needs only
   the host trait, and works with existing `wasm32-wasip2` components. A guest that is a server
   (`incoming-handler#handle`) also needs the A2 `Exports` and a way to mint resources.
2. **Injected `HttpClient` trait.** Like `Clock`: a boxed trait object set with a builder method.
   Unset, `handle` returns `ErrorCode::InternalError`. A real client ships behind an optional
   feature `http-client` (default off), on the synchronous `ureq` (the host is synchronous and
   thread-based; no async runtime). `NetworkPolicy` applies: `None` refuses, `Loopback` allows
   loopback addresses only, `External` allows all.
3. **Bodies are streams in a new handle range** (`HTTP_STREAM = 1<<29`), with branches in
   `read_stream`, `write_stream`, drop, `check-write` and the failed-stream sets. A received body
   is filled by a reader thread into a buffer; a request body is buffered and the request is
   executed when `outgoing-body.finish` runs.
4. **`future-incoming-response` is a background thread** that executes the request and fills a shared
   slot. A new `Wait::HttpResponse(slot)` (and one for `future-trailers`) makes it a pollable, like
   the stdin thread. `get` returns `None` until the slot is full.
5. **Fields keep names case-insensitively with several values each.** `set`/`append`/`delete` on a
   forbidden header name return `HeaderError::Forbidden`; fields of a received response or request
   are immutable (`Immutable`); invalid syntax is `InvalidSyntax`.
6. **Incoming API on `WasiHost`.** Before calling `Exports::..._handle` the embedder mints
   `new_incoming_request(..)` and `new_response_outparam()`; afterwards `take_response(outparam)`
   returns the `Result<OutgoingResponse, ErrorCode>` that `response-outparam.set` stored. The first
   version has no trailers and no streaming response (the response is read once `handle` returns).
   `incoming-handler` is not in the generated trait (it is an export), so no trait change.
7. **Limits.** Header count/size, body buffer size and request timeouts (`request-options`
   connect/first-byte/between-bytes) are enforced by the host; a violation maps to the matching
   `ErrorCode` (`HTTP-request-header-size`, `connection-timeout`, ...).

## Phases (one PR each, in order)

| Phase | Content |
|---|---|
| H1 ✅ | `fields`, `request-options`, `outgoing-request` (not its `body`, H3), drops; the `HttpClient` trait and builder method (no real client yet; the trait's shape may still change in H2). |
| H2 | `outgoing-handler.handle`, `future-incoming-response` (thread, `Wait`), `incoming-response`, `incoming-body` (receive side), `http-error-code`. |
| H3 | `outgoing-body` (send side), `future-trailers`, trailers. |
| H4 | `ureq` client behind feature `http-client`; end-to-end tests against a loopback server. |
| H5 | incoming: mint API, `outgoing-response`, `response-outparam`, `incoming-handler#handle` through `Exports`; tests with a `proxy`-world component. |

Each phase updates the exemption in `wasip2_wit_regression.rs` and the README.

## Risks

- TCP-level behaviour (redirects, proxies, TLS) belongs to the client; the trait must not leak
  `ureq` types. TLS is only available through the feature.
- Thread lifetime: a dropped future or body must stop its thread (flag checked between reads).
- `Slab` does not detect stale handles (existing behaviour); a guest's double drop is its own trap
  as elsewhere.
