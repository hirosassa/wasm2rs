# wasm2rs-wasi

The typed WASI 0.2.0 host for programs transpiled by [wasm2rs](https://github.com/hirosassa/wasm2rs):
one fixed `Host` trait for the whole 0.2.0 surface, whose methods trap unless a host implements
them, and the types it uses (`wasi_filesystem_types::ErrorCode`, ...).

`src/generated.rs` is generated from the official WIT in `tests/wasi_p2_wit/` and checked in;
a test regenerates it and fails on any difference. To refresh it after a generator change, run
`WASM2RS_BLESS=1 cargo test --test it wasi_generated`.
