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

## `overlay/`: later additions

A few functions were added to WASI after 0.2.0 and are imported by real toolchains at their
newer patch version. They are not in the 0.2.0 files above, which stay verbatim. `overlay/<package>/`
holds a file of the same name as the file it replaces, and `tests/it/wasi_generated.rs` lays each
overlay file over the 0.2.0 directory before generating:

| file | source | difference from 0.2.0 |
|---|---|---|
| `overlay/cli/exit.wit` | https://github.com/WebAssembly/WASI `v0.2.12` (`proposals/cli/wit/exit.wit`) | adds `exit-with-code: func(status-code: u8)` (`@since(version = 0.2.12)`) |

`docs/wasi-post-0.2.0-additions.md` lists every difference between 0.2.0 and 0.2.12 and why only
this one is here. To add one: copy the file from the tag, drop the `@since`/`@unstable` annotations
and the package version changes (the file stays in the `0.2.0` package), keep the doc comment, add
the row above, run `WASM2RS_BLESS=1 cargo test --test it wasi_generated`, check that the
`generated.rs` diff is only the new method, implement it in `wasm2rs-wasi/src/host/impl_host.rs`
and update the method count in `tests/it/wasip2_wit_regression.rs`.

`wasm2rs-wasi/src/generated.rs` is generated from these files; see `tests/it/wasi_generated.rs`
and `tests/it/wasip2_wit_regression.rs`.
