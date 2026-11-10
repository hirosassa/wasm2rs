# wasm2rs-wasi

The typed WASI 0.2.0 host for programs transpiled by [wasm2rs](https://github.com/hirosassa/wasm2rs):
one fixed `Host` trait for the whole 0.2.0 surface, whose methods trap unless a host implements
them, and the types it uses (`wasi_filesystem_types::ErrorCode`, ...).

`src/generated.rs` is generated from the official WIT in `tests/wasi_p2_wit/` and checked in;
a test regenerates it and fails on any difference. To refresh it after a generator change, run
`WASM2RS_BLESS=1 cargo test --test it wasi_generated`.

## Coverage

`WasiHost` implements every method of WASI 0.2.0 (199, generated from the official WIT):

| Package | Methods | Notes |
|---|---|---|
| `wasi:cli` | 12 | arguments, environment, exit, stdio, terminal types |
| `wasi:clocks` | 6 | monotonic and wall clocks, replaceable with `.clock(..)` |
| `wasi:filesystem` | 32 | preopens only, confined below each preopen (Unix; see the limits below) |
| `wasi:http` | 63 | client (needs an `HttpClient`) and incoming-handler side, with trailers |
| `wasi:io` | 23 | streams, pollables, `poll` |
| `wasi:random` | 5 | OS CSPRNG by default (feature `os-random`) |
| `wasi:sockets` | 58 | TCP, UDP and name lookup, gated by `NetworkPolicy` |

Status, entry points and the known limitations of the whole WASI 0.2 support are in
[docs/wasi-p2-status.md](../docs/wasi-p2-status.md); the design is in
[docs/typed-host-design.md](../docs/typed-host-design.md) and
[docs/wasi-http-design.md](../docs/wasi-http-design.md). File confinement relies on `cap-std`
and is only tested on Unix.

## Configuring a host

`WasiHost::builder()` sets everything; `build()` returns a `ConfigError` for an unusable
configuration (a missing preopen directory, a duplicate guest name, no random source).

```rust,ignore
let host = WasiHost::builder()
    .arguments(vec!["prog".into(), "-v".into()])
    .environment(vec![("KEY".into(), "value".into())])
    .preopen("/", "./data", Access::ReadOnly)
    .preopen("/out", "./out", Access::ReadWrite)
    .network(NetworkPolicy::Loopback)
    .clock(my_clock)            // `Clock`: `now` and the resolutions; waits stay in real time
    .random(my_random)          // `RandomSource`: `wasi:random`
    .stdin(std::io::Cursor::new(b"input".to_vec()))
    .stdout(std::fs::File::create("out.txt")?)
    .stderr(std::io::sink())
    .build()?;
```

Unset stdin, stdout and stderr are the process's. A given reader is read on a thread that
starts when the guest first uses stdin; a given writer is flushed when the guest flushes, at
`wasi:cli/exit` and when the host is dropped. Without the `os-random` feature (on by default)
there is no default for `wasi:random/random`, so `random` is required.

## HTTP client (feature `http-client`)

`wasi:http` requests go through an `HttpClient` given with `.http_client(..)`. The feature
`http-client` (off by default, so no extra dependency) adds `UreqClient`, one over
[`ureq`](https://crates.io/crates/ureq); no `ureq` type is in the public API.

```rust,ignore
let host = WasiHost::builder().http_client(UreqClient::new()).build()?;
```

- Redirects are not followed: a 3xx reaches the guest. 4xx and 5xx are responses, not errors;
  only a request without a response (connection, lookup, timeout) is an `HttpClientError`.
- TLS follows `ureq`'s default: `https` uses rustls with the bundled webpki root certificates.
  Bodies are not decompressed (`ureq`'s `gzip` feature is off).
- `connect-timeout` and `first-byte-timeout` are applied as given. `ureq` cannot time out each
  read, so `between-bytes-timeout` bounds the whole body instead, which is stricter.
