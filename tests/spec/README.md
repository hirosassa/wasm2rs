# WebAssembly spec testsuite (vendored subset)

`*.wast` files are copied unmodified from <https://github.com/WebAssembly/testsuite>
at commit `63908677d00585fdab4c4eccbf01f781d3a454d5` (Apache-2.0, see `LICENSE`).
They are run by `tests/it/spec_wast.rs`, which reports every directive it cannot
replay (imports, `register`, `assert_invalid`, multi-value, ...) instead of
dropping it silently.

To add a file: copy it here and add one entry to the `spec_test!` list in
`tests/it/spec_wast.rs`.
