# Fuzzing the HTTP/1.x server of `wasip2_serve`

`fuzz/` is a separate crate (its own workspace, like `fuzz-diff/`, so the main build and CI
never see it). It fuzzes the hand-written HTTP/1.x half of
[`examples/wasip2_serve/server.rs`](../examples/wasip2_serve/server.rs): the request line,
headers, `Content-Length` bodies, the 400/408/413/501 rejections, the time limits and the
answer writer (`content-length` or chunked framing, trailers, HTTP/1.0).

The server source is **not copied**: `fuzz/src/lib.rs` `include!`s the example's file, with
stand-ins for the `Instance`/`Glue`/`Exports` names the generated module would give it. The
reader and writer take a `TcpStream`, so the targets drive them over a real loopback socket;
a `ServedResponse` is built through the real `WasiHost` (nothing is mocked).

## Running

Needs nightly and `cargo install cargo-fuzz`.

```sh
cd fuzz
cargo +nightly fuzz run serve_request            -- -max_len=70000 -dict=serve.dict -max_total_time=600
cargo +nightly fuzz run serve_request_structured -- -max_len=70000 -dict=serve.dict -max_total_time=600
cargo +nightly fuzz run serve_response           -- -max_len=70000 -dict=serve.dict -max_total_time=600
```

`-max_len=70000` is wanted: the head limit is 64 KiB. A crash lands in
`fuzz/artifacts/<target>/`; minimize it with `cargo +nightly fuzz tmin <target> <file>` and
pin it with a fixed input in `fuzz/tests/boundaries.rs`. The corpus in `fuzz/corpus/` is
committed (seeds for `serve_request` are the boundary cases; the other two take structured
bytes); minimize after a long run with `cargo +nightly fuzz cmin <target>`.

The deterministic checks (limits, rejection codes, the time limits that a fuzz run cannot
reach because its peer always closes) are plain tests, runnable on stable:

```sh
cd fuzz && cargo test
```

## Targets and what they check

| Target | Input | Oracle |
|---|---|---|
| `serve_request` | raw bytes | no panic/hang/oversized allocation; the verdict (accept, or 400/413/501) and every accepted part equal those of a reference model (`fuzz/src/model.rs`) written from the rules, not from the server's code |
| `serve_request_structured` | method, target, headers, body, ... | a request built only from valid parts is accepted with exactly the parts it was built from; one with a corrupted byte must still agree with the model |
| `serve_response` | status, headers, body, trailers, HEAD/1.0 flags | the bytes parse as strict HTTP/1.1 (no stray CR/LF, so no header injection); the guest's `Content-Length`/`Connection`/`Transfer-Encoding` are replaced by the server's; exactly one framing; the body and trailers read back as written; `head` on raw entries never panics and, for entries the host accepts, gives exactly one head |

One latitude is deliberate: the head limit (64 KiB) is checked after each read of up to
4 KiB, so a head ending within one read of the limit may be taken or refused (413) depending
on how the socket split the bytes; the model allows both there and requires 413 beyond it.

`head` writes header values as given: it relies on `wasm2rs-wasi` refusing CR/LF/control
bytes in a field value (`is_header_value`). `fuzz/tests/boundaries.rs` pins that contract.

## CI

CI only runs `cargo check --bins` and `cargo test` in `fuzz/` (stable, the `fuzz-crate` job;
the tests take about a second). Running the fuzzers (nightly + libFuzzer, minutes) is manual.
