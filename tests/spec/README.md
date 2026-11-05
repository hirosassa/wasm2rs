# WebAssembly spec testsuite (vendored subset)

`*.wast` files are copied unmodified from <https://github.com/WebAssembly/testsuite>
at commit `63908677d00585fdab4c4eccbf01f781d3a454d5` (Apache-2.0, see `LICENSE`).
They are run by `tests/it/spec_wast.rs`, which reports every directive it cannot
replay (imports, `register`, `assert_invalid`, multi-value, ...) instead of
dropping it silently.

To add a file: copy it here and add one entry to the `spec_test!` list in
`tests/it/spec_wast.rs`.

The set also covers files from the merged proposals (bulk memory, memory64, multi-memory,
table64 variants of the `*64.wast` files). The harness skips what it cannot replay and
reports it: modules with imports, `register`, `assert_invalid`/`assert_malformed`, and
assertions with reference-typed arguments or results. Files that depend on those (for
example `table_get`, `table_grow`, `ref_is_null`) are not vendored yet.
