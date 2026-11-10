//! The send side of `wasi:http` (H3): `outgoing-request.body`, `outgoing-body.write` /
//! `finish`, and `future-trailers`, driven through the `Host` trait with an injected
//! `HttpClient` (the external dependency).
#![allow(clippy::unwrap_used, reason = "test code")]

use std::io::Cursor;
use std::sync::mpsc;
use std::time::Duration;

use wasm2rs_wasi::wasi_http_types::{
    ErrorCode, Fields, FutureIncomingResponse, HeaderError, IncomingResponse, Method, OutgoingBody,
    OutgoingRequest, Scheme,
};
use wasm2rs_wasi::wasi_io_streams::StreamError;
use wasm2rs_wasi::{
    Host, HttpClient, HttpClientError, HttpRequest, HttpResponse, RandomError, RandomSource,
    WasiHost, WasiHostBuilder,
};

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

/// A client that sends every request it gets through `seen` and answers `200` with `body`.
struct Recording {
    seen: mpsc::Sender<HttpRequest>,
    body: &'static str,
}

impl HttpClient for Recording {
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.seen.send(request).unwrap();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Cursor::new(self.body.as_bytes())),
            trailers: None,
        })
    }
}

fn host() -> (WasiHost, mpsc::Receiver<HttpRequest>) {
    let (seen, requests) = mpsc::channel();
    let client = Recording { seen, body: "ok" };
    (builder().http_client(client).build().unwrap(), requests)
}

fn post(h: &mut WasiHost) -> OutgoingRequest {
    let headers = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(headers);
    h.wasi_http_types_method_outgoing_request_set_method(r, Method::Post)
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(Scheme::HTTP))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_authority(r, Some("127.0.0.1:1".into()))
        .unwrap();
    r
}

/// Writes `data` with `blocking-write-and-flush` through the body's stream, then drops it.
fn write(h: &mut WasiHost, body: OutgoingBody, data: &[u8]) {
    let out = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    for chunk in data.chunks(4096) {
        h.wasi_io_streams_method_output_stream_blocking_write_and_flush(out, chunk.to_vec())
            .unwrap();
    }
    h.wasi_io_streams_resource_drop_output_stream(out);
}

fn finished(
    h: &mut WasiHost,
    future: FutureIncomingResponse,
) -> Result<IncomingResponse, ErrorCode> {
    let pollable = h.wasi_http_types_method_future_incoming_response_subscribe(future);
    h.wasi_io_poll_method_pollable_block(pollable);
    h.wasi_http_types_method_future_incoming_response_get(future)
        .expect("ready future has a result")
        .expect("result not yet taken")
}

#[test]
fn the_body_written_before_finish_reaches_the_client() {
    let (mut h, requests) = host();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let out = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    h.wasi_io_streams_method_output_stream_blocking_write_and_flush(out, b"hello ".to_vec())
        .unwrap();
    assert!(
        requests.recv_timeout(Duration::from_millis(200)).is_err(),
        "the request must wait for finish"
    );
    h.wasi_io_streams_method_output_stream_blocking_write_and_flush(out, b"world".to_vec())
        .unwrap();
    h.wasi_io_streams_resource_drop_output_stream(out);
    h.wasi_http_types_static_outgoing_body_finish(body, None)
        .unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.method, "POST");
    assert_eq!(request.body, b"hello world");
    finished(&mut h, future).unwrap();
}

#[test]
fn body_and_write_can_each_be_called_once() {
    let (mut h, _requests) = host();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    assert!(h.wasi_http_types_method_outgoing_request_body(r).is_err());
    h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    assert!(h.wasi_http_types_method_outgoing_body_write(body).is_err());
}

#[test]
fn handle_without_a_body_call_sends_an_empty_body_at_once() {
    let (mut h, requests) = host();
    let r = post(&mut h);
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(request.body.is_empty());
    finished(&mut h, future).unwrap();
}

#[test]
fn a_body_finished_before_handle_is_sent_by_handle() {
    let (mut h, requests) = host();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    write(&mut h, body, b"early");
    h.wasi_http_types_static_outgoing_body_finish(body, None)
        .unwrap();
    assert!(
        requests.try_recv().is_err(),
        "nothing is sent before handle"
    );
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(5)).unwrap().body,
        b"early"
    );
    finished(&mut h, future).unwrap();
}

#[test]
fn dropping_the_body_without_finish_discards_the_request() {
    let (mut h, requests) = host();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    write(&mut h, body, b"partial");
    h.wasi_http_types_resource_drop_outgoing_body(body);
    let error = finished(&mut h, future).unwrap_err();
    assert!(
        matches!(error, ErrorCode::InternalError(Some(_))),
        "{error:?}"
    );
    assert!(
        requests.recv_timeout(Duration::from_millis(200)).is_err(),
        "the client must not run"
    );
}

