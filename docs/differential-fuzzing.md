# Differential fuzzing against wasmtime

`fuzz-diff/` is a separate crate (its own workspace, so wasmtime is not a
dependency of wasm2rs or of its CI) that runs random modules through both
wasmtime and the Rust wasm2rs generates, and reports any difference.

```
cd fuzz-diff
cargo run --release -- [first_seed] [count]       # default: 100 seeds from 0
cargo run --release --example one -- file.wasm    # both reports for one module
wasm-tools shrink ./shrink.sh finding.wasm -o small.wasm   # minimize a finding
cargo test --release                              # harness tests
cargo test --release -- --ignored                 # known bugs (they fail until fixed)
```

## What is compared

`wasm-smith` builds a module from the seed (deterministic): MVP plus multi-value,
bulk memory, reference types, sign-extension and saturating conversions; no
imports, no SIMD/threads/exceptions/GC/memory64/tail calls. Every exported function
with numeric parameters and results is called twice with fixed arguments, then
memory 0 is hashed. Each side prints one line per call (`ok` plus the values, or
`trap`), `init-trap` if instantiation fails, and the memory hash. NaNs print as
`nan` (payloads are not deterministic across implementations).

wasmtime runs with fuel; a module that exhausts it or overflows the stack is
skipped. The Rust side is compiled with `rustc -C opt-level=0` (overflow checks on,
so an arithmetic panic shows up as a spurious trap) and killed after 20 s.

Outcomes: `match`, `skip`, `unsupported` (wasm2rs rejected the module),
`MISMATCH`, `BROKEN` (does not compile, crashes or hangs). Findings are written to
`fuzz-diff/findings/seed-N.{wasm,txt}`.

## First run (seeds 0–1700)

About 1500 modules compared equal. Findings:

1. **A trapping operation whose value is discarded by a function-level `br` is
   dropped**, so the trap never happens (`i32.const 1; i32.load; br 0` on an empty
   memory). Most mismatches (seeds 162, 282, 316, 1046, 1346, 1358) are this.
2. **`memory.grow` ignores the declared maximum**; it only enforces 65536 pages
   (seeds 796, 1080).
3. Two runs hang in wasm2rs after a first call that traps (seeds 973, 1010); not
   yet reduced, possibly related to 1.
4. `unsupported` (about 5%): active element/data segments used by
   `table.init`/`memory.init`/`*.drop` ("not passive"), and negative data offsets.
   Both are now supported (an active segment reads as empty after instantiation,
   and an `i32` offset is unsigned).

1 and 2 are reproduced by `#[ignore]`d tests in `fuzz-diff/src/lib.rs`; remove the
`ignore` when each is fixed.

## Feature profiles

`cargo run --release -- FIRST COUNT --profile NAME` widens the generator beyond the
base feature set. Findings of a non-base profile are saved as
`findings/<profile>-seed-N.{wasm,txt}`.

Re-run of every profile with seeds 1–300 after the exception-handling work (`try_table`,
`exnref`, `br_table` out of a `try`): no `unsupported` module in any profile, and the
only mismatch is the known declared-maximum one below.

| Profile | Adds | Seeds 0–299 |
| --- | --- | --- |
| `base` | the default set above | 279 match, 21 skip |
| `multi-memory` | up to 3 memories | 276 match, 24 skip |
| `multi-table` | up to 3 tables | 277 match, 23 skip |
| `memory64` | 64-bit memories and tables | 276 match, 23 skip, 1 mismatch |
| `simd` | SIMD | 281 match, 19 skip |
| `tail-call` | `return_call`, `return_call_indirect` | 271 match, 29 skip |
| `extended-const` | arithmetic in constant expressions | 278 match, 22 skip (was 36 unsupported, now supported) |
| `exceptions` | `try_table`, `throw`, `exnref` | 281 match, 19 skip, no mismatch (was 6 unsupported: `br_table` out of a `try`) |

Only memory 0 is hashed, so a bug confined to another memory shows up only through
a call's result or trap.

`memory64` findings:

- 81 modules were `unsupported` because the profile also turns on 64-bit tables.
  These are now supported (a defined `table i64`; an imported 64-bit table is not).
- The one mismatch (seed 258) is the declared-maximum bug above: `(memory 3 4)` grows
  past its maximum.

New-profile findings (seeds 0–299):

- `simd`: 93 modules were `unsupported` because of a `v128.const` global initializer.
  These are now supported and the profile has no findings.
- `extended-const`: 36 modules were `unsupported` for `i32`/`i64` `add`, `sub` or `mul` in
  a global initializer. These are now supported and the profile has no findings.
- `exceptions`: `try_table` (including `catch_ref` / `catch_all_ref`), `exnref` values,
  tables and globals, `ref.null exn`, `throw_ref`, and `funcref`/`externref`/`exnref`
  exception payloads are now supported, as is a `br_table` whose arms leave a `try`
  body. The profile has no `unsupported` modules left (seeds 1–300).
- `tail-call`: no findings.
