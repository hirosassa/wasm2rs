# `wasi:http` host (0.2.0)

Status: H1, H2 and H3 are implemented (48 of the 63 `wasi_http_*` methods in
`wasm2rs-wasi/src/generated.rs`: fields, request-options, the parts of outgoing-request,
`outgoing-handler.handle`, future-incoming-response, incoming-response, incoming-body (receive
side), `http-error-code`, outgoing-body (send side), future-trailers, their drops); the others keep the default trap.
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
- There are no HTTP dependencies. `tests/it/wasip2_wit_regression.rs` requires every method, `wasi:http`
  included, to be overridden (until H5 the http ones were exempt, which was the progress measure).

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
| H2 ✅ | `outgoing-handler.handle`, `future-incoming-response` (thread, `Wait`), `incoming-response`, `incoming-body` (receive side), `http-error-code`. The `HttpClient` signature is unchanged; the host shares the client between request threads behind a mutex (`send` runs one at a time, bodies are read outside it). A client error is `internal-error` with its message; a policy refusal is `HTTP-request-denied`. `incoming-body.finish` returns a `future-trailers` that can only be dropped (H3 adds its methods). |
| H3 ✅ | `outgoing-request.body`, `outgoing-body.write` / `finish`, `future-trailers.subscribe` / `get`. The body is a buffer behind an output stream (`HTTP_OUT_STREAM`, range above the input streams): `check-write` is the room left under the limit (`max_request_body`, 1 MiB) up to 4096, a write past the limit fails the stream (`last-operation-failed` carrying `HTTP-request-body-size`, then `closed`) and `finish` / the future report the same code. `handle` may come before or after the body is written: its thread waits for `finish` and only then calls the client; a body dropped without `finish` (or finished with trailers) fails the future with `internal-error` and the client never runs. **Trailers are not supported**: `HttpRequest` has no field for them and the `HttpClient` trait stays as it is (`ureq` cannot send them either), so `finish` with trailers is `internal-error("request trailers are not supported")`. A received message has no trailers either (`HttpResponse` carries none; `ureq` exposes none), so `future-trailers.get` answers `some(ok(ok(none)))` once, then `some(err)`; both are for H5/a richer client to lift. `UreqClient` refuses a non-empty body on `HEAD`/`CONNECT` (`ureq`'s `run` would send it); like every client error it reaches the guest as `internal-error`. |
| H4 ✅ | `ureq` client behind feature `http-client`; end-to-end tests against a loopback server. |
| H5 ✅ | incoming: `incoming-request` (method, path-with-query, scheme, authority, headers, `consume`), `outgoing-response` (status `100..=999` (wasmtime's range), default `200`; headers immutable once built; `body` once), `response-outparam.set`, and their drops. The embedder mints `new_incoming_request(IncomingRequestParts)` and `new_response_outparam()`, calls `Exports::export_wasi_http_incoming_handler_0_2_0_handle` (no change to the glue or `src/wit` was needed: `own<..>` resource parameters of an export already work) and then `take_response(outparam)`, which returns `None` when the guest never set one, else `Some(Result<ServedResponse, ErrorCode>)`: `ServedResponse` is plain data (status, headers, body), not a handle. The response body reuses the `outgoing-body` buffer of H3 (`HTTP_OUT_STREAM`) with the same limit (`max_request_body`); a body over it reports `HTTP-response-body-size`. The outparam belongs to the embedder: a guest dropping it leaves it for `take_response` to free. Outparam ids are never reused while the host lives, so an outparam kept after `take_response` traps on `set` (unknown handle) instead of writing into another call's answer. A body the guest left unfinished (or dropped) fails the answer with `internal-error`: the response is read once `handle` returns, so **streaming a response after `set` and trailers are not supported** (`finish` with trailers is `internal-error("trailers are not supported")`). Tests: `wasm2rs-wasi/tests/http_incoming.rs` (host) and `tests/it/wasip2_typed_http_incoming.rs` (a component with `import types` / `export incoming-handler`). |

## Serving over TCP: `examples/wasip2_serve.rs`

`wasip2_serve <component.wasm> [--listen ADDR]` serves a component exporting
`wasi:http/incoming-handler` (#195: "listening/accept"). It builds the program like
`wasip2_run` (the build step is shared in `examples/common`) with the server of
`examples/wasip2_serve/server.rs` appended in place of the `wasi:cli/run` call, and then
becomes it (`exec` on unix, so killing the example kills the server).

- **Shape.** `std::net::TcpListener`, default `127.0.0.1:8080` (anything else only when asked).
  One connection is one request, served in turn; the answer always carries `Connection: close`.
  No keep-alive, no concurrency. The program prints `listening on <addr>` once bound (`:0`
  picks a port).
- **HTTP/1.1 lives in the example, not in `wasm2rs-wasi`**, with no new dependency: a request
  line (`HTTP/1.0` or `1.1`, origin-form target or `*`), headers, and a `Content-Length` body.
  `Host` becomes the authority, the scheme is `HTTP`. Malformed request line, header or
  `Content-Length`: 400. `Transfer-Encoding`: 501 (chunked is not read). Head over 64 KiB or
  body over 1 MiB: 413, decided from `Content-Length` before the body is read. A client that has not sent
  its whole request within 5 seconds (a deadline, not a per-read timeout): 408 (the server is serial, so one stalled client must not hold it forever).
- **Answer.** Read after `handle` returned (`take_response`; streaming is #195). The guest's
  `content-length`, `connection` and `transfer-encoding` headers are replaced by the server's;
  `HEAD`, `1xx`, `204` and `304` get no body, and `1xx` and `204` no `content-length`. No response set: 500. `Err(error-code)`: 502 with
  the code's name in the body.
- **Traps.** A guest panic is caught around the call: 500, the server stays up, and the
  `Instance` (with its host and the handles in it) is created anew, since a trap may leave
  it half-updated. After a normal answer the instance is kept, so guest state lasts across
  requests.
- **Tests.** `tests/it/wasip2_serve_example.rs` runs the example as a process and talks real TCP
  (echo of status/headers/body, 400, 413, 501, 500 after a trap and the next request, 500/502).

Of #195 this covers "listening/accept"; the outparam stale-handle guard, the status range and the pollable epoch are in T5a; streaming responses and trailers (T5b) remain.

`wasip2_wit_regression.rs` now requires every method to be overridden.

## Risks

- TCP-level behaviour (redirects, proxies, TLS) belongs to the client; the trait must not leak
  `ureq` types. TLS is only available through the feature.
- Thread lifetime: a dropped future or body must stop its thread (flag checked between reads).
- `Slab` does not detect stale *handles* (existing behaviour); a guest's double drop is its own trap
  as elsewhere. Pollables are different: a `Wait` keeps the slot's epoch (`SlotRef`, bumped on free), so
  a pollable of a dropped stream/future/socket is always ready and never reports on whatever reuses
  its slot.
