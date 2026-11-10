# Contributing to wasm2rs

Thanks for your interest in wasm2rs! Bug reports, feature requests, docs fixes and
pull requests are all welcome.

## Reporting issues

- **Bugs**: include the wasm2rs version (`wasm2rs --version`), the command line, and
  ideally a minimal `.wat` / `.wasm` input that reproduces the problem. For a
  miscompile, say what the original wasm does (e.g. under wasmtime) versus what the
  generated Rust does.
- **Unsupported features**: wasm2rs rejects unsupported constructs with an explicit
  error rather than generating wrong code. If you hit one, please report the
  proposal/operator and a sample module.

## Development setup

Requirements:

- Rust (stable; CI also builds on beta and nightly), edition 2024
- `rustfmt` and `clippy` components
- Optional, for the end-to-end tests: the `wasm32-wasip1-threads` target, wasi-sdk,
  emsdk. See [docs/wasi-threads-e2e.md](docs/wasi-threads-e2e.md). Tests that need a
  missing tool skip locally (CI makes them mandatory).

```bash
git clone https://github.com/hirosassa/wasm2rs
cd wasm2rs
cargo build
```

## Project layout

| Path | Contents |
|------|----------|
| `src/main.rs` | CLI (behind the default `cli` feature, which pulls in mimalloc) |
| `src/lib.rs` | Public library API (`transpile`, `TranspileOptions`, ...) |
| `src/build.rs` | Build-script helpers (`transpile_to_out_dir`, `transpile_component_to_out_dir`) |
| `src/component.rs`, `src/wit.rs`, `src/wit_abi.rs` | Component Model input (`transpile_component`) and the WIT-derived host trait / glue / `Exports` trait |
| `src/codegen/` | The transpiler: IR, flattening (state machines), rendering, WASI / SIMD runtime |
| `src/codegen/func/` | Per-function lowering |
| `wasm2rs-wasi/` | Typed WASI 0.2 host library (`Host` trait, `WasiHost`), a separate crate with its own version |
| `wasm2rs-macros/` | Proc macros `include_wasm!` / `include_component!`, a separate crate |
| `tests/it/` | Integration tests, bundled into a single binary (`tests/it/main.rs`) |
| `docs/` | Design and test-infrastructure notes |
| `examples/` | Usage examples (`wasip2_run`, `wasip2_serve`, ...) |
| `fuzz-diff/` | Differential fuzzer against wasmtime, a separate crate (see [docs/differential-fuzzing.md](docs/differential-fuzzing.md)) |
| `fuzz/` | libFuzzer targets for the HTTP/1.x server of `examples/wasip2_serve`, a separate crate (see [docs/fuzzing-http-server.md](docs/fuzzing-http-server.md)) |

## Before you open a pull request

Run the same checks as CI:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --lib --no-default-features   # the library must build without the CLI feature
cargo test --release --workspace
```

Run a subset of the integration tests by module name:

```bash
cargo test --release --test it <name>
```

## Coding guidelines

- **Zero warnings.** `[lints]` in `Cargo.toml` denies warnings and a set of
  panic-prone / lossy constructs. New production code should avoid `unwrap`,
  `expect`, `panic!`, unchecked indexing and lossy `as` casts; propagate errors
  with `Result` instead.
- **No `unsafe` in the transpiler.** `unsafe_code` is denied. (Generated code may use
  `unsafe` only in the opt-in `--unsafe-memory` mode.)
- **Suppressions** need a reason: prefer `#[expect(..., reason = "...")]` at the
  narrowest scope. Test code may use a scoped `#[allow(..., reason = "test code")]`.
- Keep generated-code changes deliberate: output size and compile time of the
  generated Rust matter, so mention them in the PR when a change affects them.

## Tests

We develop test-first: write a small test that captures the behavior, watch it fail,
make it pass, then refactor.

- Prefer **real end-to-end checks** (transpile a `.wat`, compile the generated Rust
  with `rustc`, run it, assert on the result) over mocks. Only mock true external
  dependencies.
- Keep test data **inside the test function**. Avoid shared fixtures, except for data
  irrelevant to the assertion.
- New wasm features/operators should come with positive tests *and*, where the
  feature is intentionally unsupported, a test that pins the explicit error.
- Add the test to the matching module under `tests/it/` (or add a module and register
  it in `tests/it/main.rs`).

### Spec testsuite

`tests/spec/` holds a vendored subset of the official
[WebAssembly spec testsuite](https://github.com/WebAssembly/testsuite) (`.wast`),
replayed by `tests/it/spec_wast.rs`: each `module` and the `assert_return` /
`assert_trap` directives after it become one generated program. Directives the
harness cannot express yet (imports, `register`, `assert_invalid`, multi-value,
v128/ref values, ...) are **skipped with a reason**, never dropped; run
`cargo test --release --test it spec_wast -- --nocapture` to see the counts.

- To add a file, copy it into `tests/spec/` (see its `README.md` for the pinned
  upstream commit) and add it to `spec_test!`.
- A file whose module the transpiler rejects goes in `spec_known_gap!` with the
  expected error. That test fails once the gap is fixed, so the entry gets promoted
  to `spec_test!`.

## Pull requests

1. Fork and create a topic branch from the latest `main`
   (`git fetch origin && git rebase origin/main`). Resolve conflicts and re-run the
   checks above before pushing.
2. Keep PRs focused; one logical change per PR.
3. Describe **what** changed and **why**, and link related issues.
4. Add an entry under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md)
   ([Keep a Changelog](https://keepachangelog.com/en/1.1.0/) format) for user-visible
   changes. Update README/docs if behavior or options change.
5. CI must be green (fmt, clippy, tests on stable/beta/nightly, and the wasi-sdk /
   emsdk E2E jobs).

Commit messages: a short imperative summary, optionally with a `feat:` / `fix:` /
`perf:` / `docs:` prefix as used in the history.

## Releasing (maintainers)

1. Move `[Unreleased]` entries in `CHANGELOG.md` under the new version heading.
2. Bump `version` in `Cargo.toml` and in `wasm2rs-macros/Cargo.toml` (its own version and its `wasm2rs` dependency), then refresh `Cargo.lock`. `wasm2rs-wasi` has its own version in `wasm2rs-wasi/Cargo.toml`; bump it only when that crate changed.
3. Tag the release (`vX.Y.Z`, matching the `wasm2rs` version); `.github/workflows/release.yaml` builds the prebuilt binaries and publishes `wasm2rs`, then `wasm2rs-macros`, then `wasm2rs-wasi` (skipped if that version is already on crates.io).

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
