# `wasm2rs-wasi`: promoting the typed WASI 0.2 host to a library

Status: P0, P1 and P2 are implemented (the crate exists, with the trait, the host logic and the
configuration surface). The "Problem" section describes the state before P1.
Background: [typed-host-design.md](typed-host-design.md) describes the host as it is today
(it described the host as `examples/wasip2_host/typed.rs`; since P1 that code is `wasm2rs-wasi/src/host.rs`); [component-model-poc.md](component-model-poc.md)
describes how the component analysis came about.

## Problem

The `Host` trait and its types (`Pollable`, `Descriptor`, the `ErrorCode` enums, ...) are generated
per component by `--wit-host --glue`, so `typed.rs` cannot be a compiled module. It is spliced into
the generated program as source text:

- `keep_declared_methods` (`filter.rs`) drops, line by line, the `// optional:` blocks whose methods
  the component's trait does not declare (over 150 markers).
- `type_aliases` scrapes `-> Result<_, X>` out of the generated text to name `FsErrorCode` and
  `NetErrorCode`, because a component importing both interfaces gets `ErrorCode` and `ErrorCode2` in
  import order.
- `rustc` runs at run time. A component importing something `typed.rs` lacks fails with E0046 only
  there.

## Decisions

1. **A new workspace crate, `wasm2rs-wasi`**, with its own version. The transpiler stays free of
   runtime code and dependencies.
2. **Methods the host does not implement default to a trap** (documented), so one `impl Host` always
   compiles and an unsupported interface (today: all of `wasi:http`) fails at the call, not at build.
3. **The configuration API ships complete in the first release**: arguments, environment, several
   preopens, network policy, and injectable clocks, random and standard streams.

## Design

### One fixed `Host` trait, owned by the library

`wasm2rs-wasi` defines the `Host` trait for the whole WASI 0.2.0 surface (199 methods today) and its
types once. Types that the generator names by import order (`ErrorCode`, `ErrorCode2`) are instead
qualified by interface (`fs::ErrorCode`, `net::ErrorCode`); the generator gets a deterministic
naming rule for this.

The trait and types are **generated and checked in** (`wasm2rs-wasi/src/generated.rs`) from the
official 0.2.0 WIT already vendored in `tests/wasi_p2_wit/`. A freshness test regenerates them and
fails on any difference, replacing the allowlist-vs-`typed.rs` comparison of
`tests/it/wasip2_wit_regression.rs` (the allowlist shrinks to the methods that keep the trapping
default, i.e. `wasi:http`).

### Glue in an "external types" mode

`wit_host_glue` today emits the trait, its types and `Glue<H: Host>`. A new option makes it emit
only `Glue<H: wasm2rs_wasi::Host>` (the per-component part: lifting and lowering the flat core
parameters of the imports that component declares) and refer to the library's types by path. The
embedded mode stays for custom hosts. Glue still depends on the component, so it is generated per
component; it is just short.

### Use

```rust
let host = WasiHost::builder()
    .args(["prog", "--flag"])
    .env([("KEY", "value")])
    .preopen("/data", "./data", Access::ReadWrite)?
    .build()?;
let mut inst = Instance::new(Glue(host));
```

No `rustc` at run time: the transpile (plus glue) happens in `build.rs` or, later, in an
`include_component!` macro (A3).

### Configuration surface

| Area | API |
|---|---|
| Arguments, environment, initial cwd | builder setters (replace the process values) |
| Preopens | several, each with a guest name, a host path and read-only / read-write; `Result` instead of today's `canonicalize().expect` |
| Network | policy: none / loopback only / external (today's `with_external_network`) |
| Clocks | `Clock` trait for wall and monotonic time, with resolutions; default = system |
| Random | `RandomSource` trait for `random` and `insecure`/`insecure-seed`; the default must be a CSPRNG for `wasi:random/random` (today's splitmix64 is not) |
| stdin / stdout / stderr | `Read + Send` / `Write + Send` objects or inherit the process streams. The stdin reader runs on a thread started at the guest's first use; stdout and stderr are flushed by the guest's flush, at `wasi:cli/exit` and on drop |

Errors in configuration are `Result`s. A guest-triggered trap stays a panic with the same message
contract as today (the generated code turns it into a trap).

### What moves

`typed.rs` splits in two: the `impl Host` (about 1,550 lines, 136 methods)
needs the generated types; the rest (`Slab`, `Wait`, `SharedStdin`, `FileStream`, `TcpOut`, path
resolution with the preopen escape rules, socket-address helpers, clocks) does not and becomes
ordinary library modules. The `// optional:` markers, `keep_declared_methods` and `type_aliases`
are deleted.

## Phases

| Phase | Content |
|---|---|
| P0 | Generator: external-types glue mode; interface-qualified type names. Tests against the existing hand-written WIT subsets. **Done**: `WitOptions { naming, external_types }` with `wit_host_trait_with_options` / `wit_host_glue_with_options`, and `--wit-host --glue --external-types=<path>`; see the note below. |
| P1 | Create `wasm2rs-wasi`: generate and check in the trait; move the host logic; add the freshness test; switch the test helpers (`build_program`) and `wasip2_run` to the library. |
| P2 | Configuration surface above, including the injectable clocks, random and streams. |

P0 as built: with `WitNaming::Qualified` each type is in a `pub mod` named like the method-name
prefix of its defining interface (`wasi_filesystem_types::ErrorCode`; the decoder records the
definer on every record, variant, enum, flags and resource), with its WIT name and no counter. The
type identity key includes the definer (a type shared by several interfaces has one definer, so it is
declared once; two interfaces that each define the same name and shape are two types). Only if two
different interface names make the same module name (e.g. two versions of one interface) does the
later get a `_2` suffix. The default naming and its output are unchanged. The CLI writes only the
external glue (`--external-types`); the library's trait comes from the API.

Each phase is its own PR; P1 depends on P0, P2 on P1. Out of scope: the known limitations of
[typed-host-design.md](typed-host-design.md) (pollable slots never freed, stdin thread never joined,
TOCTOU preopen escape).

## Risks and constraints

- **Test cost.** About 24 wasip2/wit test files compile a generated program with `rustc` at test
  time; switching helpers to the library does not remove that cost.
- **Lints.** The hardening lints are set in the root `[lints]` only, so the new crate does not
  inherit them. The host uses `unwrap`/`expect`/`assert!` for traps; either keep that behind scoped
  `#[expect(..., reason = ...)]` or convert to `Result` internally.
- **Release.** The tag-driven `release.yaml` checks only the root version and publishes two crates
  by hard-coded steps. Adding the crate needs a workspace member entry, a `cargo publish -p
  wasm2rs-wasi` step after `wasm2rs`, and its own version.
- **Compatibility.** The library targets WASI 0.2.0; import names drop the version suffix
  (`named_imports`), so 0.2.x components using only the 0.2.0 surface work. A host trait for a
  different WASI version would be a different crate version.
- **`wasi:http`.** Needs the export side (`canon lift`, A2) first.
