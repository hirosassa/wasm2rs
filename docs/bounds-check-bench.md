# Bounds-check cost on memory-heavy workloads

`examples/bench_memory.rs` transpiles each workload in `benches/memory/` twice —
default (bounds-checked) and `--unsafe-memory` (every check dropped, so it is the
upper bound on what removing checks could gain) — builds both with
`rustc -O` (`opt-level=3`), and times `run(n)` in-process. The median of 7 runs is
reported, with the two variants interleaved. Both variants must print the same
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
  the shape a loop-invariant / range analysis (roadmap 6b) or base reuse (6a) could
  attack, since the addresses are affine in the induction variables.
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
  share no base local; only a range analysis over induction variables (6b) would
  remove those, and that needs an IR the transpiler does not have (high effort and
  risk).
- Decision: stop here. Users who trust their module can already use
  `--unsafe-memory`. Revisit only with a profile of a real workload showing where
  the checked build loses, and an x86_64 measurement.
