//! `wasi:http/outgoing-handler.handle` and the receive side (H2): future-incoming-response,
//! incoming-response, incoming-body and `http-error-code`, driven through the `Host` trait
//! with an injected `HttpClient` (the external dependency).

use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wasm2rs_wasi::wasi_http_types::{
    ErrorCode, FutureIncomingResponse, Method, OutgoingRequest, Scheme,
};
use wasm2rs_wasi::wasi_io_streams::{InputStream, StreamError};
use wasm2rs_wasi::{
    Host, HttpClient, HttpClientError, HttpRequest, HttpResponse, NetworkPolicy, RandomError,
    RandomSource, WasiHost, WasiHostBuilder,
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

/// A client that answers each request with `reply`.
struct FnClient<F>(F);

impl<F> HttpClient for FnClient<F>
where
    F: FnMut(HttpRequest) -> Result<HttpResponse, HttpClientError> + Send,
{
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        (self.0)(request)
    }
}

fn response(status: u16, headers: &[(&str, &str)], body: &'static str) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect(),
        body: Box::new(Cursor::new(body.as_bytes())),
    }
}

fn host_with(client: impl HttpClient + 'static) -> WasiHost {
    builder().http_client(client).build().unwrap()
}

/// A request for `scheme://authority/path` with no headers.
fn request(
    h: &mut WasiHost,
    scheme: Option<Scheme>,
    authority: Option<&str>,
    path: Option<&str>,
) -> OutgoingRequest {
    let headers = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(headers);
    h.wasi_http_types_method_outgoing_request_set_scheme(r, scheme)
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_authority(r, authority.map(Into::into))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_path_with_query(r, path.map(Into::into))
        .unwrap();
    r
}

fn get_request(h: &mut WasiHost, authority: &str) -> OutgoingRequest {
    request(h, Some(Scheme::HTTP), Some(authority), Some("/"))
}

/// Waits for the future and takes its result.
fn finished(
    h: &mut WasiHost,
    future: FutureIncomingResponse,
) -> Result<wasm2rs_wasi::wasi_http_types::IncomingResponse, ErrorCode> {
    let pollable = h.wasi_http_types_method_future_incoming_response_subscribe(future);
    h.wasi_io_poll_method_pollable_block(pollable);
    h.wasi_http_types_method_future_incoming_response_get(future)
        .expect("ready future has a result")
        .expect("result not yet taken")
}

/// Reads a stream to its end with `blocking-read`.
fn read_all(h: &mut WasiHost, stream: InputStream) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        match h.wasi_io_streams_method_input_stream_blocking_read(stream, 4096) {
            Ok(chunk) => out.extend(chunk),
            Err(StreamError::Closed) => return out,
            Err(e) => panic!("unexpected stream error {e:?}"),
        }
    }
}

fn v(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[test]
fn a_response_round_trips_status_headers_and_body() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let mut h = host_with(FnClient(move |req: HttpRequest| {
        recorded.lock().unwrap().push(req);
        Ok(response(
            201,
            &[("X-A", "1"), ("x-a", "2"), ("Content-Type", "text/plain")],
            "hello",
        ))
    }));
    let headers = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_method_fields_append(headers, "Accept".into(), v("*/*"))
        .unwrap();
    h.wasi_http_types_method_fields_append(headers, "accept".into(), v("text/html"))
        .unwrap();
    let r = h.wasi_http_types_constructor_outgoing_request(headers);
    h.wasi_http_types_method_outgoing_request_set_method(r, Method::Post)
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(Scheme::HTTPS))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_authority(r, Some("127.0.0.1:8080".into()))
        .unwrap();
    h.wasi_http_types_method_outgoing_request_set_path_with_query(r, Some("/p?q=1".into()))
        .unwrap();

    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let resp = finished(&mut h, future).unwrap();

    assert_eq!(h.wasi_http_types_method_incoming_response_status(resp), 201);
    let fields = h.wasi_http_types_method_incoming_response_headers(resp);
    assert_eq!(
        h.wasi_http_types_method_fields_get(fields, "x-a".into()),
        vec![v("1"), v("2")]
    );
    assert_eq!(
        h.wasi_http_types_method_fields_get(fields, "content-type".into()),
        vec![v("text/plain")]
    );
    // a received response is immutable
    assert!(
        h.wasi_http_types_method_fields_append(fields, "x-b".into(), v("1"))
            .is_err()
    );
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    assert_eq!(read_all(&mut h, stream), b"hello");
    h.wasi_io_streams_resource_drop_input_stream(stream);
    let trailers = h.wasi_http_types_static_incoming_body_finish(body);
    h.wasi_http_types_resource_drop_future_trailers(trailers);
    h.wasi_http_types_resource_drop_incoming_response(resp);
    h.wasi_http_types_resource_drop_future_incoming_response(future);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method, "POST");
    assert_eq!(seen[0].url, "https://127.0.0.1:8080/p?q=1");
    assert_eq!(
        seen[0].headers,
        vec![
            ("Accept".to_string(), v("*/*")),
            ("Accept".to_string(), v("text/html")),
        ]
    );
    assert!(seen[0].body.is_empty());
}

