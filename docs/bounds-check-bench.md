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

## Result (Apple M4 Max, rustc 1.99.1)

| workload | safe (ms) | unsafe (ms) | safe / unsafe |
|---|---|---|---|
| hash | 1002.2 | 1005.7 | 0.997 |
| matmul | 742.3 | 746.4 | 0.995 |
| random | 860.6 | 865.0 | 0.995 |

## Reading it

On these workloads the checks cost nothing measurable: the ratio is within noise
(±1%). In the generated code the address is a plain `u32`-derived index into a
`Vec<u8>`, and the checks are well-predicted branches that LLVM either hoists or
the core retires for free on this machine.

This is **not** the whole picture. An earlier end-to-end measurement on a large
real program (googlesql, ~40K functions) showed `unsafe_memory` about 17% faster,
so checks do matter once code is branchy and register-starved. Synthetic loops
over a fixed small memory are the best case for the checked variant, so they only
bound the *easy* part.

Consequences for the roadmap:

- Optimisations that remove checks in simple loops (basic-block base reuse,
  loop-invariant ranges) have little to gain on code like the above; do not
  expect them to explain the googlesql gap.
- The next useful measurement is on a real module (per-function profile of where
  the checked variant loses), not more synthetic loops. It is also worth running
  this benchmark on x86_64, where the result may differ.
