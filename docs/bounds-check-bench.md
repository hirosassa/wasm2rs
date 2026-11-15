# Bounds-check cost on memory-heavy workloads

`examples/bench_memory.rs` transpiles each workload in `benches/memory/` three ways —
default (bounds-checked), `--typed-traps` (explicit checks raising a typed trap, see
below) and `--unsafe-memory` (every check dropped, so it is the
upper bound on what removing checks could gain) — builds all with
`rustc -O` (`opt-level=3`), and times `run(n)` in-process. The median of 7 runs is
reported, with the two variants interleaved. All variants must print the same
checksum, and `tests/it/bounds_bench.rs` checks each checksum against a plain-Rust
reference.

```
cargo run --release --example bench_memory [runs]
```

| workload | what it stresses |
|---|---|
| `hash` | sequential byte loads (FNV over 64 KiB) |
| `matmul` | two i32 loads per inner step (64x64 product) |
| `random` | one load + one store at an LCG-chosen address |
| `divrem` | `div_u`/`rem_u`/`div_s`/`rem_s` by a data-dependent divisor |
| `atomics`, `atomics_shared` | atomic store/load/rmw/cmpxchg, on an unshared and on a shared memory (see the last section) |

## Pitfall: keep the memory length opaque

If the instance is built and driven in the same function, its memory length is a
compile-time constant, LLVM proves every access in bounds, and the safe and unsafe
builds compile to *identical* machine code (an earlier version of this benchmark
measured exactly that and wrongly reported "no cost"). The harness therefore calls
`run` through an `#[inline(never)]` function on a `black_box`ed instance, as a real
embedder that grows memory or loads it at runtime would. Checking the generated
assembly (`rustc -O --emit asm`): with the instance opaque the safe builds keep
their `slice_index` panic paths and extra compares (matmul inner loop: 10 `cmp` vs
4 in the unsafe build), without it neither build has any panic reference.

## Result (Apple M4 Max, rustc 1.99.1)

| workload | safe (ms) | unsafe (ms) | safe / unsafe |
|---|---|---|---|
| hash | 992.4 | 985.7 | 1.007 |
| matmul | 1068.3 | 721.0 | 1.482 |
| random | 866.7 | 856.5 | 1.012 |

## Reading it

- `matmul` pays about **48%** for its checks: two loads per inner step, each a
  bounds-checked 4-byte slice on an address computed from loop counters. This is
  the shape a loop-invariant / range analysis or base reuse could attack,
  since the addresses are affine in the induction variables.
- `hash` and `random` pay about 1%: the loop is bound by a multiply chain or by
  memory latency, and the well-predicted check branches are hidden behind it.
- For scale, `unsafe_memory` was about 17% faster end to end on googlesql. So
  checks matter on arithmetic-dense, load-heavy loops and barely at all on
  latency-bound ones; matmul is the upper end of that range, not a typical case.

Machine code is not vectorised in either matmul build (no `.4s` operations), so the
gap is the checks themselves, not a lost vectorisation.

## Survey of real modules (how much could static removal reach?)

A throwaway scan with `wasmparser` (not committed) counted, per memory access, whether
its address is a bare `local.get` base and whether an earlier access in the same
block frame used the same base with the base local not reassigned in between (so the
earlier check dominates). "Strictly covered" additionally requires the earlier
access's `[offset, offset+size)` to contain the later one's, so dropping the later
check cannot move a trap earlier. "Mergeable" only needs the same base and version,
so folding the checks into one range check could trap before an intervening store or
call: it ignores side effects and is an upper bound, not a safe count.

| module | memory ops | bare `local.get` base | mergeable (upper bound) | strictly covered (safe) |
|---|---|---|---|---|
| googlesql (40,525 funcs), all | 651,921 | 74.4% | 43.3% | **7.5%** |
| googlesql, inside loops | 151,531 | 75.7% | 37.2% | 5.4% |
| small Rust program (284 funcs), all | 3,030 | 58.8% | 22.3% | 2.9% |

About 23% of googlesql's memory accesses are inside loops.

## Conclusions

- Check removal that preserves trap order (reusing an earlier, covering check) can
  reach only about 7.5% of accesses on a large real module, 5% inside loops. Not
  worth a pass on its own.
- Merging checks could reach up to ~43% but needs a "no side effect in between"
  condition that was not measured, and changes where an out-of-bounds trap fires.
