//! The incoming side of `wasi:http` (H5): `incoming-request`, `outgoing-response` and
//! `response-outparam`, driven through the `Host` trait the way a guest's
//! `incoming-handler#handle` does. The component path is covered by the `proxy` tests of the
//! workspace; these are the edges.

use std::io::Read;
use std::time::Duration;

use wasm2rs_wasi::wasi_http_types::{
    ErrorCode, Method, OutgoingResponse, ResponseOutparam, Scheme,
};
use wasm2rs_wasi::wasi_io_streams::StreamError;
use wasm2rs_wasi::{
    Host, IncomingRequestParts, RandomError, RandomSource, ServedBody, WasiHost, WasiHostBuilder,
};

/// A random source for the build without the `os-random` feature, which has no default one.
struct Fixed;

impl RandomSource for Fixed {
    fn fill_random(&mut self, buf: &mut [u8]) -> Result<(), RandomError> {
        buf.fill(0);
        Ok(())
    }
    fn insecure_u64(&mut self) -> u64 {
        42
    }
    fn insecure_seed(&mut self) -> (u64, u64) {
        (1, 2)
    }
}

fn builder() -> WasiHostBuilder {
    let builder = WasiHost::builder();
    if cfg!(feature = "os-random") {
        builder
    } else {
        builder.random(Fixed)
    }
}

fn host() -> WasiHost {
    builder().build().unwrap()
}

fn parts() -> IncomingRequestParts {
    IncomingRequestParts {
        method: Method::Put,
        path_with_query: Some("/a?b=c".into()),
        scheme: Some(Scheme::HTTP),
        authority: Some("localhost:80".into()),
        headers: vec![
            ("Accept".into(), b"a".to_vec()),
            ("accept".into(), b"b".to_vec()),
        ],
        body: b"payload".to_vec(),
        trailers: None,
    }
}

/// Reads a served body to its end.
fn read_all(body: &mut ServedBody) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    body.read_to_end(&mut out)?;
    Ok(out)
}

/// A response the guest builds: empty headers.
fn new_response(h: &mut WasiHost) -> OutgoingResponse {
    let headers = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_constructor_outgoing_response(headers)
}

fn set_ok(h: &mut WasiHost, out: ResponseOutparam, response: OutgoingResponse) {
    h.wasi_http_types_static_response_outparam_set(out, Ok(response));
}

#[test]
fn the_request_parts_are_what_the_embedder_minted() {
    let mut h = host();
    let r = h.new_incoming_request(parts());
    assert_eq!(
        h.wasi_http_types_method_incoming_request_method(r),
        Method::Put
    );
    assert_eq!(
        h.wasi_http_types_method_incoming_request_path_with_query(r),
        Some("/a?b=c".to_string())
    );
    assert_eq!(
        h.wasi_http_types_method_incoming_request_scheme(r),
        Some(Scheme::HTTP)
    );
    assert_eq!(
        h.wasi_http_types_method_incoming_request_authority(r),
        Some("localhost:80".to_string())
    );
    let headers = h.wasi_http_types_method_incoming_request_headers(r);
    // names match case-insensitively and keep every value, in order
    assert_eq!(
        h.wasi_http_types_method_fields_get(headers, "ACCEPT".into()),
        vec![b"a".to_vec(), b"b".to_vec()]
    );
}

#[test]
fn the_headers_of_a_request_are_immutable() {
    let mut h = host();
    let r = h.new_incoming_request(parts());
    let headers = h.wasi_http_types_method_incoming_request_headers(r);
    assert!(
        h.wasi_http_types_method_fields_append(headers, "x".into(), b"1".to_vec())
            .is_err()
    );
}

