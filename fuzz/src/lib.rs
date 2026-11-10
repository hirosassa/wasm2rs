//! Fuzzing harness for the HTTP/1.x half of `examples/wasip2_serve`.
//!
//! The server is *not* copied: `examples/wasip2_serve/server.rs` is `include!`d below, with
//! stand-ins for the three names the generated module gives it (`Instance`, `Glue`,
//! `Exports`, only used by the guest call, which is never reached from here). The parser and
//! the response writer are then driven over a real loopback socket (they take a `TcpStream`).

pub mod model;
pub mod net;
pub mod respond;

#[allow(
    dead_code,
    unused_imports,
    clippy::all,
    reason = "the server is included verbatim from an example; only part of it is driven"
)]
mod server {
    pub struct Glue<T>(pub T);

    pub struct Instance<T> {
        pub imports: Option<T>,
    }

    impl<T> Instance<T> {
        pub fn new(imports: T) -> Self {
            Self {
                imports: Some(imports),
            }
        }
    }

    pub struct Exports;

    impl Exports {
        pub fn export_wasi_http_incoming_handler_0_2_0_handle<A, B>(
            _: &mut Instance<Glue<wasm2rs_wasi::WasiHost>>,
            _: A,
            _: B,
        ) {
            unreachable!("the guest is never called by the fuzz harness")
        }
    }

    include!("../../examples/wasip2_serve/server.rs");

    /// What the harness may reach (a child module sees the parent's private items).
    pub mod api {
        use super::*;
        use std::net::TcpStream;
        use std::time::{Duration, Instant};

        pub const MAX_HEAD: usize = super::MAX_HEAD;
        pub const MAX_BODY: usize = super::MAX_BODY;
        pub const DRAIN_TIME: Duration = super::DRAIN_TIME;

        /// A rejection as `(status, reason)`.
        pub type Rejection = (u16, &'static str);

        pub fn read_request(
            stream: &mut TcpStream,
            deadline: Instant,
        ) -> Result<(IncomingRequestParts, bool), Rejection> {
            super::read_request(stream, deadline).map_err(|Reject(status, why)| (status, why))
        }

        pub enum Frame {
            Length(usize),
            Chunked,
        }

        pub fn head(status: u16, headers: &[(String, Vec<u8>)], framing: Option<Frame>) -> Vec<u8> {
            super::head(
                status,
                headers,
                framing.map(|f| match f {
                    Frame::Length(n) => Framing::Length(n),
                    Frame::Chunked => Framing::Chunked,
                }),
            )
        }

        pub fn reason(status: u16) -> &'static str {
            super::reason(status)
        }

        pub fn respond(
            stream: &mut TcpStream,
            head_only: bool,
            http10: bool,
            response: ServedResponse,
            idle: Duration,
        ) {
            super::respond(stream, head_only, http10, response, idle)
        }

        pub fn respond_text(stream: &mut TcpStream, status: u16, text: &str) {
            super::respond_text(stream, status, text)
        }

        pub fn drain(stream: &mut TcpStream) {
            super::drain(stream)
        }
    }
}

pub use server::api;
pub use wasm2rs_wasi::IncomingRequestParts;
