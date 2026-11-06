# WebAssembly spec testsuite (vendored subset)

`*.wast` files are copied unmodified from <https://github.com/WebAssembly/testsuite>
at commit `63908677d00585fdab4c4eccbf01f781d3a454d5` (Apache-2.0, see `LICENSE`).
They are run by `tests/it/spec_wast.rs`, which reports every directive it cannot
replay (imports, `register`, `assert_invalid`, multi-value, ...) instead of
dropping it silently.

To add a file: copy it here and add one entry to the `spec_test!` list in
`tests/it/spec_wast.rs`.

The set also covers files from the merged proposals (bulk memory, memory64, multi-memory,
reference types, the `*64.wast` variants). The harness skips what it cannot replay and
reports it: modules with imports other than `spectest.print*` (replayed with a no-op host), `register`, `assert_invalid`/`assert_malformed`, multi-value
results and GC reference types. If a state-changing call is skipped, the rest of that
module's assertions are skipped too, so a skipped call never causes a false failure.

Not vendored yet: `memory_grow64` and `memory_size` (`memory.grow` does not honor a
memory's declared maximum), and files whose modules mostly import from `spectest` or
need `register` (`global`, `table`, `ref_func`, `return_call*`, ...). `elem` and `data` are vendored but replay
only the modules without such imports, and only their `invoke`/`assert_return`/`assert_trap (invoke ...)`
directives: a module-level `assert_trap (module ...)` (an instantiation trap) is not replayed yet,
so the out-of-bounds segment checks are skipped and reported. `type` holds only `assert_malformed` and `names` is rejected by the `wast`
parser's confusing-unicode lint, so neither is vendored.

`try_table` replays only 2 assertions: its main modules need imports, so the rest are skipped and reported.