#[test]
fn the_request_options_timeouts_reach_the_client() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let mut h = host_with(FnClient(move |req: HttpRequest| {
        recorded.lock().unwrap().push(req);
        Ok(response(200, &[], ""))
    }));
    let options = h.wasi_http_types_constructor_request_options();
    h.wasi_http_types_method_request_options_set_connect_timeout(options, Some(1))
        .unwrap();
    h.wasi_http_types_method_request_options_set_between_bytes_timeout(options, Some(3))
        .unwrap();
    let r = get_request(&mut h, "localhost");
    let future = h
        .wasi_http_outgoing_handler_handle(r, Some(options))
        .unwrap();
    finished(&mut h, future).unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].connect_timeout, Some(1));
    assert_eq!(seen[0].first_byte_timeout, None);
    assert_eq!(seen[0].between_bytes_timeout, Some(3));
}

#[test]
fn missing_scheme_and_path_take_the_defaults() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let mut h = host_with(FnClient(move |req: HttpRequest| {
        recorded.lock().unwrap().push(req.url);
        Ok(response(200, &[], ""))
    }));
    let r = request(&mut h, None, Some("localhost"), None);
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    finished(&mut h, future).unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["https://localhost/".to_string()]
    );
}

#[test]
fn a_request_without_an_authority_is_an_invalid_uri() {
    let mut h = host_with(FnClient(|_| Ok(response(200, &[], ""))));
    let r = request(&mut h, Some(Scheme::HTTP), None, Some("/"));
    assert_eq!(
        h.wasi_http_outgoing_handler_handle(r, None).err(),
        Some(ErrorCode::HTTPRequestURIInvalid)
    );
}

