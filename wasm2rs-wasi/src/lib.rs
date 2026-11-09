//! The typed WASI 0.2.0 host for programs transpiled by wasm2rs.
//!
//! [`Host`] declares every method of the WASI 0.2.0 surface; a method a host does not
//! override traps (panics, naming the method), so one implementation always compiles and an
//! unsupported interface (today `wasi:http`) fails at the call instead of at build time.

#[rustfmt::skip]
#[expect(
    clippy::result_unit_err,
    reason = "the signatures are the WIT's: `result<_>` without an error type is `Result<_, ()>`"
)]
mod generated;

pub use generated::*;

mod host;

pub use host::WasiHost;
