//! The typed trap payload of [`WasiHost`](crate::WasiHost).

use std::fmt;

/// Why the host trapped the guest, for a host built with
/// [`WasiHostBuilder::typed_traps`](crate::WasiHostBuilder::typed_traps).
///
/// The generated `Host` methods have no error channel for these, so a trap unwinds. By
/// default it is a panic with a message string; with `typed_traps` it is
/// `std::panic::resume_unwind(Box::new(WasiTrap))`, which skips the panic hook (nothing
/// is printed) and is caught with `catch_unwind` and `payload.downcast_ref::<WasiTrap>()`.
/// [`Display`](fmt::Display) is the message the default panic carries.
///
/// A broken internal invariant, a poisoned lock, a configuration error found while
/// building, a guest contract violation (a length over a limit, an empty poll list) and
/// a system clock before the Unix epoch stay panics with a string payload.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WasiTrap {
    /// The guest used a handle that is not in the table: one it never got, or one it dropped.
    InvalidResourceHandle {
        /// The kind of resource the handle names (`"tcp-socket"`, `"http fields"`, ...).
        resource: &'static str,
    },
    /// A handle table grew past what a `u32` handle can name.
    HandleTableExhausted,
    /// `wasi:random` was called and the host has no [`RandomSource`](crate::RandomSource)
    /// (only a host made by `WasiHost::new` without the `os-random` feature).
    RandomSourceMissing,
    /// The host's [`RandomSource`](crate::RandomSource) could not produce bytes.
    RandomSourceFailed {
        /// The source's [`RandomError`](crate::RandomError), as text.
        message: String,
    },
}

impl fmt::Display for WasiTrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WasiTrap::InvalidResourceHandle { resource } => write_invalid_handle(f, resource),
            WasiTrap::HandleTableExhausted => f.write_str("handle table exhausted"),
            WasiTrap::RandomSourceMissing => {
                f.write_str("wasi:random: no random source (enable `os-random` or use the builder)")
            }
            WasiTrap::RandomSourceFailed { message } => {
                write!(f, "wasi:random: the random source failed: {message}")
            }
        }
    }
}

/// The untyped panic messages are kept byte for byte, so the three shapes stay.
fn write_invalid_handle(f: &mut fmt::Formatter<'_>, resource: &str) -> fmt::Result {
    if resource == "stream" {
        f.write_str("read from an unknown or dropped stream")
    } else if let Some(kind) = resource.strip_prefix("http ") {
        write!(f, "wasi:http: unknown {kind} handle")
    } else {
        write!(f, "unknown or dropped {resource}")
    }
}

impl std::error::Error for WasiTrap {}