#[test]
fn get_is_none_before_the_result_and_some_after() {
    let (release, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let mut h = host_with(FnClient(move |_| {
        wait.lock().unwrap().recv().unwrap();
        Ok(response(200, &[], "x"))
    }));
    let r = get_request(&mut h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let pollable = h.wasi_http_types_method_future_incoming_response_subscribe(future);

    assert!(!h.wasi_io_poll_method_pollable_ready(pollable));
    assert!(
        h.wasi_http_types_method_future_incoming_response_get(future)
            .is_none()
    );

    release.send(()).unwrap();
    h.wasi_io_poll_method_pollable_block(pollable);
    assert!(h.wasi_io_poll_method_pollable_ready(pollable));
    let got = h.wasi_http_types_method_future_incoming_response_get(future);
    assert!(matches!(got, Some(Ok(Ok(_)))));
    // the result can be taken once; the pollable stays ready
    assert!(matches!(
        h.wasi_http_types_method_future_incoming_response_get(future),
        Some(Err(()))
    ));
    assert!(h.wasi_io_poll_method_pollable_ready(pollable));
}

#[test]
fn a_client_error_is_an_internal_error_with_its_message() {
    let mut h = host_with(FnClient(|_| Err(HttpClientError::new("boom"))));
    let r = get_request(&mut h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    assert_eq!(
        finished(&mut h, future).err(),
        Some(ErrorCode::InternalError(Some("boom".to_string())))
    );
}

#[test]
fn without_a_client_handle_fails_with_an_internal_error() {
    let mut h = builder().build().unwrap();
    let r = get_request(&mut h, "localhost");
    assert!(matches!(
        h.wasi_http_outgoing_handler_handle(r, None),
        Err(ErrorCode::InternalError(_))
    ));
}

fn calls_with_policy(policy: NetworkPolicy, authority: &str) -> (bool, usize) {
    let calls = Arc::new(Mutex::new(0usize));
    let counted = Arc::clone(&calls);
    let mut h = builder()
        .network(policy)
        .http_client(FnClient(move |_| {
            *counted.lock().unwrap() += 1;
            Ok(response(200, &[], ""))
        }))
        .build()
        .unwrap();
    let r = get_request(&mut h, authority);
    let allowed = match h.wasi_http_outgoing_handler_handle(r, None) {
        Ok(future) => {
            finished(&mut h, future).unwrap();
            true
        }
        Err(e) => {
            assert_eq!(e, ErrorCode::HTTPRequestDenied);
            false
        }
    };
    let n = *calls.lock().unwrap();
    (allowed, n)
}

#[test]
fn network_policy_none_refuses_every_destination() {
    for authority in ["127.0.0.1:80", "localhost", "example.com"] {
        assert_eq!(
            calls_with_policy(NetworkPolicy::None, authority),
            (false, 0)
        );
    }
}

#[test]
fn network_policy_loopback_refuses_other_destinations() {
    for authority in [
        "127.0.0.1:8080",
        "localhost",
        "localhost:3000",
        "[::1]:9",
        "user@127.0.0.2",
    ] {
        assert_eq!(
            calls_with_policy(NetworkPolicy::Loopback, authority),
            (true, 1),
            "{authority}"
        );
    }
    for authority in [
        "example.com",
        "8.8.8.8",
        "[2001:db8::1]:443",
        "localhost.example.com",
    ] {
        assert_eq!(
            calls_with_policy(NetworkPolicy::Loopback, authority),
            (false, 0),
            "{authority}"
        );
    }
}

#[test]
fn network_policy_external_allows_any_destination() {
    for authority in ["example.com", "8.8.8.8", "127.0.0.1"] {
        assert_eq!(
            calls_with_policy(NetworkPolicy::External, authority),
            (true, 1)
        );
    }
}

#[test]
fn response_headers_that_a_guest_could_not_set_are_kept() {
    let mut h = host_with(FnClient(|_| {
        Ok(response(
            200,
            &[("Transfer-Encoding", "chunked"), ("Connection", "close")],
            "",
        ))
    }));
    let r = get_request(&mut h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let resp = finished(&mut h, future).unwrap();
    let fields = h.wasi_http_types_method_incoming_response_headers(resp);
    assert_eq!(
        h.wasi_http_types_method_fields_get(fields, "transfer-encoding".into()),
        vec![v("chunked")]
    );
}

#[test]
fn consume_and_stream_can_be_taken_once() {
    let mut h = host_with(FnClient(|_| Ok(response(200, &[], "x"))));
    let r = get_request(&mut h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let resp = finished(&mut h, future).unwrap();
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    assert!(
        h.wasi_http_types_method_incoming_response_consume(resp)
            .is_err()
    );
    h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    assert!(h.wasi_http_types_method_incoming_body_stream(body).is_err());
}

#[test]
fn read_waits_for_a_slow_body_and_poll_reports_it() {
    let (feed, source) = mpsc::channel::<Vec<u8>>();
    let source = Mutex::new(Some(source));
    let mut h = host_with(FnClient(move |_| {
        struct Chunks(mpsc::Receiver<Vec<u8>>, Vec<u8>);
        impl Read for Chunks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.1.is_empty() {
                    match self.0.recv() {
                        Ok(next) => self.1 = next,
                        Err(_) => return Ok(0),
                    }
                }
                let n = buf.len().min(self.1.len());
                buf[..n].copy_from_slice(&self.1[..n]);
                self.1.drain(..n);
                Ok(n)
            }
        }
        let rx = source.lock().unwrap().take().unwrap();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Chunks(rx, Vec::new())),
        })
    }));
    let r = get_request(&mut h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let resp = finished(&mut h, future).unwrap();
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    let pollable = h.wasi_io_streams_method_input_stream_subscribe(stream);

    // nothing yet: read does not wait, the pollable is not ready
    assert_eq!(
        h.wasi_io_streams_method_input_stream_read(stream, 16),
        Ok(vec![])
    );
    assert!(!h.wasi_io_poll_method_pollable_ready(pollable));

    feed.send(v("ab")).unwrap();
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(stream, 16),
        Ok(v("ab"))
    );
    feed.send(v("cd")).unwrap();
    drop(feed);
    assert_eq!(read_all(&mut h, stream), b"cd");
    h.wasi_io_poll_method_pollable_block(pollable);
    // at its end the stream is closed, a zero-length read included
    assert_eq!(
        h.wasi_io_streams_method_input_stream_read(stream, 0),
        Err(StreamError::Closed)
    );
}

struct Failing(bool);

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0 {
            return Err(std::io::Error::other("body broke"));
        }
        self.0 = true;
        buf[..2].copy_from_slice(b"ok");
        Ok(2)
    }
}