#[test]
fn check_write_is_at_most_4096_and_shrinks_to_the_limit() {
    let (seen, _requests) = mpsc::channel();
    let client = Recording { seen, body: "" };
    let mut h = builder()
        .http_client(client)
        .max_request_body(5000)
        .build()
        .unwrap();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let out = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    assert_eq!(
        h.wasi_io_streams_method_output_stream_check_write(out),
        Ok(4096)
    );
    h.wasi_io_streams_method_output_stream_write(out, vec![0; 4096])
        .unwrap();
    assert_eq!(
        h.wasi_io_streams_method_output_stream_check_write(out),
        Ok(904)
    );
}

#[test]
fn exceeding_the_body_limit_fails_the_stream_and_finish() {
    let (seen, requests) = mpsc::channel();
    let client = Recording { seen, body: "" };
    let mut h = builder()
        .http_client(client)
        .max_request_body(10)
        .build()
        .unwrap();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let out = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    let error = h
        .wasi_io_streams_method_output_stream_blocking_write_and_flush(out, vec![1; 11])
        .unwrap_err();
    assert!(
        matches!(error, StreamError::LastOperationFailed(_)),
        "{error:?}"
    );
    assert!(matches!(
        h.wasi_io_streams_method_output_stream_check_write(out),
        Err(StreamError::Closed)
    ));
    h.wasi_io_streams_resource_drop_output_stream(out);
    assert!(matches!(
        h.wasi_http_types_static_outgoing_body_finish(body, None),
        Err(ErrorCode::HTTPRequestBodySize(Some(11)))
    ));
    assert!(matches!(
        finished(&mut h, future),
        Err(ErrorCode::HTTPRequestBodySize(Some(11)))
    ));
    assert!(requests.try_recv().is_err());
}

fn fields_of(h: &mut WasiHost, entries: &[(&str, &str)]) -> Fields {
    let fields = h.wasi_http_types_constructor_fields();
    for (name, value) in entries {
        h.wasi_http_types_method_fields_append(fields, name.to_string(), value.as_bytes().to_vec())
            .unwrap();
    }
    fields
}

#[test]
fn request_trailers_reach_the_client() {
    let (mut h, requests) = host();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    write(&mut h, body, b"data");
    let trailers = fields_of(&mut h, &[("x-sum", "1"), ("x-sum", "2"), ("x-end", "done")]);
    h.wasi_http_types_static_outgoing_body_finish(body, Some(trailers))
        .unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(request.body, b"data");
    assert_eq!(
        request.trailers,
        vec![
            ("x-sum".to_string(), b"1".to_vec()),
            ("x-sum".to_string(), b"2".to_vec()),
            ("x-end".to_string(), b"done".to_vec()),
        ]
    );
    finished(&mut h, future).unwrap();
}

#[test]
fn a_request_finished_without_or_with_empty_trailers_has_none() {
    let (mut h, requests) = host();
    for empty in [false, true] {
        let r = post(&mut h);
        let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
        let trailers = empty.then(|| fields_of(&mut h, &[]));
        h.wasi_http_types_static_outgoing_body_finish(body, trailers)
            .unwrap();
        let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
        let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(request.trailers.is_empty());
        finished(&mut h, future).unwrap();
    }
}

#[test]
fn forbidden_trailers_are_refused_and_the_request_fails() {
    // a received message may carry names a guest could not set; a clone of its fields is
    // mutable, so the trailers are checked again at `finish`
    struct Connection {
        sent: mpsc::Sender<HttpRequest>,
    }
    impl HttpClient for Connection {
        fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
            self.sent.send(request).unwrap();
            Ok(HttpResponse {
                status: 200,
                headers: vec![("connection".into(), b"close".to_vec())],
                body: Box::new(Cursor::new(Vec::new())),
                trailers: None,
            })
        }
    }
    let (sent, requests) = mpsc::channel();
    let mut h = builder().http_client(Connection { sent }).build().unwrap();
    let first = post(&mut h);
    let future = h.wasi_http_outgoing_handler_handle(first, None).unwrap();
    let response = finished(&mut h, future).unwrap();
    requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let received = h.wasi_http_types_method_incoming_response_headers(response);
    let cloned = h.wasi_http_types_method_fields_clone(received);

    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let refused = h
        .wasi_http_types_static_outgoing_body_finish(body, Some(cloned))
        .unwrap_err();
    assert!(
        matches!(refused, ErrorCode::InternalError(Some(_))),
        "{refused:?}"
    );
    assert!(finished(&mut h, future).is_err());
    assert!(requests.try_recv().is_err());
}

