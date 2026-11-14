# `cargo_manifest` release profile: codegen-units × LTO (2026-11-14, wasm2rs 0.5.0)

Question (G3 of the improvement survey): does the generated `Cargo.toml`'s
`codegen-units = 1` + `lto = "thin"` (`cargo_manifest` in `src/lib.rs`) actually pay off on a
googlesql-class output, or is a cheaper profile just as good? **Result: no alternative clearly beats
the current profile, so `cargo_manifest` is unchanged (docs only; no product code changed).** The
one clear loss is `codegen-units = 16` without LTO (2.8x slower at run time).

Everything below is **[verified]** (measured here) unless tagged **[inferred]**.

## Setup

- Input: `googlesql.wasm` v0.4.0 (13.4 MB, 40 K functions, SHA-256 `0751f79c...`), transpiled by the
  current `wasm2rs` with the split layout (`wasm2rs googlesql.wasm out 200 0`, ~112 MB of Rust).
- Harness: the googlesql-rs `native` backend (a copy of the repo with its `[profile.release]` set to
  `opt-level = 3`, `strip = true`, unwinding panics — what `cargo_manifest` emits — and no
  `overflow-checks`). The profile is varied through `CARGO_PROFILE_RELEASE_CODEGEN_UNITS` / `CARGO_PROFILE_RELEASE_LTO`, so
  LTO spans the generated `guest` crate and the driver crate, as it does for a consumer.
- Workload: an example that calls `Module::parse_statement` 20,000 times over three queries (one
  trivial, two with joins/windows/aggregates), after a warm-up; run 3 times per build.
- Toolchain: `rustc 1.99.1`, aarch64 macOS, 16 cores, 64 GB, one build at a time.
  `RUSTFLAGS="-C debuginfo=0 -C link-arg=-fuse-ld=lld"` for every row (`debuginfo=0` avoids an LLVM
  DWARF-emission SIGBUS on this crate; lld is the macOS fast linker).

## Results

| codegen-units | lto | rustc wall | binary (stripped) | parse, us/call (3 runs) |
|---|---|---|---|---|
| 1 | thin (**current**) | 39m 27s | 51.6 MB | 54.9 / 55.3 / 55.8 |
| 1 | off | 35m 28s | 53.0 MB | 53.8 / 54.1 / 53.3 |
| 16 | thin | 40m 46s | 52.2 MB | 54.3 / 53.9 / 53.8 |
| 16 | off | 30m 53s | 61.2 MB | 153 / 164 / 154 |

(Run-to-run spread within one binary is about 1-2%.)

## Reading

- **Build time is not parallelised by `codegen-units`.** User time equals wall time in every row and
  peak RSS is 18.1 GB in all of them: the time is in the single-threaded front end, the same `rustc`
  cost as in [f4-rustc-time-spike.md](f4-rustc-time-spike.md). The only faster row (`16/off`, about
  4.5 min under `1/off`) is the one that loses 2.8x at run time; [inferred] it simply does less
  optimisation work.
- **The slow configuration is the one with neither mechanism.** `16 CGUs + no LTO` splits the
  generated `Instance` methods across codegen units without cross-unit inlining: 2.8x slower and 16%
  bigger than `1/off`. Either `codegen-units = 1` or thin LTO recovers all of it, and using both
  gains no speed: the three fast rows span 53.3-55.8 us (the current profile is the slowest by
  2-3%, a gap outside the 1-2% run-to-run spread but from one build per row, so it may be
  build-to-build code layout rather than a real effect).
- **Alternatives against the current profile**: `1/off` builds 10% faster, is 2-3% faster and
  2.7% bigger; `16/thin` is 2-3% faster, 1% bigger and builds 1 min slower. The speed edge is
  within what one build per row can show, and the size and build-time trade-offs cancel it, so
  neither is a clear win.
- **Decision**: leave `cargo_manifest` as is. A consumer who wants about 10% less build time can set
  `lto = "off"` with `codegen-units = 1` without losing speed; never use `codegen-units > 1` without LTO.

## Not measured

- One workload (googlesql `parse_statement`); a numeric-heavy or small module may rank the
  configurations differently, though small modules also build in seconds.
- `lto = "fat"`, `opt-level = 2/s`, `panic = "abort"` (kept off in `cargo_manifest` on purpose, see
  its doc comment).
- One build per row; the build-time column has no variance estimate, so the 10% difference is
  indicative only.