#[test]
fn a_body_failure_is_reported_once_with_an_http_error_code() {
    let mut h = host_with(FnClient(|_| {
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Failing(false)),
        })
    }));
    let r = get_request(&mut h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    let resp = finished(&mut h, future).unwrap();
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();

    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(stream, 16),
        Ok(v("ok"))
    );
    let err = match h.wasi_io_streams_method_input_stream_blocking_read(stream, 16) {
        Err(StreamError::LastOperationFailed(err)) => err,
        other => panic!("expected last-operation-failed, got {other:?}"),
    };
    assert!(
        h.wasi_io_error_method_error_to_debug_string(err)
            .contains("body broke")
    );
    assert!(matches!(
        h.wasi_http_types_http_error_code(err),
        Some(ErrorCode::HTTPProtocolError | ErrorCode::InternalError(_))
    ));
    assert_eq!(
        h.wasi_io_streams_method_input_stream_read(stream, 16),
        Err(StreamError::Closed)
    );
}

#[test]
fn http_error_code_is_none_for_a_failure_that_is_not_http() {
    let mut h = host_with(FnClient(|_| Ok(response(200, &[], ""))));
    // the handle of an `error` the host never made
    assert_eq!(
        h.wasi_http_types_http_error_code(wasm2rs_wasi::wasi_io_error::Error(999)),
        None
    );
}

/// An endless body that notes when it is dropped, i.e. when the reader thread has stopped.
struct Endless(Arc<AtomicBool>);

impl Read for Endless {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_micros(100));
        let n = buf.len().min(64);
        buf[..n].fill(b'z');
        Ok(n)
    }
}

impl Drop for Endless {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn endless_response(h: &mut WasiHost) -> wasm2rs_wasi::wasi_http_types::IncomingResponse {
    let r = get_request(h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    finished(h, future).unwrap()
}

fn wait_until(flag: &AtomicBool) {
    let start = Instant::now();
    while !flag.load(Ordering::SeqCst) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the reader thread did not stop"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn dropping_the_body_stops_its_reader_thread() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let slot = Mutex::new(Some(flag));
    let mut h = host_with(FnClient(move |_| {
        let flag = slot.lock().unwrap().take().unwrap();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Endless(flag)),
        })
    }));
    let resp = endless_response(&mut h);
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    // the thread is reading: the buffer fills up to its limit and the thread waits
    h.wasi_io_streams_method_input_stream_blocking_read(stream, 16)
        .unwrap();
    assert!(!stopped.load(Ordering::SeqCst));

    h.wasi_io_streams_resource_drop_input_stream(stream);
    h.wasi_http_types_resource_drop_incoming_body(body);
    wait_until(&stopped);
}

#[test]
fn finishing_the_body_stops_its_reader_thread() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let slot = Mutex::new(Some(flag));
    let mut h = host_with(FnClient(move |_| {
        let flag = slot.lock().unwrap().take().unwrap();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Endless(flag)),
        })
    }));
    let resp = endless_response(&mut h);
    let body = h
        .wasi_http_types_method_incoming_response_consume(resp)
        .unwrap();
    let stream = h.wasi_http_types_method_incoming_body_stream(body).unwrap();
    h.wasi_io_streams_method_input_stream_blocking_read(stream, 16)
        .unwrap();
    h.wasi_io_streams_resource_drop_input_stream(stream);
    let trailers = h.wasi_http_types_static_incoming_body_finish(body);
    h.wasi_http_types_resource_drop_future_trailers(trailers);
    wait_until(&stopped);
}

#[test]
fn dropping_an_unread_response_drops_its_body() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let slot = Mutex::new(Some(flag));
    let mut h = host_with(FnClient(move |_| {
        let flag = slot.lock().unwrap().take().unwrap();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Endless(flag)),
        })
    }));
    let resp = endless_response(&mut h);
    h.wasi_http_types_resource_drop_incoming_response(resp);
    wait_until(&stopped);
}

#[test]
fn dropping_the_future_discards_a_late_response() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let (release, wait) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let slot = Mutex::new(Some(flag));
    let mut h = host_with(FnClient(move |_| {
        wait.lock().unwrap().recv().unwrap();
        let flag = slot.lock().unwrap().take().unwrap();
        Ok(HttpResponse {
            status: 200,
            headers: vec![],
            body: Box::new(Endless(flag)),
        })
    }));
    let r = get_request(&mut h, "localhost");
    let future = h.wasi_http_outgoing_handler_handle(r, None).unwrap();
    h.wasi_http_types_resource_drop_future_incoming_response(future);
    release.send(()).unwrap();
    // the response arrives after the guest gave up: it is dropped, not kept
    wait_until(&stopped);
}
