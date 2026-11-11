# `wasi:http` host (0.2.0)

Status: implemented. H1 to H5, the T5 follow-ups (streaming, incoming-side trailers) and T6 (client-side
trailers) are done: `WasiHost` overrides all 63 `wasi_http_*` methods of
`wasm2rs-wasi/src/generated.rs` (none is left at the trapping default).
Background: [wasm2rs-wasi-design.md](wasm2rs-wasi-design.md) (the host library),
[component-exports-design.md](component-exports-design.md) (the export side that `incoming-handler`
needs).

## At a glance

The typed WASI preview2 host is the `wasm2rs-wasi` crate (`WasiHost` and the `Host` trait for the whole
WASI 0.2.0 surface; `examples/wasip2_run.rs` runs a component with it). Its general design (streams, the
TCP send queue, error reporting, limitations) is in [typed-host-design.md](typed-host-design.md). Every
method of `wasi:http` 0.2.0 is implemented:

- **Outgoing side:** `fields`, `request-options`, `outgoing-request` with its `outgoing-body` (buffered,
  1 MiB by default, `max_request_body`; request trailers go to `HttpRequest::trailers`),
  `outgoing-handler.handle`, the receive side (`future-incoming-response`, `incoming-response`,
  `incoming-body`) and `future-trailers`, on top of an `HttpClient` set on the builder.
- **`UreqClient`**, an `ureq` client behind the `http-client` feature: no redirects, `ureq`'s default
  TLS, no trailers either way (a request with trailers is refused).
- **Incoming side** (a guest exporting `incoming-handler#handle`: `incoming-request`,
  `outgoing-response`, `response-outparam`), driven by the embedder through
  `WasiHost::new_incoming_request`, `new_response_outparam` and `take_response`. `ServedResponse::body`
  is a `ServedBody`, an `io::Read` that follows the guest's `outgoing-body`, so a body written after
  `response-outparam.set` or after `handle` returns is streamed. Trailers: `IncomingRequestParts::trailers`
  reach `future-trailers.get`; a response body's `finish` trailers come back from `ServedBody::trailers`.
