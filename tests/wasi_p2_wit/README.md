# Official WASI 0.2.0 WIT (test fixture)

Verbatim copies of the `wit/` directories of the WASI preview2 proposals, tag `v0.2.0`
(each without its vendored `deps/`, `deps.toml` and `deps.lock`; the packages are pushed
together by `tests/it/wasi_generated.rs`):

| directory     | source                                               |
|---------------|------------------------------------------------------|
| `io/`         | https://github.com/WebAssembly/wasi-io `v0.2.0`         |
| `clocks/`     | https://github.com/WebAssembly/wasi-clocks `v0.2.0`     |
| `random/`     | https://github.com/WebAssembly/wasi-random `v0.2.0`     |
| `filesystem/` | https://github.com/WebAssembly/wasi-filesystem `v0.2.0` |
| `sockets/`    | https://github.com/WebAssembly/wasi-sockets `v0.2.0`    |
| `cli/`        | https://github.com/WebAssembly/wasi-cli `v0.2.0`        |
| `http/`       | https://github.com/WebAssembly/wasi-http `v0.2.0`       |

Do not edit these files; refresh them from the tags above.

`wasm2rs-wasi/src/generated.rs` is generated from these files; see `tests/it/wasi_generated.rs`
and `tests/it/wasip2_wit_regression.rs`.