#[test]
fn the_body_of_a_request_is_read_through_consume_once() {
    let mut h = host();
    let r = h.new_incoming_request(parts());
    let body = h
        .wasi_http_types_method_incoming_request_consume(r)
        .unwrap();
    assert!(
        h.wasi_http_types_method_incoming_request_consume(r)
            .is_err()
    );
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    let mut got = Vec::new();
    loop {
        match h.wasi_io_streams_method_input_stream_blocking_read(stream, 3) {
            Ok(bytes) => got.extend(bytes),
            Err(StreamError::Closed) => break,
            Err(other) => panic!("{other:?}"),
        }
    }
    assert_eq!(got, b"payload");
}

#[test]
fn a_response_starts_as_200_and_takes_status_codes_of_http() {
    let mut h = host();
    let response = new_response(&mut h);
    assert_eq!(
        h.wasi_http_types_method_outgoing_response_status_code(response),
        200
    );
    for bad in [0, 99, 1000, u16::MAX] {
        assert!(
            h.wasi_http_types_method_outgoing_response_set_status_code(response, bad)
                .is_err(),
            "{bad}"
        );
    }
    // wasmtime accepts 100..=999, so 600 and above are not refused here either
    for ok in [100, 404, 600, 999] {
        h.wasi_http_types_method_outgoing_response_set_status_code(response, ok)
            .unwrap();
        assert_eq!(
            h.wasi_http_types_method_outgoing_response_status_code(response),
            ok
        );
    }
}

#[test]
fn the_headers_of_a_response_are_immutable_once_built() {
    let mut h = host();
    let fields = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_method_fields_append(fields, "x-a".into(), b"1".to_vec())
        .unwrap();
    let response = h.wasi_http_types_constructor_outgoing_response(fields);
    let view = h.wasi_http_types_method_outgoing_response_headers(response);
    assert_eq!(
        h.wasi_http_types_method_fields_get(view, "x-a".into()),
        vec![b"1".to_vec()]
    );
    assert!(
        h.wasi_http_types_method_fields_set(view, "x-a".into(), vec![b"2".to_vec()])
            .is_err()
    );
    let out = h.new_response_outparam();
    set_ok(&mut h, out, response);
    let served = h.take_response(out).unwrap().unwrap();
    assert_eq!(served.headers, vec![("x-a".to_string(), b"1".to_vec())]);
}

/// Writes `bytes` to the body of `response` and finishes it.
fn write_body(h: &mut WasiHost, response: OutgoingResponse, bytes: &[u8]) {
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    let out = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    h.wasi_io_streams_method_output_stream_write(out, bytes.to_vec())
        .unwrap();
    h.wasi_io_streams_resource_drop_output_stream(out);
    h.wasi_http_types_static_outgoing_body_finish(body, None)
        .unwrap();
}

#[test]
fn a_set_response_is_taken_with_its_body() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    h.wasi_http_types_method_outgoing_response_set_status_code(response, 201)
        .unwrap();
    write_body(&mut h, response, b"made");
    set_ok(&mut h, out, response);
    let mut served = h.take_response(out).unwrap().unwrap();
    assert_eq!(served.status, 201);
    assert!(served.headers.is_empty());
    assert_eq!(served.body.finished_len(), Some(4));
    assert_eq!(read_all(&mut served.body).unwrap(), b"made");
    assert_eq!(served.body.trailers(), None);
}

#[test]
fn a_response_without_a_body_call_has_an_empty_body() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    set_ok(&mut h, out, response);
    let mut served = h.take_response(out).unwrap().unwrap();
    assert_eq!(read_all(&mut served.body).unwrap(), b"");
}

#[test]
fn the_body_of_a_response_can_be_asked_for_once() {
    let mut h = host();
    let response = new_response(&mut h);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    assert!(
        h.wasi_http_types_method_outgoing_response_body(response)
            .is_err()
    );
    h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    assert!(h.wasi_http_types_method_outgoing_body_write(body).is_err());
}

#[test]
fn an_answer_is_taken_once() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    set_ok(&mut h, out, response);
    assert!(h.take_response(out).is_some());
    assert!(h.take_response(out).is_none());
}

