//! `WasiHostBuilder::typed_traps`: a trap the host raises is a `WasiTrap` payload
//! (`resume_unwind`, so the panic hook stays quiet) instead of a message string.

mod common;

use common::builder;
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use wasm2rs_wasi::wasi_http_types::Fields;
use wasm2rs_wasi::wasi_io_streams::InputStream;
use wasm2rs_wasi::wasi_sockets_tcp::TcpSocket;
use wasm2rs_wasi::{Host, RandomError, RandomSource, WasiHost, WasiTrap};

fn payload_of<R>(f: impl FnOnce() -> R) -> Box<dyn Any + Send> {
    catch_unwind(AssertUnwindSafe(f))
        .err()
        .expect("the call must trap")
}

fn typed_trap<R>(f: impl FnOnce() -> R) -> WasiTrap {
    *payload_of(f)
        .downcast::<WasiTrap>()
        .expect("a typed trap is a WasiTrap payload")
}

fn string_trap<R>(f: impl FnOnce() -> R) -> String {
    match payload_of(f).downcast::<String>() {
        Ok(s) => *s,
        Err(p) => (*p.downcast::<&str>().expect("a string payload")).to_string(),
    }
}

struct Failing;

impl RandomSource for Failing {
    fn fill_random(&mut self, _: &mut [u8]) -> Result<(), RandomError> {
        Err(RandomError::new("entropy pool unavailable"))
    }
    fn insecure_u64(&mut self) -> u64 {
        0
    }
    fn insecure_seed(&mut self) -> (u64, u64) {
        (0, 0)
    }
}

#[test]
fn an_unknown_handle_is_an_invalid_resource_handle_by_kind() {
    let mut host = builder().typed_traps(true).build().unwrap();
    let t = typed_trap(|| host.wasi_sockets_tcp_method_tcp_socket_is_listening(TcpSocket(77)));
    assert_eq!(
        t,
        WasiTrap::InvalidResourceHandle {
            resource: "tcp-socket"
        }
    );
    let t = typed_trap(|| host.wasi_io_streams_method_input_stream_read(InputStream(9999), 1));
    assert_eq!(t, WasiTrap::InvalidResourceHandle { resource: "stream" });
    let t = typed_trap(|| host.wasi_http_types_method_fields_entries(Fields(5)));
    assert_eq!(
        t,
        WasiTrap::InvalidResourceHandle {
            resource: "http fields"
        }
    );
}

#[test]
fn off_by_default_each_trap_is_the_same_string_panic_as_before() {
    let mut host = builder().build().unwrap();
    assert_eq!(
        string_trap(|| host.wasi_sockets_tcp_method_tcp_socket_is_listening(TcpSocket(77))),
        "unknown or dropped tcp-socket"
    );
    assert_eq!(
        string_trap(|| host.wasi_io_streams_method_input_stream_read(InputStream(9999), 1)),
        "read from an unknown or dropped stream"
    );
    assert_eq!(
        string_trap(|| host.wasi_http_types_method_fields_entries(Fields(5))),
        "wasi:http: unknown fields handle"
    );
    let mut failing = WasiHost::builder().random(Failing).build().unwrap();
    assert_eq!(
        string_trap(|| failing.wasi_random_random_get_random_bytes(4)),
        "wasi:random: the random source failed: entropy pool unavailable"
    );
}

#[test]
fn a_failing_random_source_is_random_source_failed() {
    let mut host = WasiHost::builder()
        .random(Failing)
        .typed_traps(true)
        .build()
        .unwrap();
    let t = typed_trap(|| host.wasi_random_random_get_random_bytes(4));
    assert_eq!(
        t,
        WasiTrap::RandomSourceFailed {
            message: "entropy pool unavailable".to_string()
        }
    );
}

#[cfg(not(feature = "os-random"))]
#[test]
fn no_random_source_is_random_source_missing() {
    let mut host = WasiHost::new(Vec::new()).with_typed_traps(true);
    let t = typed_trap(|| host.wasi_random_random_get_random_bytes(4));
    assert_eq!(t, WasiTrap::RandomSourceMissing);
}

#[test]
fn a_guest_contract_violation_stays_a_string_panic_when_typed() {
    let mut host = builder().typed_traps(true).build().unwrap();
    let m = string_trap(|| host.wasi_random_random_get_random_bytes((64 << 20) + 1));
    assert!(m.contains("random bytes: length over 64 MiB"), "{m}");
}

#[test]
fn display_keeps_the_untyped_messages() {
    fn assert_error<E: std::error::Error + Send + Sync + 'static>() {}
    assert_error::<WasiTrap>();
    for (trap, text) in [
        (
            WasiTrap::InvalidResourceHandle {
                resource: "udp-socket",
            },
            "unknown or dropped udp-socket",
        ),
        (
            WasiTrap::InvalidResourceHandle {
                resource: "http fields",
            },
            "wasi:http: unknown fields handle",
        ),
        (
            WasiTrap::InvalidResourceHandle { resource: "stream" },
            "read from an unknown or dropped stream",
        ),
        (WasiTrap::HandleTableExhausted, "handle table exhausted"),
        (
            WasiTrap::RandomSourceMissing,
            "wasi:random: no random source (enable `os-random` or use the builder)",
        ),
        (
            WasiTrap::RandomSourceFailed {
                message: "x".into(),
            },
            "wasi:random: the random source failed: x",
        ),
    ] {
        assert_eq!(trap.to_string(), text);
    }
}
