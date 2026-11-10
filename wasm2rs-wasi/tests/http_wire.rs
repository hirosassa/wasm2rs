//! What leaves and enters `wasi:http` at the `HttpClient` boundary: the method and scheme the
//! client is told, and the `error-code` a failed response body maps to. The client is the
//! external dependency and is injected.

use std::io::Read;
use std::sync::{Arc, Mutex};

use wasm2rs_wasi::wasi_http_types::{
    ErrorCode, FutureIncomingResponse, IncomingResponse, Method, Scheme,
};
use wasm2rs_wasi::wasi_io_streams::StreamError;
use wasm2rs_wasi::{
    Host, HttpClient, HttpClientError, HttpRequest, HttpResponse, RandomError, RandomSource,
    WasiHost, WasiHostBuilder,
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

type TrailerSource = Box<dyn FnOnce() -> Vec<(String, Vec<u8>)> + Send>;

struct FnClient<F>(F);

impl<F> HttpClient for FnClient<F>
where
    F: FnMut(HttpRequest) -> Result<HttpResponse, HttpClientError> + Send,
{
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        (self.0)(request)
    }
}

fn empty_response() -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: vec![],
        body: Box::new(std::io::empty()),
        trailers: None,
    }
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

/// Sends `method` over `scheme` to `localhost` and returns what the client was handed.
fn sent(method: Method, scheme: Scheme) -> HttpRequest {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let mut h = builder()
        .http_client(FnClient(move |req: HttpRequest| {
            recorded.lock().unwrap().push(req);
            Ok(empty_response())
        }))
        .build()
        .unwrap();
    let headers = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(headers);
    h.wasi_http_types_method_outgoing_request_set_method(r, method)
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(scheme))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_authority(r, Some("localhost".into()))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_path_with_query(r, Some("/x".into()))
        .unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    finished(&mut h, future).unwrap();
    let mut seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    seen.remove(0)
}

#[test]
fn every_method_reaches_the_client_under_its_own_name() {
    for (method, name) in [
        (Method::Get, "GET"),
        (Method::Head, "HEAD"),
        (Method::Post, "POST"),
        (Method::Put, "PUT"),
        (Method::Delete, "DELETE"),
        (Method::Connect, "CONNECT"),
        (Method::Options, "OPTIONS"),
        (Method::Trace, "TRACE"),
        (Method::Patch, "PATCH"),
        (Method::Other("PURGE".into()), "PURGE"),
    ] {
        assert_eq!(sent(method, Scheme::HTTP).method, name);
    }
}

#[test]
fn the_scheme_is_part_of_the_url_the_client_gets() {
    for (scheme, url) in [
        (Scheme::HTTP, "http://localhost/x"),
        (Scheme::HTTPS, "https://localhost/x"),
        (Scheme::Other("ftp".into()), "ftp://localhost/x"),
    ] {
        assert_eq!(sent(Method::Get, scheme).url, url);
    }
}

/// A body of one chunk, then an error of `kind`.
struct FailsWith(std::io::ErrorKind, bool);

impl Read for FailsWith {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.1 {
            return Err(std::io::Error::new(self.0, "wire failure"));
        }
        self.1 = true;
        buf[..2].copy_from_slice(b"ok");
        Ok(2)
    }
}

/// The `error-code` of the failure that ends a response body, read through the stream.
fn code_of_failing_body(body: Box<dyn Read + Send>) -> Option<ErrorCode> {
    let mut body = Some(body);
    let mut h = builder()
        .http_client(FnClient(move |_| {
            Ok(HttpResponse {
                status: 200,
                headers: vec![],
                body: body.take().unwrap(),
                trailers: None,
            })
        }))
        .build()
        .unwrap();
    let headers = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(headers);
    h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(Scheme::HTTP))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_authority(r, Some("localhost".into()))
        .unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let resp = finished(&mut h, future).unwrap();
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    loop {
        match h.wasi_io_streams_method_input_stream_blocking_read(stream, 16) {
            Ok(_) => {}
            Err(StreamError::LastOperationFailed(err)) => {
                return h.wasi_http_types_http_error_code(err);
            }
            Err(StreamError::Closed) => return None,
        }
    }
}

#[test]
fn a_failing_body_maps_its_io_error_to_the_matching_error_code() {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionReset, TimedOut, UnexpectedEof};
    for (kind, expected) in [
        (TimedOut, ErrorCode::ConnectionReadTimeout),
        (UnexpectedEof, ErrorCode::HTTPResponseIncomplete),
        (ConnectionReset, ErrorCode::ConnectionTerminated),
        (ConnectionAborted, ErrorCode::ConnectionTerminated),
    ] {
        assert_eq!(
            code_of_failing_body(Box::new(FailsWith(kind, false))),
            Some(expected),
            "{kind:?}"
        );
    }
    // anything else keeps the client's message
    assert_eq!(
        code_of_failing_body(Box::new(FailsWith(std::io::ErrorKind::Other, false))),
        Some(ErrorCode::InternalError(Some("wire failure".into())))
    );
}

#[test]
fn a_client_that_panics_reading_trailers_fails_the_body_instead_of_hanging() {
    struct Empty;
    impl Read for Empty {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }
    let mut trailers: Option<TrailerSource> = Some(Box::new(|| panic!("trailer source broke")));
    let mut h = builder()
        .http_client(FnClient(move |_| {
            Ok(HttpResponse {
                status: 200,
                headers: vec![],
                body: Box::new(Empty),
                trailers: trailers.take(),
            })
        }))
        .build()
        .unwrap();
    let headers = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(headers);
    h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(Scheme::HTTP))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_authority(r, Some("localhost".into()))
        .unwrap();
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let resp = finished(&mut h, future).unwrap();
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    let err = match h.wasi_io_streams_method_input_stream_blocking_read(stream, 16) {
        Err(StreamError::LastOperationFailed(err)) => err,
        other => panic!("expected a failed read, got {other:?}"),
    };
    assert!(
        h.wasi_io_error_method_error_to_debug_string(err)
            .contains("panicked reading trailers")
    );
}