- The matmul gap (about 48%) comes from addresses computed from loop counters, which
  share no base local; only a range analysis over induction variables would
  remove those, and that needs an IR the transpiler does not have (high effort and
  risk).
- Decision: stop here. Users who trust their module can already use
  `--unsafe-memory`. Revisit only with a profile of a real workload showing where
  the checked build loses, and an x86_64 measurement.

## Fixed-size memory (`min == max`) prototype

Idea: when a defined memory declares `min == max`, keep it as
`Box<[u8; N]>`, so its length is a compile-time constant (and `memory.size` could be a
constant; `memory.grow` would return the current size for a delta of 0 and -1 otherwise).

Prototype: the generated code of each `benches/memory/` workload was edited by hand
(`Vec<u8>` field to `Box<[u8; 65536]>`, `mem`/`mem_mut` through `[..]`), built like the
other runs (`rustc -O`, `drive` not inlined), 7 interleaved runs, median, M4 Max.

| workload | safe (ms) | fixed-size (ms) | unsafe (ms) | fixed / safe |
|---|---|---|---|---|
| hash | 883 | 883 | 882 | 0.999 |
| matmul | 1007 | 682 | 683 | 0.677 |
| random | 767 | 758 | 753 | 0.988 |

Findings:

- matmul with a fixed-size memory matches `unsafe_memory`: its addresses derive from
  loop counters with small constant bounds, so once the length is a constant LLVM proves
  every access in range. hash and random compute addresses LLVM cannot bound, so the
  constant length changes nothing.
- The gain therefore needs addresses that are provably below the declared size. Real
  modules take addresses from pointers and heap data, so they will rarely qualify.
- Real modules seldom declare `min == max`: the googlesql module (557 pages) has no
  maximum, and Rust and growable Emscripten builds declare none or a larger one.
- A fixed-size memory also cannot be an imported or shared one, and the change touches
  the `memory.grow` helper (which now also enforces the declared maximum).

Decision: not implemented. Revisit with a profile of a real module that declares
`min == max` and spends its time in bounded-address loops.

## Typed traps for division and memory/table access (#391, stage 2)

`--typed-traps` replaces Rust's built-in panics, which carry no typed payload:
division and remainder call `i32_div_s`-style helpers (`#[inline(always)]`, zero
check, `MIN / -1` check for `div_s`, then `wrapping_*`), and every memory and table
access uses `get`/`get_mut(..).unwrap_or_else(|| trap_memory_oob())` (a `#[cold]
#[inline(never)]` function) instead of `[..]`; `copy_within` gets an explicit range
check first. With the option off nothing changes: the output is byte-identical (3,608
modules extracted from the spec suite, compared against the previous commit's binary).

Measured with the harness above (Apple M4 Max, rustc 1.99.1, 9 interleaved runs,
median; the machine was shared with other builds, so differences of about 1% are
noise):

| workload | safe (ms) | typed (ms) | unsafe (ms) | typed / safe |
|---|---|---|---|---|
| hash | 892.2 | 899.0 | 899.1 | 1.008 |
| matmul | 1028.3 | 965.5 | 695.5 | 0.939 |
| random | 792.9 | 797.7 | 767.2 | 1.006 |
| divrem | 162.6 | 161.3 | 161.7 | 0.992 |

Object size of the generated module compiled alone (`--crate-type=lib --emit=obj`,
`opt-level=3`):

| workload | safe (bytes) | typed (bytes) | unsafe (bytes) | typed / safe |
|---|---|---|---|---|
| hash | 2240 | 3368 | 1512 | 1.504 |
| matmul | 2536 | 3536 | 2272 | 1.394 |
| random | 2272 | 2968 | 1800 | 1.306 |
| divrem | 1264 | 2064 | 1264 | 1.633 |

- Speed: no measurable cost over the default checks. Rust's `/` and slice indexing
  already test the same conditions; the explicit form tests them with a cold call
  instead of a panic path, which LLVM lays out as well (matmul even came out 6%
  faster, likely layout). The typed build is therefore not slower than the default.
- Size: these modules are a few functions, so the object is dominated by a fixed cost
  of about 0.45 KB for `Wasm2RsTrap` (its `Display`/`Error` impls and
  `resume_unwind`; a module with only `unreachable` grows from 2008 to 2456 bytes in
  the same measurement) plus one cold trap function per kind (about 0.1 KB). For a
  large module the per-site cost is what counts: googlesql (40,525 functions) grows
  from 115,646,685 to 116,121,445 bytes of generated source (+0.41%), with
  `--unsafe-memory` 115,648,387 to 116,121,624. (The Rust build of googlesql was not
  repeated for this option.)
- `--unsafe-memory` stays consistent: linear-memory accesses keep the unchecked
  `get_unchecked` form (an out-of-bounds access remains undefined behaviour and no
  `trap_memory_oob` is emitted), while division and table accesses are still typed,
  since `unsafe_memory` never covered them.
- Not covered yet: a shared memory's accesses (they go through `SharedMemory::bytes`
  and still panic on the slice index); covered by #407, see the last section.

