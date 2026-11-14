# Component toolchain survey (2026-11-12, wasm2rs 0.4.0)

Goal: find out which real-world `wasm32-wasip2` components `--component` handles today, so the
follow-ups (#184, #185, #186 and new ones) are ordered by what users hit first.

Method: one `wasi:cli/run` "hello" per toolchain, run with `examples/wasip2_run` (release build,
Apple M-series, `transpile_component` + `rustc -C opt-level=1`). Fixtures were built in a scratch
directory; the commands are below so the survey can be repeated.

## Results

| # | Toolchain | Component | Result |
|---|---|---|---|
| A | `rustc --target wasm32-wasip2` (std) | 2.7 MB, one core module | **works** (about 16 s with the `wasm2rs-wasi` build), args passed through |
| B | `wasm32-wasip1` core + `wasm-tools component new --adapt` (command adapter) | 2.2 MB, main + adapter module | **works** (L3 linking; run in 7.0 s with the `wasm2rs-wasi` build, rustc 1.6 s): prints `hello from rust`. The first run failed only in `wasip2_run`, which looked for `wasi:cli/run@0.2.0` while the adapter exports `@0.2.3` |
| C | Go (`GOOS=wasip1`) + the same adapter | 2.5 MB, main + adapter module | **runs when the environment is empty; fails with an environment, on wasmtime too** (#311): transpiles and builds (about 3 min in total; rustc 168 s, 4.3 GB peak RSS at `-C opt-level=1`). With an inherited environment it traps with `assertion failed at adapter line 2745` (the adapter's `State` magic) because the adapter's two extra pages (36 -> 38) overlap Go's heap range (`bloc` 36 pages, `blocMax` 38 pages). The same failure reproduces on wasmtime v30.0.0 with `-S inherit-env`; `wasmtime run` passes an empty environment by default, and `env -i` makes the wasm2rs program print `hello from go`. Not a wasm2rs defect: a Go + adapter incompatibility |
| D | `jco componentize` (StarlingMonkey, JS) | 13.9 MB | **works at `-C opt-level=0`** (2026-11-13, after F3 and F4-1): prints `hello from js`; `wasip2_run` end to end took **88 s** (rustc 73 s, 7.6 GB peak RSS; the binary is 45 MB and starts in 0.8 s). At `-C opt-level=1`, the `wasip2_run` default, LLVM did **not finish in 80 minutes** (single codegen unit, 125 MB `program.rs`; stuck in IR passes / the machine scheduler of one huge function, and `-C llvm-args=-enable-misched=false` did not help in 40 minutes). `WASM2RS_EXAMPLE_OPT_LEVEL=0` selects the level for the example |
| E | `componentize-py` (CPython 3.14) | 17.6 MB, 14 core modules (12 without the shim and fixup) | **works** (F2-1 to F2-4, 2026-11-13): transpiles in 1.1 s to a 122 MB source, builds and runs `wasi:cli/run` printing `hello from python`; `wasip2_run` end to end took **337 s** (`rustc -C opt-level=1`, 7.5 GB peak RSS) after F4-1 removed the redundant parentheses. `run` works without calling `exports#init` first (the build-time init transform already ran). See [f2-dynamic-linking-design.md](f2-dynamic-linking-design.md), section 9 |
| F | Rust `wasm32-wasip2` + `wasi` 0.14 crate, `wasi:http` handler | 114 KB, one core module | **works** (2026-11-14): `wasip2_serve` answers `200`, a header, a body naming the request path and a trailer (`curl -i` shows it after the last chunk); build and start 8 s with the `wasm2rs-wasi` build. It exports `incoming-handler@0.2.12`, which `wasip2_serve` refused until it accepted any patch version (see G1). `#[ignore]` test: `WASM2RS_HTTP_HELLO_COMPONENT` |
| G | `jco componentize` (StarlingMonkey, JS), `fetch` event | 13.0 MB | **works at `-C opt-level=0`** (2026-11-14): `200`, header and body over `wasip2_serve`; 86 s from start to listening. `-C opt-level=1` is the known F4 problem (#331) |
| H | `componentize-py`, `IncomingHandler` | 18.4 MB | **works at `-C opt-level=0`** (2026-11-14): `200`, header and body over `wasip2_serve`, also for a second request; 75 s from start to listening |
| I | TinyGo 0.44 `tinygo build -target=wasip2` (Go `fmt.Println`) | 773 KB, one core module, `wasi:cli/run@0.2.0` | **works** (2026-11-14): prints `hello from tinygo`, exit 0; `wasip2_run` end to end 24 s (rustc at `-C opt-level=1`, 480 MB peak RSS). No wall. `#[ignore]` test: `WASM2RS_TINYGO_HELLO_COMPONENT` |
| J | wasi-sdk 34 `clang --target=wasm32-wasip2`, C `puts` | 227 KB, one core module, `wasi:cli/run@0.2.12` | **works** (2026-11-14): prints `hello from wasi-sdk C`, exit 0; 5.3 s end to end with a warm `wasm2rs-wasi` build (290 MB peak RSS). `#[ignore]` test: `WASM2RS_WASISDK_C_HELLO_COMPONENT` |
| K | wasi-sdk 34 `clang++ --target=wasm32-wasip2 -fno-exceptions`, `std::cout` | 3.2 MB | **works** (2026-11-14, after #339): prints `hello from wasi-sdk C++`, exit 0; 10 s end to end with a warm `wasm2rs-wasi` build. The program imports `wasi:cli/exit@0.2.12`'s `exit-with-code`; before the overlay added it to the 0.2.0 `Host` trait, `rustc` failed with E0599 (`no method named wasi_cli_exit_exit_with_code`). `#[ignore]` test: `WASM2RS_WASISDK_CPP_HELLO_COMPONENT`. Without `-fno-exceptions` the link fails in `wasm-ld` (`__cxa_throw`; wasi-sdk has no C++ exceptions), not a wasm2rs matter. See F6 (#339) |
| L | `cargo component build` 0.21.1, custom world (`export ops` with a record, a string and integers; no `wasi:cli/run`) | 72 KB, `wasm32-wasip1` + adapter (main + adapter module, as B) | **works** (2026-11-14): `wasip2_run` does not apply (no `run`), so a small program calls the typed `Exports` (`test_calc_ops_add(40, 2)` = 42, `greet`, `shift` on a record); 3 s for the test with a warm library build (430 MB peak RSS). `#[ignore]` test: `WASM2RS_CARGO_COMPONENT_CALC` |
| M | MoonBit `moon 0.1.20261104` (`--target wasm`) + `wit-bindgen moonbit` 0.63.0 + `wasm-tools component embed`/`new`, the `calc` world as L | 4.2 KB (3.6 KB without `greet`), one core module, no WASI imports | **works, including strings** (2026-11-14; #344 fixed): `add`, `greet` (with a non-BMP emoji, kanji and an empty string) and `shift` transpile, build and return `42`, `hello, wasm2rs`, `hello, 日本語😀`, `hello, ` and `11 2` through `Exports`. Before the fix, `greet` stopped the transpile with `Unsupported("export ... uses a string encoding other than utf8")`. Pinned by the ignored test `a_real_moonbit_component_passes_utf16_strings_through_the_typed_exports` (`WASM2RS_MOONBIT_CALC`). wasmtime v30.0.0's `wasmtime run --invoke` does not support components, so the expected values come from the sources |
| N | Zig 0.15.2 (`mise`), `wasm32-wasi` + the command adapter (hello); `wasm32-freestanding` `export fn add` + `component embed` | 36 KB (hello, main + adapter module); 313 bytes (`add`) | **works** (2026-11-14): `wasip2_run` prints `hello from zig` as `wasmtime run` does, exit 0, 6.2 s end to end with a warm `wasm2rs-wasi` build (290 MB peak RSS). The `add` component has a root-level function export (no interface); `Exports::add(40, 2)` = 42. Zig 0.15 has no `wasm32-wasip2` target, so every WASI program goes through the adapter, as B |
| O | AssemblyScript 0.28.20 (`npm`) + `@assemblyscript/wasi-shim`, `--runtime stub`, + the command adapter (hello); `export function add` + `component embed` | 24 KB (hello, main + adapter module); 318 bytes (`add`) | **works** (2026-11-14): `wasip2_run` prints `hello from assemblyscript` as `wasmtime run` does, exit 0, 5.9 s end to end with a warm `wasm2rs-wasi` build (290 MB peak RSS); `Exports::add(40, 2)` = 42. Also preview1-only, so through the adapter |

## Findings

### F1. Adapter-based components pick the adapter as the program module (B, C)

`wasm-tools component new --adapt` links two core modules: the user's program (`__main_module__`)
and the WASI adapter. The adapter imports `env.memory` and `__main_module__._start`, and it is the
module that exports `wasi:cli/run#run`. `transpile_component` chooses the module by the lifted
exports (see [component-input-design.md](component-input-design.md), decision 4), so it picks the
adapter, which imports its memory, and `reentrant_imports` rejects that.

This is the shape of every component made from a preview1 toolchain (older `cargo-component`,
Go, wasi-sdk C/C++ through the adapter). It is the most likely thing a user hits.

(Fixed: B and C now transpile by linking the core modules, see the table and
[core-module-linking-design.md](core-module-linking-design.md).) Ways out, cheapest first:
1. A clear error that names the cause ("the module that lifts the exports imports its memory from
   another core module; linking several core modules is not supported") instead of the options
   error. Tiny, and worth doing regardless.
2. Link the core modules inside one `Instance`: the adapter's imports of the main module's
   exports become direct calls, and its memory import becomes the main module's memory. Needs a
   design for a module that is both a program and an importer (cross-module imports of
   functions, memories, tables and globals). Medium to large; also unblocks #185.

### F2. Dynamically linked components (E)

`componentize-py` output is built from `libpython`, `libc`, runtime and binding libraries, each a
core module, joined through shared memory and tables (`wit-component` dylib linking). The first
guard hit is `exports#init`, whose memory comes from another instance. Beyond that guard this
needs F1's linking and a lot more (shared table, indirect calls across modules, `dlopen`-style
init order). Treat as out of scope until F1 is done; the error message is already explicit.

Update (2026-11-13, after L1-L5): investigated in [f2-dynamic-linking-design.md](f2-dynamic-linking-design.md).
The linking side is small (shared memory and table, constant `__table_base`, no start functions); the
blocker was F4; with F4-1 (#321) and the F2-1 to F2-4 link stages (#322, #324, #326, #327) it builds and runs (case E, 337 s).

### F3. A function imported twice makes the glue incomplete (D)

(Fixed: D now builds, see the table.) The JS component's program module imports the same WIT function twice (for example
`wasi:cli/environment@0.2.12#get-arguments`; why it is imported twice is not yet investigated).
`named_imports` gives the second one the name `..._81`; the generated `Imports` trait has
both methods, but the `Glue` impl only emits the first, so `rustc` fails with E0046 "missing
`wasi_cli_environment_get_arguments_81`, `wasi_io_poll_poll_85`, ..." (25 methods).

This looks like a real bug in `wit_host_glue`: the glue should implement every `Imports` method,
delegating duplicates to the same `Host` method. It is independent of F1 and small.

The same component also imports `wasi:*@0.2.12` and `wasi:http/types@0.2.10`: patch versions newer
than 0.2.0 are accepted (the first glue method names show `@0.2.12`), so they are not a problem
by themselves.

### F4. `rustc` time on large components (D)

The 13.9 MB JS component produced a 2 M-line `program.rs`; `rustc -C opt-level=1` took about 10
minutes (user 270 s, sys 332 s: the system time points at memory pressure). Not a correctness
issue, but it makes large guests impractical. Worth a separate measurement (`--split`, lower
opt-level, `-Z time-passes`) once F3 lets D build at all.

Measured in [f4-rustc-time-spike.md](f4-rustc-time-spike.md): the time is a quadratic `rustc` early-lint
cost on redundant parentheses in one huge file (587 s -> 61 s on the JS `program.rs` without them), not a large function and not LLVM.

After F3 and F4-1 the remaining cost for D is in **LLVM at `-C opt-level=1`**, which the metadata-only
F4 measurements above did not cover: more than 80 minutes without finishing on the JS `program.rs`
(the Python component, 122 MB, takes 337 s). `opt-level=0` takes 73 s. Cause not isolated (the
three 40 K-line flattened state machines are the suspects); follow-up in a separate issue.

### G1. `wasip2_serve` refused handlers exported at a newer patch version (F)

Real toolchains export `wasi:http/incoming-handler` at the version of their WASI bindings (the
`wasi` 0.14 crate: `@0.2.12`), not `@0.2.0`. `examples/wasip2_serve.rs` matched the interface at
any patch version but then rejected anything but `@0.2.0`, and `server.rs` named the generated
`export_wasi_http_incoming_handler_0_2_0_handle` method. Fixed: the method name now takes the
version of the interface the component exports. This was the only wall for F, G and H.

jco and componentize-py need the WIT of the version they target: `jco componentize` falls back to
an old componentize-js (which has no `wasi:http` export) when given WASI WIT older than 0.2.10,
and `wasm-tools component wit --out-dir` output from a component is not accepted by jco. Use the
`proposals/*/wit` directories of the `v0.2.10` tag of `WebAssembly/WASI` (below).

### F5. Things that worked and need no work

- `wasm32-wasip2` std output, including arguments (A).
- Newer patch versions of the WASI 0.2 interfaces (D's imports at 0.2.10 / 0.2.12).

### F6. `exit-with-code` was outside the generated `Host` trait (K, fixed by #339)

wasi-sdk's libc++ (`std::cout`) pulls in `wasi:cli/exit@0.2.12`'s `exit-with-code: func(status-code: u8)`,
a function added after 0.2.0 (it is not in `tests/wasi_p2_wit`, the fixture `wasm2rs-wasi`'s
generated `Host` is made from). The glue calls `Host::wasi_cli_exit_exit_with_code`, which does not exist,
so the build failed with E0599. Rust, TinyGo and wasi-sdk C do not import it. Fixed with an overlay on the
0.2.0 WIT (`tests/wasi_p2_wit/overlay/cli/exit.wit`, the generated trait gains this one method, `WasiHost`
exits with `status-code`); `docs/wasi-post-0.2.0-additions.md` lists the other 0.2.1 to 0.2.12 differences
(all `@unstable` or a type alias, none imported by a surveyed toolchain).

### F7. UTF-16 components (M, #344 and #351, fixed)

MoonBit (`String` is UTF-16) lifts strings as `string-encoding=utf16`, which `transpile_component`
used to reject, so a MoonBit component with a string in an export signature did not transpile.
The glue now transcodes at the boundary for a lifted export: a `String` passed to the guest is
written as code units into memory from the guest's `cabi_realloc` (aligned to 2, the length counts code
units), and a string the guest returns is read from `(ptr, len)` with `String::from_utf16`. An
unpaired surrogate or an odd pointer traps (a panic, like invalid UTF-8 on the utf8 path); nothing is
replaced silently. `compact-utf16` (latin1+utf16) stays `Unsupported`, with a message that says so.
The import side (#351) works the same way per function: a function lowered with `utf16` has the
strings a guest passes to it read as UTF-16 (`String::from_utf16`; an unpaired surrogate or an odd
pointer traps) and the strings the host returns written as code units from the lowering's `realloc`,
so a UTF-16 guest that imports a function with a string (for example MoonBit using WASI paths)
transpiles. A function lowered with `compact-utf16`, or with both `utf8` and `utf16` (the glue has
one method per function), whose signature holds a string is `Unsupported`, naming it, instead of being
misread.
Pinned on a real MoonBit component (2026-11-14, `moon 0.1.20261104`): `world app { import host; export run: func(name: string) -> string; }`
with `host { shout: func(s: string) -> string; }` and `run(name) = "got " + shout(name)` (both
`canon lower` and `canon lift` say `string-encoding=utf16`). With a host that upper-cases and appends `!`,
`run` returns `got WASM2RS!`, `got 日本語😀X!` and `got !` through `Exports`. The ignored test is
`a_real_moonbit_component_passes_utf16_strings_through_a_host_import` (`WASM2RS_MOONBIT_IMPORT`); build the
component as in item M of the reproduction, with that WIT and the one-line body above.

### F8. Notes on the new toolchains (M, N, O)

- MoonBit is not in the `mise` registry; the official installer rewrites the shell profile, so the
  tarballs it downloads were unpacked into a scratch `MOON_HOME` instead (commands below). `wit-bindgen moonbit` 0.63.0
  generates `declare pub fn` stubs that the generated project layout cannot implement (the
  implementing package must be a virtual package, which needs a `pkg.mbti` and abstract types), so
  the functions were written into the generated package. A toolchain quirk, not a wasm2rs matter.
- Zig and AssemblyScript emit `wasm32-wasi` core modules only, so a WASI program is B's shape (main
  module + command adapter); no new wasm2rs path was involved.

## Suggested order

1. F3 (glue emits every duplicate import). Small, a real bug, and it tells us whether D runs.
2. F1 step 1 (clear error for adapter-based input), then F1 step 2 (module linking) as the main
   design item. #185 (exports from several modules) should be designed together with it.
3. #184 and #186 as small follow-ups (no survey case hit them): both done with F1 step 2 (L3, L4).
4. F4 measurement after F3.
5. F2 only after F1 step 2: done as an investigation; F4 first for the end-to-end goal, see the F2 design.

Not covered yet: `wasi:http` guests built with TinyGo, wasi-sdk or `cargo-component`; Kotlin/Wasm (WasmGC output, no component tooling) and SwiftWasm. Add a row to the
table when one is tried.

## Reproduction

```bash
# A
rustc --target wasm32-wasip2 -O hello.rs -o rust-hello.wasm
# B (adapter = wasi_snapshot_preview1.command.wasm from the wasmtime v30.0.0 release)
rustc --target wasm32-wasip1 -O hello.rs -o p1.core.wasm
wasm-tools component new p1.core.wasm --adapt wasi_snapshot_preview1=adapter.wasm -o p1-hello.wasm
# C
GOOS=wasip1 GOARCH=wasm go build -o go.core.wasm go-hello.go   # go 1.27
wasm-tools component new go.core.wasm --adapt wasi_snapshot_preview1=adapter.wasm -o go-hello.wasm
# D (wit/ = tests/wasi_p2_wit as wit/deps, world: include wasi:cli/imports; export wasi:cli/run)
npx @bytecodealliance/jco componentize app.js --wit wit --world-name w -o js-hello.wasm
# E
uvx componentize-py -d wit -w w componentize app -o py-hello.wasm   # componentize-py 0.25.1
# I (tinygo 0.44, wasm-tools in PATH)
tinygo build -target=wasip2 -o tg-hello.wasm .
# J, K (wasi-sdk 34; K needs -fno-exceptions to link)
$WASI_SDK/bin/clang --target=wasm32-wasip2 -O2 hello.c -o c-hello.wasm
$WASI_SDK/bin/clang++ --target=wasm32-wasip2 -O2 -fno-exceptions hello.cpp -o cpp-hello.wasm
# L (cargo install cargo-component --locked; wit/world.wit: interface ops { record point { x: s32, y: s32 }
#   add: func(a: s32, b: s32) -> s32; greet: func(name: string) -> string; shift: func(p: point, dx: s32) -> point; }
#   world calc { export ops; }; impl: add = a + b, greet = "hello, {name}", shift = point with x + dx, same y)
cargo component new --lib calc --namespace test && (cd calc && cargo component build --release)
# from the wasm2rs checkout (an absolute path: tests run in the package root)
WASM2RS_CARGO_COMPONENT_CALC="$SCRATCH/calc/target/wasm32-wasip1/release/calc.wasm" cargo test --test it component_real_custom_world -- --ignored
# M (moon: curl -fsSL https://cli.moonbitlang.com/binaries/latest/moonbit-darwin-aarch64.tar.gz | tar xz -C $MOON_HOME,
#   cores/core-latest.tar.gz into $MOON_HOME/lib, PATH=$MOON_HOME/bin:$PATH, then `moon -C $MOON_HOME/lib/core bundle --warn-list -a --all`
#   (and `--target wasm-gc --quiet`), as the installer does; cargo install wit-bindgen-cli --locked)
wit-bindgen moonbit wit --out-dir mb --derive-show   # world calc as L; in gen/.../ops/top.mbt replace the `declare pub fn` stubs with bodies and reset
#   gen/.../ops/moon.pkg to `warnings = "-44"` (details in #344)
(cd mb && moon build --target wasm --release)
wasm-tools component embed wit mb/_build/wasm/release/build/gen/gen.wasm --world calc --encoding utf16 -o emb.wasm
wasm-tools component new emb.wasm -o mb-calc.wasm   # without strings: drop greet from the WIT, re-run wit-bindgen, rewrite the bodies, then moon build and embed again
# N (mise use zig@0.15.2; add.wit: package test:addonly; world w { export add: func(a: s32, b: s32) -> s32; })
zig build-exe hello.zig -target wasm32-wasi -O ReleaseSmall -femit-bin=zig.core.wasm
wasm-tools component new zig.core.wasm --adapt wasi_snapshot_preview1=adapter.wasm -o zig-hello.wasm
zig build-exe add.zig -target wasm32-freestanding -O ReleaseSmall -rdynamic -fno-entry -femit-bin=add.core.wasm
wasm-tools component embed add.wit add.core.wasm --world w -o add.emb.wasm && wasm-tools component new add.emb.wasm -o zig-add.wasm
# O (npm i assemblyscript @assemblyscript/wasi-shim; asconfig.json: {"extends": "./node_modules/@assemblyscript/wasi-shim/asconfig.json"})
npx asc assembly/hello.ts --config asconfig.json -o as.core.wasm -O3 --runtime stub
wasm-tools component new as.core.wasm --adapt wasi_snapshot_preview1=adapter.wasm -o as-hello.wasm
npx asc assembly/add.ts -o add.core.wasm -O3 --runtime stub   # then embed/new as for Zig's add
# M, N, O `add`: a component without imports has no `Glue`: `Instance::new()` takes no host, then `Exports::add(&mut inst, 40, 2)`
# run
cargo run --release --example wasip2_run -- <component>.wasm

# F (handler = `wasi::http::proxy::export!`; `wasi = "0.14"`, crate-type cdylib)
cargo build --release --target wasm32-wasip2
# G, H (wit/: proposals/{cli,clocks,filesystem,http,io,random,sockets}/wit of the v0.2.10 tag of
# WebAssembly/WASI as wit/deps/<name>/, plus world.wit: `world w { include wasi:http/proxy@0.2.10; }`;
# G uses `w`, H the same file with the world renamed to `proxy`, as `-w` below)
npx @bytecodealliance/jco componentize app.js --wit wit --world-name w -o js-handler.wasm
uvx componentize-py -d wit -w proxy bindings . && uvx componentize-py -d wit -w proxy componentize app -o py-handler.wasm
# serve, then curl -i http://127.0.0.1:8080/foo?x=1  (WASM2RS_EXAMPLE_OPT_LEVEL=0 for G and H)
cargo run --release --example wasip2_serve -- <handler>.wasm --listen 127.0.0.1:8080
```
