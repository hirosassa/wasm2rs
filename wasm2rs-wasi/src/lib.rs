//! The typed WASI 0.2.0 host for programs transpiled by wasm2rs.
//!
//! [`Host`] declares every method of the WASI 0.2.0 surface; a method a host does not
//! override traps (panics, naming the method), so one implementation always compiles and an
//! unsupported interface fails at the call instead of at build time.

#[rustfmt::skip]
#[expect(
    clippy::result_unit_err,
    reason = "the signatures are the WIT's: `result<_>` without an error type is `Result<_, ()>`"
)]
mod generated;

pub use generated::*;

mod host;

// Declared here, not in `host.rs`, so that the client stays a file of its own.
#[cfg(feature = "http-client")]
#[path = "host/ureq_client.rs"]
mod ureq_client;
#[cfg(feature = "http-client")]
pub use ureq_client::UreqClient;

#[cfg(feature = "os-random")]
pub use host::OsRandom;
pub use host::{
    Access, Clock, ConfigError, HttpClient, HttpClientError, HttpRequest, HttpResponse,
    IncomingRequestParts, InsecureRandom, NetworkPolicy, RandomError, RandomSource, ServedResponse,
    SystemClock, WasiHost, WasiHostBuilder,
};
