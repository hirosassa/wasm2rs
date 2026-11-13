# WASI 0.2.x additions after 0.2.0

`wasm2rs-wasi`'s `Host` trait is generated from the official WASI **0.2.0** WIT
(`tests/wasi_p2_wit/`) plus a small overlay (`tests/wasi_p2_wit/overlay/`) for functions
added in a later 0.2.x that real toolchains import (#339). A component built against a newer
patch version imports its functions at that version (`wasi:cli/exit@0.2.12`); the glue matches
interfaces by name and compatible version, so only a function the 0.2.0 WIT lacks is a gap
(the glue calls a `Host` method that does not exist, so `rustc` fails with E0599).

This is the comparison of 0.2.0 with the newest tag, `v0.2.12` of
[WebAssembly/WASI](https://github.com/WebAssembly/WASI) (`proposals/*/wit`), made by listing every
`@since(version = 0.2.N)` with N ≥ 1 and every `@unstable` in the 0.2.12 WIT (2026-11-14). Every
other item of 0.2.12 is `@since(version = 0.2.0)`: the same surface as 0.2.0, with the package
version in its `use` paths changed.

| Package | Item | Kind | Since | Overlay | Imported by a surveyed toolchain? |
|---|---|---|---|---|---|
| `wasi:cli/exit` | `exit-with-code: func(status-code: u8)` | function | 0.2.12 (stable) | **yes** | **yes**: wasi-sdk C++ `std::cout` (survey K) |
| `wasi:http/types` | `field-name` (`type field-name = field-key`) | type alias | 0.2.1 (stable) | no | no method: an alias of `field-key`, no wire difference, nothing for `Host` |
| `wasi:http/types` | `response-outparam.send-informational` | function | `@unstable(informational-outbound-responses)` | no | not seen |
| `wasi:sockets/network` | `network-error-code: func(err: borrow<error>) -> option<error-code>` | function | `@unstable(network-error-code)` | no | not seen |
| `wasi:clocks/timezone` | whole interface: `display`, `utc-offset`, `timezone-display` | interface | `@unstable(clocks-timezone)` | no | not seen |

The "surveyed" column is the toolchain survey of `docs/component-toolchain-survey.md` (Rust,
TinyGo, wasi-sdk C and C++, JS, Python, `cargo-component`: hello and, where tried, `wasi:http`).
An `@unstable` item is only imported by a component built with that feature switched on, so none
is in the overlay. If one turns up, add it to the overlay (see `tests/wasi_p2_wit/README.md`)
with a `WasiHost` implementation; nothing else in the overlay procedure changes.