## Atomics: alignment check (#408) and typed shared memory (#407)

Two workloads, `atomics` (unshared) and `atomics_shared` (`(memory 1 1 shared)`), run
the same loop: per step an `i32.atomic.store`, `load`, `rmw.add` and `rmw.cmpxchg`
(always succeeding) on one word, the address walking a 64 KiB memory in 4-byte
strides. A mutable global that is always 0 is added to the address, so the access is
aligned at run time but LLVM cannot prove it. (An earlier version without it measured
no object-size change from #408 at all: the masked address was provably aligned, so the
check folded away.) `tests/it/bounds_bench.rs` checks each checksum against a
plain-Rust model (and that it is not a degenerate 0). The shared one is
single-threaded: it measures the code path (`SharedMemory`), not contention.

To measure what #408 and #407 cost, the same example was built at the commit before
each (`6e565d2`, the parent of #407, and `6d1bca7`, the parent of #408) and at
`93499ff`, with `BENCH_KEEP_DIR` keeping the binaries. All three print the same
checksum. 9 runs, interleaved across the three binaries, median (Apple M4 Max, rustc
1.99.1; shared machine, so about 1% is noise):

| workload | mode | before #407 (ms) | before #408 (ms) | now (ms) | now / before #408 |
|---|---|---|---|---|---|
| atomics | safe | 1140.6 | 1135.3 | 1145.9 | 1.009 |
| atomics | typed | 1128.9 | 1123.9 | 1129.1 | 1.005 |
| atomics | unsafe | 1093.1 | 1097.3 | 1104.9 | 1.007 |
| atomics_shared | safe | 1235.1 | 1236.7 | 1234.3 | 0.998 |
| atomics_shared | typed | 1225.0 | 1227.7 | 1224.7 | 0.998 |
| atomics_shared | unsafe | 1219.0 | 1216.7 | 1216.2 | 1.000 |

Object size of the generated module alone (bytes), before #407 / before #408 / now:

| workload | safe | typed | unsafe |
|---|---|---|---|
| atomics | 2416 / 2416 / 2920 | 3040 / 3040 / 3784 | 1520 / 1520 / 2640 |
| atomics_shared | 9968 / 9968 / 9968 | 9984 / 10400 / 10400 | 9992 / 9992 / 9992 |

- #408 (alignment trap on unshared atomics): within noise in every mode (+0.5% to +0.9%,
  under the ~1% noise floor). The check is one mask test per access, hidden behind the
  loop's multiply/add chain. It costs size, not time: +21% (safe), +24% (typed) and
  +74% (unsafe) object bytes on this four-access loop. `unsafe_memory` drops bounds
  checks but not this alignment trap, so its object grows most.
- #407 (typed shared-memory accesses): shared typed 1225.0 ms before and 1227.7 ms after (+0.2%), no
  measurable cost, for +4.2% object size (9984 to 10400 bytes), the cold trap
  functions. The safe and unsafe shared builds are untouched.
- `unsafe_memory` does nothing for a shared memory: every atomic goes through
  `SharedMemory`, so `atomics_shared` shows no consistent difference between the three
  modes (within about 1.5%).
- Shared atomics are slower than unshared ones (safe: 300M steps in 1.23 s against 900M
  in 1.15 s, about 4.1 ns against 1.27 ns per step, 3.2x). Not separated by this
  benchmark: the unshared side uses plain loads and stores, while the shared side does
  real SeqCst atomics (rmw as a compare-exchange loop) plus `SharedMemory`'s own
  per-access length, alignment and bounds checks.
- "now" is `93499ff`, which also contains #409, #411 and #417; none of them is expected
  to touch these modules' atomic paths, and the "now / before #408" column is read as
  the effect of #408.