- **Serving over TCP:** `cargo run --release --example wasip2_serve -- component.wasm --listen 127.0.0.1:8080`
  (details in [Serving over TCP](#serving-over-tcp-exampleswasip2_servers)).

## Starting point

- Every resource handle is a `u32` newtype (`Fields`, `OutgoingRequest`, `IncomingBody`, ...); own and
  borrow are the same type. No associated types.
- `WasiHost` keeps each resource kind in a `Slab<T>`; streams share one `u32` space split by range
  (`0..3` stdio, files from `FIRST_FILE_STREAM`, TCP from `TCP_STREAM = 1<<30`), dispatched in
  `read_stream` / `write_stream`. Pollables are `Wait` values (`Copy`, slot indexes into host tables)
  waited in `wait_any`; stdin is fed by a thread through shared state.
- Pluggable parts (`Clock`, `RandomSource`) are boxed trait objects set through the builder.
- (At the start) no HTTP dependencies; the only one now is the optional `ureq` behind the `http-client` feature. `tests/it/wasip2_wit_regression.rs` requires every method, `wasi:http`
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
   version (H5) had no trailers and no streaming response; both were added later (see "Streaming and trailers").
   `incoming-handler` is not in the generated trait (it is an export), so no trait change.
7. **Limits.** Header count/size, body buffer size and request timeouts (`request-options`
   connect/first-byte/between-bytes) are enforced by the host; a violation maps to the matching
   `ErrorCode` (`HTTP-request-header-size`, `connection-timeout`, ...).

## Phases (one PR each, in order)

| Phase | Content |
|---|---|
| H1 ✅ | `fields`, `request-options`, `outgoing-request` (not its `body`, H3), drops; the `HttpClient` trait and builder method (no real client yet; the trait's shape may still change in H2). |
| H2 ✅ | `outgoing-handler.handle`, `future-incoming-response` (thread, `Wait`), `incoming-response`, `incoming-body` (receive side), `http-error-code`. The `HttpClient` signature is unchanged; the host shares the client between request threads behind a mutex (`send` runs one at a time, bodies are read outside it). A client error is `internal-error` with its message; a policy refusal is `HTTP-request-denied`. `incoming-body.finish` returns a `future-trailers` that can only be dropped (H3 adds its methods). |
| H3 ✅ | `outgoing-request.body`, `outgoing-body.write` / `finish`, `future-trailers.subscribe` / `get`. The body is a buffer behind an output stream (`HTTP_OUT_STREAM`, range above the input streams): `check-write` is the room left under the limit (`max_request_body`, 1 MiB) up to 4096, a write past the limit fails the stream (`last-operation-failed` carrying `HTTP-request-body-size`, then `closed`) and `finish` / the future report the same code. `handle` may come before or after the body is written: its thread waits for `finish` and only then calls the client; a body dropped without `finish` (or finished with invalid trailers) fails the future with `internal-error` and the client never runs. Trailers were not supported in H3 (refused at `finish`; `future-trailers.get` was always `none`); T6 added them to the trait, see "Client-side trailers". `UreqClient` refuses a non-empty body on `HEAD`/`CONNECT` (`ureq`'s `run` would send it); like every client error it reaches the guest as `internal-error`. |
| H4 ✅ | `ureq` client behind feature `http-client`; end-to-end tests against a loopback server. |
| H5 ✅ | incoming: `incoming-request` (method, path-with-query, scheme, authority, headers, `consume`), `outgoing-response` (status `100..=999` (wasmtime's range), default `200`; headers immutable once built; `body` once), `response-outparam.set`, and their drops. The embedder mints `new_incoming_request(IncomingRequestParts)` and `new_response_outparam()`, calls `Exports::export_wasi_http_incoming_handler_0_2_0_handle` (no change to the glue or `src/wit` was needed: `own<..>` resource parameters of an export already work) and then `take_response(outparam)`, which returns `None` when the guest never set one, else `Some(Result<ServedResponse, ErrorCode>)`: `ServedResponse` is plain data (status, headers, body), not a handle. The response body reuses the `outgoing-body` buffer of H3 (`HTTP_OUT_STREAM`) with the same limit (`max_request_body`); a body over it reports `HTTP-response-body-size`. The outparam belongs to the embedder: a guest dropping it leaves it for `take_response` to free. Outparam ids are never reused while the host lives, so an outparam kept after `take_response` traps on `set` (unknown handle) instead of writing into another call's answer. A body that already failed (dropped before `finish`, over the limit) fails the answer; an open one is streamed (T5b, below). Tests: `wasm2rs-wasi/tests/http_incoming.rs` (host) and `tests/it/wasip2_typed_http_incoming.rs` (a component with `import types` / `export incoming-handler`). |

## Streaming and trailers (T5b, #195)

- **`ServedResponse.body` is a `ServedBody`** (breaking: it was `Vec<u8>`): `io::Read + Send`, no borrow
  of the host, over the same buffer as the guest's `outgoing-body`. `read` waits while the body is
  open and empty, `Ok(0)` once the guest finished it, an error once it failed (dropped unfinished, over
  the limit, or the host dropped: dropping the slab fails open bodies). `set_idle_timeout` bounds the
  wait (`ErrorKind::TimedOut`). `finished_len()` tells a server the length when the body is already
  finished, so it can send a `content-length`.
- **`take_response` no longer fails on an open body**; only a body that failed already. What is new is
  that the answer can be read *while the guest writes*: after `set` inside `handle`, or after `handle`
  returned when another guest thread keeps writing. With a single-threaded guest nothing can write
  after `handle` returns, so an open body there is a guest that forgot to finish: use the idle timeout.
- **The limit counts unread bytes** (`max_request_body`): a body larger than it passes through a
  reader that keeps up; one written faster than read still fails with `HTTP-response-body-size`.
- **Trailers, the incoming side.** `IncomingRequestParts.trailers` (breaking: new field) is handed
  to `future-trailers.get` of the request's `incoming-body` (immutable fields; `none` when `None`).
  A response body may `finish` with trailers: `ServedBody::trailers()` returns them once finished.

## Client-side trailers (T6, #209)

The `HttpClient` trait carries trailers both ways (breaking: new fields, no default but empty/`None`).

- **Sending.** `HttpRequest.trailers` (`Vec<(String, Vec<u8>)>`, empty for none) holds what the guest
  finished the request body with. `outgoing-body.finish` checks them as a header the guest sets is
  (forbidden names, token syntax): a `fields.clone` of a received message is mutable and may hold names
  a guest could not `append`, so the check is repeated at `finish`. A refused set fails the request
  with `internal-error` and the client never runs. A client that cannot send trailers must refuse the
  request, not drop them.
- **Receiving.** `HttpResponse.trailers` is an `Option<ResponseTrailers>`, a `FnOnce() -> Vec<(String,
  Vec<u8>)>` the body-reader thread calls once, after the body ended without error (so a client can
  read the chunked trailer section from the connection it streamed the body from). The result is kept
  with the body; `incoming-body.finish` hands it to `future-trailers`, whose `get` answers
  `some(ok(some(fields)))` (immutable fields), or `none` for no reader, an empty result, or a body that
  failed or was finished before it ended. A panicking reader fails the body read like a client error; a reader that never returns holds back the end of the body for the guest (the host has no timeout on it). Trailers of a served response body are checked the same way at `finish`.
- **`UreqClient`** supports neither: a request with trailers is refused with an `HttpClientError`
  (`internal-error` for the guest) and a response never has a trailers reader (`ureq` exposes none). A
  client that really sends and receives trailers (hyper-based, ...) is future work.

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
- **Answer.** `take_response` after `handle` returned. A body the guest finished (without trailers)
  is sent with a `content-length`; otherwise it is sent chunked as the guest writes it, with the
  trailers after the last chunk, and a body that fails or stalls for `--body-timeout SECS` (default 30)
  is cut without the closing chunk, so the client sees it truncated. The guest's
  `content-length`, `connection` and `transfer-encoding` headers are replaced by the server's;
  `HEAD`, `1xx`, `204` and `304` get no body, and `1xx` and `204` no `content-length`. No response set: 500. `Err(error-code)`: 502 with
  the code's name in the body.
- **Traps.** A guest panic is caught around the call: 500, the server stays up, and the
  `Instance` (with its host and the handles in it) is created anew, since a trap may leave
  it half-updated. After a normal answer the instance is kept, so guest state lasts across
  requests.
- **Tests.** `tests/it/wasip2_serve_example.rs` runs the example as a process and talks real TCP
  (echo of status/headers/body, 400, 413, 501, 500 after a trap and the next request, 500/502).

Of #195 this covers "listening/accept" (#196), the outparam stale-handle guard, the status range and the pollable epoch (T5a), and streaming responses and the incoming-side trailers (T5b). Client-side trailers are T6 (#209), above.

`wasip2_wit_regression.rs` now requires every method to be overridden.

## Risks

- TCP-level behaviour (redirects, proxies, TLS) belongs to the client; the trait must not leak
  `ureq` types. TLS is only available through the feature.
- Thread lifetime: a dropped future or body must stop its thread (flag checked between reads).
- `Slab` does not detect stale *handles* (existing behaviour); a guest's double drop is its own trap
  as elsewhere. Pollables are different: a `Wait` keeps the slot's epoch (`SlotRef`, bumped on free), so
  a pollable of a dropped stream/future/socket is always ready and never reports on whatever reuses
  its slot.
