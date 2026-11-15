//! `WasiHostBuilder::typed_traps`: a trap the host raises is a `WasiTrap` payload
//! (`resume_unwind`, so the panic hook stays quiet) instead of a message string.

mod common;

use common::{Zeros, builder};
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use wasm2rs_wasi::wasi_http_types::Fields;
use wasm2rs_wasi::wasi_io_streams::InputStream;
use wasm2rs_wasi::wasi_sockets_tcp::TcpSocket;
use wasm2rs_wasi::{Host, RandomError, RandomSource, WasiHost, WasiTrap};

fn payload_of(f: impl FnOnce()) -> Box<dyn Any + Send> {
    catch_unwind(AssertUnwindSafe(f)).expect_err("the call must trap")
}

fn typed(payload: Box<dyn Any + Send>) -> WasiTrap {
    *payload
        .downcast::<WasiTrap>()
        .expect("a typed trap is a WasiTrap payload")
}

fn message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
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
    let t = typed(payload_of(|| {
        host.wasi_sockets_tcp_method_tcp_socket_is_listening(TcpSocket(77));
    }));
    assert_eq!(
        t,
        WasiTrap::InvalidResourceHandle {
            resource: "tcp-socket"
        }
    );
    let t = typed(payload_of(|| {
        drop(host.wasi_io_streams_method_input_stream_read(InputStream(9999), 1))
    }));
    assert_eq!(t, WasiTrap::InvalidResourceHandle { resource: "stream" });
    let t = typed(payload_of(|| {
        drop(host.wasi_http_types_method_fields_entries(Fields(5)))
    }));
    assert_eq!(
        t,
        WasiTrap::InvalidResourceHandle {
            resource: "http fields"
        }
    );
}

#[test]
fn the_display_of_each_variant_is_the_untyped_message() {
    let mut off = builder().build().unwrap();
    let mut on = builder().typed_traps(true).build().unwrap();
    for (untyped, typed_) in [
        (
            message(payload_of(|| {
                off.wasi_sockets_tcp_method_tcp_socket_is_listening(TcpSocket(77));
            })),
            typed(payload_of(|| {
                on.wasi_sockets_tcp_method_tcp_socket_is_listening(TcpSocket(77));
            })),
        ),
        (
            message(payload_of(|| {
                drop(off.wasi_http_types_method_fields_entries(Fields(5)))
            })),
            typed(payload_of(|| {
                drop(on.wasi_http_types_method_fields_entries(Fields(5)))
            })),
        ),
        (
            message(payload_of(|| {
                drop(off.wasi_io_streams_method_input_stream_read(InputStream(9999), 1))
            })),
            typed(payload_of(|| {
                drop(on.wasi_io_streams_method_input_stream_read(InputStream(9999), 1))
            })),
        ),
    ] {
        assert_eq!(typed_.to_string(), untyped);
    }
}

#[test]
fn off_by_default_the_trap_is_a_string_panic() {
    let mut host = builder().build().unwrap();
    let m = message(payload_of(|| {
        host.wasi_sockets_tcp_method_tcp_socket_is_listening(TcpSocket(77));
    }));
    assert_eq!(m, "unknown or dropped tcp-socket");
}

#[test]
fn a_failing_random_source_is_random_source_failed() {
    let mut host = WasiHost::builder()
        .random(Failing)
        .typed_traps(true)
        .build()
        .unwrap();
    let t = typed(payload_of(|| {
        drop(host.wasi_random_random_get_random_bytes(4))
    }));
    assert_eq!(
        t,
        WasiTrap::RandomSourceFailed {
            message: "entropy pool unavailable".to_string()
        }
    );
    assert_eq!(
        t.to_string(),
        "wasi:random: the random source failed: entropy pool unavailable"
    );
    let mut off = WasiHost::builder().random(Failing).build().unwrap();
    assert_eq!(
        message(payload_of(|| drop(
            off.wasi_random_random_get_random_bytes(4)
        ))),
        t.to_string()
    );
}

#[cfg(not(feature = "os-random"))]
#[test]
fn no_random_source_is_random_source_missing() {
    let mut host = WasiHost::new(Vec::new()).with_typed_traps(true);
    let t = typed(payload_of(|| {
        drop(host.wasi_random_random_get_random_bytes(4))
    }));
    assert_eq!(t, WasiTrap::RandomSourceMissing);
}

#[test]
fn a_guest_contract_violation_stays_a_string_panic_when_typed() {
    let mut host = builder().typed_traps(true).build().unwrap();
    let m = message(payload_of(|| {
        drop(host.wasi_random_random_get_random_bytes((64 << 20) + 1))
    }));
    assert!(m.contains("random bytes: length over 64 MiB"), "{m}");
}

#[test]
fn the_trap_is_an_error_and_display_names_each_variant() {
    fn is_error<E: std::error::Error + Send + Sync + 'static>(_: &E) {}
    let all = [
        WasiTrap::HandleTableExhausted,
        WasiTrap::RandomSourceMissing,
        WasiTrap::RandomSourceFailed {
            message: "x".into(),
        },
        WasiTrap::InvalidResourceHandle {
            resource: "pollable",
        },
    ];
    for t in &all {
        is_error(t);
        assert!(!t.to_string().is_empty());
    }
    assert_eq!(all[0].to_string(), "handle table exhausted");
    let _ = Zeros;
}
