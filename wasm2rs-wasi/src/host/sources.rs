//! The injectable time and randomness sources of a [`WasiHost`](super::WasiHost).

use super::slab::trap;
use crate::wasi_clocks_wall_clock::Datetime;
use std::fmt;
use std::time::Instant;

/// The time the guest sees through `wasi:clocks`.
///
/// Only the values of `now` and the resolutions come from the clock. Waiting does not:
/// `subscribe-duration`, `subscribe-instant` and every `poll` wait in real time, so a fake
/// clock that is advanced by hand does not release a pollable. `subscribe-instant` waits for
/// `when - monotonic_now()` real nanoseconds (nothing if that is not positive).
pub trait Clock: Send {
    /// Nanoseconds on the monotonic clock, from an arbitrary but fixed origin.
    fn monotonic_now(&self) -> u64;

    /// The monotonic clock's resolution in nanoseconds (1 by default).
    fn monotonic_resolution(&self) -> u64 {
        1
    }

    /// The wall clock: the time since the Unix epoch.
    fn wall_now(&self) -> Datetime;

    /// The wall clock's resolution (1 ns by default).
    fn wall_resolution(&self) -> Datetime {
        Datetime {
            seconds: 0,
            nanoseconds: 1,
        }
    }
}

/// The default [`Clock`]: the system's. The monotonic clock counts from its creation.
#[derive(Debug, Clone, Copy)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn monotonic_now(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn wall_now(&self) -> Datetime {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_else(|e| {
                trap(format_args!(
                    "the system clock is before the Unix epoch: {e}"
                ))
            });
        Datetime {
            seconds: since.as_secs(),
            nanoseconds: since.subsec_nanos(),
        }
    }
}

/// A [`RandomSource`] could not produce bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomError(String);

impl RandomError {
    pub fn new(message: impl Into<String>) -> Self {
        RandomError(message.into())
    }
}

impl fmt::Display for RandomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RandomError {}

/// The randomness the guest sees through `wasi:random`.
pub trait RandomSource: Send {
    /// Fills `buf` for `wasi:random/random`, which must be cryptographically secure. If it
    /// cannot, return an error: the host traps the guest rather than hand out weak bytes.
    fn fill_random(&mut self, buf: &mut [u8]) -> Result<(), RandomError>;

    /// For `wasi:random/insecure`: fast and unpredictable enough for hash seeds, not for keys.
    fn insecure_u64(&mut self) -> u64;

    /// For `wasi:random/insecure-seed`.
    fn insecure_seed(&mut self) -> (u64, u64);
}

/// A small fast generator (splitmix64) for the `insecure` interfaces: fine for `HashMap` seeds
/// and examples, not for keys or tokens.
#[derive(Debug, Clone)]
pub struct InsecureRandom {
    state: u64,
}

impl InsecureRandom {
    /// A generator that always gives the same sequence for the same `seed`.
    pub fn new(seed: u64) -> Self {
        InsecureRandom { state: seed }
    }

    /// A generator seeded from the OS-seeded `std` hasher.
    pub fn from_os() -> Self {
        use std::hash::BuildHasher;
        Self::new(std::collections::hash_map::RandomState::new().hash_one(0u8))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// The default [`RandomSource`] (feature `os-random`): the OS's CSPRNG through `getrandom`
/// for `wasi:random/random`, and [`InsecureRandom`] for the rest.
#[cfg(feature = "os-random")]
#[derive(Debug, Clone)]
pub struct OsRandom {
    insecure: InsecureRandom,
}

#[cfg(feature = "os-random")]
impl OsRandom {
    pub fn new() -> Self {
        OsRandom {
            insecure: InsecureRandom::from_os(),
        }
    }
}

#[cfg(feature = "os-random")]
impl Default for OsRandom {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "os-random")]
impl RandomSource for OsRandom {
    fn fill_random(&mut self, buf: &mut [u8]) -> Result<(), RandomError> {
        getrandom::fill(buf).map_err(|e| RandomError::new(e.to_string()))
    }

    fn insecure_u64(&mut self) -> u64 {
        self.insecure.next_u64()
    }

    fn insecure_seed(&mut self) -> (u64, u64) {
        (self.insecure.next_u64(), self.insecure.next_u64())
    }
}

/// A request an [`HttpClient`] executes for `wasi:http/outgoing-handler.handle`.
///
/// The body is buffered: the host executes the request once the guest has finished writing it
/// (`outgoing-body.finish`). The trailers the guest finished the body with are in `trailers`
/// (empty for none); a client that cannot send them must refuse the request, not drop them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// The method, as the guest wrote it (`GET`, `POST`, or an extension method).
    pub method: String,
    /// `scheme://authority/path-with-query`, completed with the defaults of the host.
    pub url: String,
    /// Every header value on its own entry, in the guest's order.
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
    /// Every trailer value on its own entry, in the guest's order; empty when the guest sent none.
    pub trailers: Vec<(String, Vec<u8>)>,
    /// The `request-options` timeouts, in nanoseconds (`None`: the client's default).
    pub connect_timeout: Option<u64>,
    pub first_byte_timeout: Option<u64>,
    pub between_bytes_timeout: Option<u64>,
}

/// What an [`HttpClient`] got back. The body is read on a thread of the host, so the client may
/// stream it.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Box<dyn std::io::Read + Send>,
    /// Reads the trailers once `body` has been read to its end (`None`: the response has none).
    /// The host calls it at most once, on the thread that reads the body (which reads ahead of
    /// the guest), after the body ended without error. An empty result means no trailers. The guest sees the end of the body only after it returns, so it must not block for ever.
    pub trailers: Option<ResponseTrailers>,
}

/// See [`HttpResponse::trailers`].
pub type ResponseTrailers = Box<dyn FnOnce() -> Vec<(String, Vec<u8>)> + Send>;

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("trailers", &self.trailers.is_some())
            .finish_non_exhaustive()
    }
}

/// An [`HttpClient`] could not get a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpClientError(String);

impl HttpClientError {
    pub fn new(message: impl Into<String>) -> Self {
        HttpClientError(message.into())
    }
}

impl fmt::Display for HttpClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpClientError {}

/// Executes the requests of `wasi:http/outgoing-handler`, set with
/// [`WasiHostBuilder::http_client`](super::WasiHostBuilder::http_client). Without one, `handle`
/// fails with `internal-error`.
pub trait HttpClient: Send {
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError>;
}
