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
| `src/codegen/` | The transpiler: IR, flattening (state machines), rendering, WASI / SIMD runtime |
| `src/codegen/func/` | Per-function lowering |
| `tests/it/` | Integration tests, bundled into a single binary (`tests/it/main.rs`) |
| `docs/` | Design and test-infrastructure notes |
| `examples/` | Usage examples |

## Before you open a pull request

Run the same checks as CI:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --lib --no-default-features   # the library must build without the CLI feature
cargo test --release
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
2. Bump `version` in `Cargo.toml`.
3. Tag the release; `.github/workflows/release.yaml` builds the prebuilt binaries.

## License

By contributing, you agree that your contributions are licensed under the
[MIT License](LICENSE).