#[test]
fn a_guest_cannot_put_a_forbidden_name_in_trailers() {
    let (mut h, _requests) = host();
    let fields = h.wasi_http_types_constructor_fields();
    assert_eq!(
        h.wasi_http_types_method_fields_append(fields, "transfer-encoding".into(), b"x".to_vec()),
        Err(HeaderError::Forbidden)
    );
}

/// A client answering `200` with `body` and, after it, `trailers` (`None`: no reader).
struct WithTrailers {
    trailers: Option<Vec<(String, Vec<u8>)>>,
}

impl HttpClient for WithTrailers {
    fn send(&mut self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        let trailers = self.trailers.take();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Cursor::new(b"body".to_vec())),
            trailers: trailers.map(|t| Box::new(move || t) as _),
        })
    }
}

/// Reads the response body to its end and finishes it; the answer of `future-trailers.get`.
fn trailers_of_response(h: &mut WasiHost) -> Option<Fields> {
    let r = post(h);
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let response = finished(h, future).unwrap();
    let incoming = h
        .wasi_http_types_method_incoming_response_consume(response)
        .unwrap();
    let stream = h
        .wasi_http_types_method_incoming_body_stream(incoming)
        .unwrap();
    let mut read = Vec::new();
    loop {
        match h.wasi_io_streams_method_input_stream_blocking_read(stream, 4096) {
            Ok(chunk) => read.extend(chunk),
            Err(StreamError::Closed) => break,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!(read, b"body");
    h.wasi_io_streams_resource_drop_input_stream(stream);
    let future = Host::wasi_http_types_static_incoming_body_finish(h, incoming);
    let pollable = h.wasi_http_types_method_future_trailers_subscribe(future);
    h.wasi_io_poll_method_pollable_block(pollable);
    let Some(Ok(Ok(fields))) = h.wasi_http_types_method_future_trailers_get(future) else {
        panic!("future-trailers.get must answer ok");
    };
    fields
}

#[test]
fn response_trailers_come_through_future_trailers_after_the_body() {
    let trailers = vec![
        ("x-sum".to_string(), b"1".to_vec()),
        ("x-sum".to_string(), b"2".to_vec()),
    ];
    let mut h = builder()
        .http_client(WithTrailers {
            trailers: Some(trailers),
        })
        .build()
        .unwrap();
    let fields = trailers_of_response(&mut h).expect("some trailers");
    assert_eq!(
        h.wasi_http_types_method_fields_get(fields, "x-sum".into()),
        vec![b"1".to_vec(), b"2".to_vec()]
    );
    // received trailers cannot be changed
    assert_eq!(
        h.wasi_http_types_method_fields_append(fields, "x-more".into(), b"3".to_vec()),
        Err(HeaderError::Immutable)
    );
}

#[test]
fn a_response_without_or_with_empty_trailers_answers_none() {
    for trailers in [None, Some(vec![])] {
        let mut h = builder()
            .http_client(WithTrailers { trailers })
            .build()
            .unwrap();
        assert!(trailers_of_response(&mut h).is_none());
    }
}

#[test]
fn a_flush_and_check_write_leave_the_body_open() {
    let (mut h, requests) = host();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let out = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    h.wasi_io_streams_method_output_stream_write(out, b"ab".to_vec())
        .unwrap();
    h.wasi_io_streams_method_output_stream_flush(out).unwrap();
    assert!(requests.recv_timeout(Duration::from_millis(200)).is_err());
    h.wasi_io_streams_resource_drop_output_stream(out);
    h.wasi_http_types_static_outgoing_body_finish(body, None)
        .unwrap();
    assert_eq!(
        requests.recv_timeout(Duration::from_secs(5)).unwrap().body,
        b"ab"
    );
    finished(&mut h, future).unwrap();
}

#[test]
fn a_body_on_head_reaches_the_client_unchanged() {
    // the host does not judge the method: a client that refuses it reports a client error
    let (mut h, requests) = host();
    let r = post(&mut h);
    h.wasi_http_types_method_outgoing_request_set_method(r, Method::Head)
        .unwrap();
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    write(&mut h, body, b"x");
    h.wasi_http_types_static_outgoing_body_finish(body, None)
        .unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        (request.method.as_str(), request.body.as_slice()),
        ("HEAD", &b"x"[..])
    );
    finished(&mut h, future).unwrap();
}

#[test]
fn a_client_error_after_the_body_is_an_internal_error() {
    struct Refusing;
    impl HttpClient for Refusing {
        fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
            Err(HttpClientError::new(format!(
                "no body for {}",
                request.method
            )))
        }
    }
    let mut h = builder().http_client(Refusing).build().unwrap();
    let r = post(&mut h);
    h.wasi_http_types_method_outgoing_request_set_method(r, Method::Connect)
        .unwrap();
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    h.wasi_http_types_static_outgoing_body_finish(body, None)
        .unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    assert_eq!(
        finished(&mut h, future).unwrap_err(),
        ErrorCode::InternalError(Some("no body for CONNECT".into()))
    );
}

