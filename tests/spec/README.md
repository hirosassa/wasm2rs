# WebAssembly spec testsuite (vendored subset)

`*.wast` files are copied unmodified from <https://github.com/WebAssembly/testsuite>
at commit `63908677d00585fdab4c4eccbf01f781d3a454d5` (Apache-2.0, see `LICENSE`).
They are run by `tests/it/spec_wast.rs`, which reports every directive it cannot
replay (imports, `register`, multi-value, ...) instead of
dropping it silently. `assert_invalid` and binary `assert_malformed` modules are
replayed as "`transpile` must reject it" (text-format `assert_malformed` is rejected by
the wast parser itself and is skipped).

To add a file: copy it here and add one entry to the `spec_test!` list in
`tests/it/spec_wast.rs`.

The set also covers files from the merged proposals (bulk memory, memory64, multi-memory,
reference types, the `*64.wast` variants). The harness skips what it cannot replay and
reports it: modules importing anything but the standard `spectest` host (`print*`, the
`global_*` constants, a 10-slot `table`, a 1-page `memory`, all replayed with a small
host), `register` and the other modules' instances, multi-value results and GC reference types. A module that grows an imported
`spectest` table or memory is skipped (the host does not enforce their maximums). A
module-level `assert_trap (module ...)` is replayed as "instantiation traps" (a stateless
module has no instantiation, so it is skipped). If a state-changing call is skipped, the rest of
that module's assertions are skipped too, so a skipped call never causes a false failure.

Also vendored (C9): `float_exprs`, `float_literals`, `float_memory`, `int_exprs`, `local_init`, `func`,
`memory_size`, `table`, `ref_func`, `exports`, `imports`, `binary`, `binary-leb128`, `custom`, `comments`,
`unreached-invalid` and the function-references files `call_ref`, `return_call_ref`, `br_on_null`,
`br_on_non_null`.

Not vendored yet: `memory_grow64`, `type`, `token` and `linking` (nothing would run: only
`assert_malformed`/`assert_unlinkable`/`register`), `names` (the `wast` parser's confusing-unicode lint
rejects it), `tag` (the stack-switching proposal allows tag results, so its `assert_invalid`
"non-empty tag result type" is intentionally accepted), and the `simd_*`, `gc` and threads files. `return_call*`, `switch` and the exception-handling files
(`throw`, `throw_ref`, `try_table`, `unwind`) are vendored. `global`, `elem` and `data` are vendored
but replay only part of their assertions.

`try_table` replays only 2 assertions: its main modules need imports, so the rest are skipped and reported.