#[test]
fn an_outparam_the_guest_dropped_has_no_answer() {
    let mut h = host();
    let out = h.new_response_outparam();
    h.wasi_http_types_resource_drop_response_outparam(out);
    assert!(h.take_response(out).is_none());
}

#[test]
fn an_error_the_guest_set_is_the_answer() {
    let mut h = host();
    let out = h.new_response_outparam();
    h.wasi_http_types_static_response_outparam_set(out, Err(ErrorCode::LoopDetected));
    assert!(matches!(
        h.take_response(out),
        Some(Err(ErrorCode::LoopDetected))
    ));
}

#[test]
fn a_body_the_guest_did_not_finish_is_streamed_as_it_is_written() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    let stream = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    h.wasi_io_streams_method_output_stream_write(stream, b"one ".to_vec())
        .unwrap();
    set_ok(&mut h, out, response);

    // `handle` returned with the body open: the answer is there, the end is not
    let mut served = h.take_response(out).unwrap().unwrap();
    assert!(!served.body.is_finished());
    assert_eq!(served.body.finished_len(), None);
    let reader = std::thread::spawn(move || read_all(&mut served.body));

    std::thread::sleep(Duration::from_millis(50));
    h.wasi_io_streams_method_output_stream_write(stream, b"two".to_vec())
        .unwrap();
    h.wasi_io_streams_resource_drop_output_stream(stream);
    h.wasi_http_types_static_outgoing_body_finish(body, None)
        .unwrap();
    assert_eq!(reader.join().unwrap().unwrap(), b"one two");
}

#[test]
fn a_streamed_body_may_pass_the_buffer_limit_while_it_is_read() {
    let mut h = builder().max_request_body(4).build().unwrap();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    let stream = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    set_ok(&mut h, out, response);
    let mut served = h.take_response(out).unwrap().unwrap();
    for chunk in [b"abcd", b"efgh"] {
        h.wasi_io_streams_method_output_stream_write(stream, chunk.to_vec())
            .unwrap();
        let mut got = [0u8; 4];
        served.body.read_exact(&mut got).unwrap();
        assert_eq!(&got, chunk);
    }
}

#[test]
fn reading_an_open_body_gives_up_after_the_idle_timeout() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    h.wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    set_ok(&mut h, out, response);
    let mut served = h.take_response(out).unwrap().unwrap();
    served
        .body
        .set_idle_timeout(Some(Duration::from_millis(50)));
    let err = read_all(&mut served.body).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
}

#[test]
fn a_body_that_fails_while_streamed_ends_the_read_with_an_error() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    set_ok(&mut h, out, response);
    let mut served = h.take_response(out).unwrap().unwrap();
    h.wasi_http_types_resource_drop_outgoing_body(body);
    assert!(read_all(&mut served.body).is_err());
}

#[test]
fn dropping_the_host_fails_a_body_that_was_still_open() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    h.wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    set_ok(&mut h, out, response);
    let mut served = h.take_response(out).unwrap().unwrap();
    drop(h);
    assert!(read_all(&mut served.body).is_err());
}

#[test]
fn a_body_dropped_before_finish_fails_the_response() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    h.wasi_http_types_resource_drop_outgoing_body(body);
    set_ok(&mut h, out, response);
    assert!(matches!(
        h.take_response(out),
        Some(Err(ErrorCode::InternalError(_)))
    ));
}

#[test]
fn a_response_body_over_the_limit_reports_the_response_body_size() {
    let mut h = builder().max_request_body(4).build().unwrap();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    let stream = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    assert!(
        h.wasi_io_streams_method_output_stream_write(stream, vec![0; 5])
            .is_err()
    );
    assert_eq!(
        h.wasi_http_types_static_outgoing_body_finish(body, None),
        Err(ErrorCode::HTTPResponseBodySize(Some(5)))
    );
    set_ok(&mut h, out, response);
    assert!(matches!(
        h.take_response(out),
        Some(Err(ErrorCode::HTTPResponseBodySize(Some(5))))
    ));
}