#[test]
fn incoming_body_finish_gives_future_trailers_that_answer_once() {
    let (mut h, _requests) = host();
    let r = post(&mut h);
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let response = finished(&mut h, future).unwrap();
    let incoming = h
        .wasi_http_types_method_incoming_response_consume(response)
        .unwrap();
    let trailers = Host::wasi_http_types_static_incoming_body_finish(&mut h, incoming);
    let pollable = h.wasi_http_types_method_future_trailers_subscribe(trailers);
    h.wasi_io_poll_method_pollable_block(pollable);
    assert!(matches!(
        h.wasi_http_types_method_future_trailers_get(trailers),
        Some(Ok(Ok(None)))
    ));
    assert!(matches!(
        h.wasi_http_types_method_future_trailers_get(trailers),
        Some(Err(()))
    ));
    h.wasi_http_types_resource_drop_future_trailers(trailers);
}

#[test]
fn the_shared_client_still_serves_a_second_request() {
    let (mut h, requests) = host();
    for round in 0..2u8 {
        let r = post(&mut h);
        let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
        write(&mut h, body, &[round]);
        h.wasi_http_types_static_outgoing_body_finish(body, None)
            .unwrap();
        let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
        finished(&mut h, future).unwrap();
    }
    let bodies: Vec<_> = requests.try_iter().map(|r| r.body).collect();
    assert_eq!(bodies, vec![vec![0], vec![1]]);
}

#[test]
fn a_full_body_still_permits_a_write_that_then_fails() {
    let (seen, _requests) = mpsc::channel();
    let client = Recording { seen, body: "" };
    let mut h = builder()
        .http_client(client)
        .max_request_body(4)
        .build()
        .unwrap();
    let r = post(&mut h);
    let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
    let out = h.wasi_http_types_method_outgoing_body_write(body).unwrap();
    h.wasi_io_streams_method_output_stream_write(out, vec![0; 4])
        .unwrap();
    // exactly the limit is fine, and the guest's check-write loop must not see 0 for ever
    assert!(
        h.wasi_io_streams_method_output_stream_check_write(out)
            .unwrap()
            > 0
    );
    let error = h
        .wasi_io_streams_method_output_stream_write(out, vec![0])
        .unwrap_err();
    assert!(
        matches!(error, StreamError::LastOperationFailed(_)),
        "{error:?}"
    );
}

#[test]
fn a_pollable_of_a_dropped_future_does_not_follow_the_future_in_its_slot() {
    let (mut h, _requests) = host();
    let first = post(&mut h);
    let done = h.wasi_http_outgoing_handler_handle(first, None).unwrap();
    let stale = h.wasi_http_types_method_future_incoming_response_subscribe(done);
    h.wasi_io_poll_method_pollable_block(stale);
    h.wasi_http_types_resource_drop_future_incoming_response(done);

    // the next future reuses the slot and stays pending: its request waits for `finish`
    let second = post(&mut h);
    let _body = h
        .wasi_http_types_method_outgoing_request_body(second)
        .unwrap();
    let pending = h.wasi_http_outgoing_handler_handle(second, None).unwrap();
    let live = h.wasi_http_types_method_future_incoming_response_subscribe(pending);
    assert!(!h.wasi_io_poll_method_pollable_ready(live));

    assert!(
        h.wasi_io_poll_method_pollable_ready(stale),
        "a pollable of a dropped resource is ready, whatever took its slot"
    );
}

#[test]
fn the_streams_of_several_outgoing_bodies_each_write_their_own_body() {
    let (mut h, requests) = host();
    let mut bodies = Vec::new();
    for _ in 0..2 {
        let r = post(&mut h);
        let body = h.wasi_http_types_method_outgoing_request_body(r).unwrap();
        let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
        bodies.push((body, future));
    }
    // the second body is written first, through its own stream
    write(&mut h, bodies[1].0, b"second");
    write(&mut h, bodies[0].0, b"first");
    for (body, _) in &bodies {
        h.wasi_http_types_static_outgoing_body_finish(*body, None)
            .unwrap();
    }
    let mut got: Vec<Vec<u8>> = (0..2)
        .map(|_| requests.recv_timeout(Duration::from_secs(5)).unwrap().body)
        .collect();
    got.sort();
    assert_eq!(got, vec![b"first".to_vec(), b"second".to_vec()]);
}
