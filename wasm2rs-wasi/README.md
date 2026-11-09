# wasm2rs-wasi

The typed WASI 0.2.0 host for programs transpiled by [wasm2rs](https://github.com/hirosassa/wasm2rs):
one fixed `Host` trait for the whole 0.2.0 surface, whose methods trap unless a host implements
them, and the types it uses (`wasi_filesystem_types::ErrorCode`, ...).

`src/generated.rs` is generated from the official WIT in `tests/wasi_p2_wit/` and checked in;
a test regenerates it and fails on any difference. To refresh it after a generator change, run
`WASM2RS_BLESS=1 cargo test --test it wasi_generated`.

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