fn fields_of(h: &mut WasiHost, entries: &[(&str, &str)]) -> wasm2rs_wasi::wasi_http_types::Fields {
    let fields = h.wasi_http_types_constructor_fields();
    for (name, value) in entries {
        h.wasi_http_types_method_fields_append(fields, name.to_string(), value.as_bytes().to_vec())
            .unwrap();
    }
    fields
}

#[test]
fn trailers_finishing_a_response_body_come_with_the_served_body() {
    let mut h = host();
    let out = h.new_response_outparam();
    let response = new_response(&mut h);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .unwrap();
    let trailers = fields_of(&mut h, &[("x-sum", "42"), ("x-sum", "43")]);
    h.wasi_http_types_static_outgoing_body_finish(body, Some(trailers))
        .unwrap();
    set_ok(&mut h, out, response);
    let mut served = h.take_response(out).unwrap().unwrap();
    assert_eq!(read_all(&mut served.body).unwrap(), b"");
    assert_eq!(
        served.body.trailers(),
        Some(vec![
            ("x-sum".to_string(), b"42".to_vec()),
            ("x-sum".to_string(), b"43".to_vec()),
        ])
    );
}

#[test]
fn trailers_of_an_incoming_request_arrive_through_future_trailers() {
    let mut h = host();
    let mut p = parts();
    p.trailers = Some(vec![("x-end".into(), b"done".to_vec())]);
    let r = h.new_incoming_request(p);
    let body = h
        .wasi_http_types_method_incoming_request_consume(r)
        .unwrap();
    let future = h.wasi_http_types_static_incoming_body_finish(body);
    let got = h
        .wasi_http_types_method_future_trailers_get(future)
        .unwrap()
        .unwrap()
        .unwrap()
        .expect("the trailers");
    assert_eq!(
        h.wasi_http_types_method_fields_get(got, "X-End".into()),
        vec![b"done".to_vec()]
    );
    // immutable, like any received fields
    assert!(
        h.wasi_http_types_method_fields_set(got, "x-end".into(), vec![])
            .is_err()
    );
    assert!(
        h.wasi_http_types_method_future_trailers_get(future)
            .unwrap()
            .is_err()
    );
}

#[test]
fn a_request_without_trailers_answers_none_through_future_trailers() {
    let mut h = host();
    let r = h.new_incoming_request(parts());
    let body = h
        .wasi_http_types_method_incoming_request_consume(r)
        .unwrap();
    let future = h.wasi_http_types_static_incoming_body_finish(body);
    assert!(
        h.wasi_http_types_method_future_trailers_get(future)
            .unwrap()
            .unwrap()
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_dropped_request_or_response_frees_its_slot() {
    let mut h = host();
    let r = h.new_incoming_request(parts());
    h.wasi_http_types_resource_drop_incoming_request(r);
    assert_eq!(h.new_incoming_request(parts()), r);
    let response = new_response(&mut h);
    h.wasi_http_types_resource_drop_outgoing_response(response);
    assert_eq!(new_response(&mut h), response);
}

#[test]
fn an_outparam_that_was_taken_cannot_set_a_reused_slot() {
    let mut h = host();
    let old = h.new_response_outparam();
    let response = new_response(&mut h);
    set_ok(&mut h, old, response);
    assert!(h.take_response(old).is_some());

    // the next call's outparam may reuse the old one's storage
    let fresh = h.new_response_outparam();
    let late = new_response(&mut h);
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        set_ok(&mut h, old, late);
    }));
    assert!(refused.is_err(), "a spent outparam must be refused");
    assert!(
        h.take_response(fresh).is_none(),
        "the fresh outparam must stay unset"
    );
}

#[test]
fn a_dropped_guest_outparam_handle_stays_distinct_after_take() {
    let mut h = host();
    let old = h.new_response_outparam();
    assert!(h.take_response(old).is_none());
    let fresh = h.new_response_outparam();
    assert_ne!(old, fresh);
}
