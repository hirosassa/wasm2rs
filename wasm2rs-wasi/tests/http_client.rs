//! `UreqClient` (feature `http-client`) against a real loopback HTTP server.
#![cfg(feature = "http-client")]
#![allow(clippy::unwrap_used, reason = "test code")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use wasm2rs_wasi::{HttpClient, HttpRequest, UreqClient};

/// Serves one connection with `handler`; returns the address and the handler's result.
fn serve<T: Send + 'static>(
    handler: impl FnOnce(TcpStream) -> T + Send + 'static,
) -> (String, JoinHandle<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = thread::spawn(move || handler(listener.accept().unwrap().0));
    (addr, handle)
}

/// Reads the request head, and the body its `content-length` announces.
fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        buf.push(byte[0]);
    }
    let head = String::from_utf8(buf).unwrap();
    let len = head
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).unwrap();
    (head, body)
}

fn request(method: &str, addr: &str, path: &str) -> HttpRequest {
    HttpRequest {
        method: method.to_string(),
        url: format!("http://{addr}{path}"),
        headers: Vec::new(),
        body: Vec::new(),
        connect_timeout: None,
        first_byte_timeout: None,
        between_bytes_timeout: None,
    }
}

fn read_body(response: wasm2rs_wasi::HttpResponse) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut body = response.body;
    body.read_to_end(&mut out)?;
    Ok(out)
}

#[test]
fn get_round_trip_with_headers() {
    let (addr, server) = serve(|mut s| {
        let (head, _) = read_request(&mut s);
        s.write_all(
            b"HTTP/1.1 200 OK\r\nx-reply: a\r\nx-reply: b\r\ncontent-length: 5\r\n\r\nhello",
        )
        .unwrap();
        head
    });
    let mut req = request("GET", &addr, "/path?q=1");
    req.headers.push(("x-test".into(), b"v1".to_vec()));
    let response = UreqClient::new().send(req).unwrap();
    assert_eq!(response.status, 200);
    let replies: Vec<_> = response
        .headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("x-reply"))
        .map(|(_, v)| v.clone())
        .collect();
    assert_eq!(replies, vec![b"a".to_vec(), b"b".to_vec()]);
    assert_eq!(read_body(response).unwrap(), b"hello");
    let head = server.join().unwrap().to_ascii_lowercase();
    assert!(head.starts_with("get /path?q=1 http/1.1"), "{head}");
    assert!(head.contains("x-test: v1"), "{head}");
}

#[test]
fn post_sends_the_body() {
    let (addr, server) = serve(|mut s| {
        let (head, body) = read_request(&mut s);
        s.write_all(b"HTTP/1.1 201 Created\r\ncontent-length: 0\r\n\r\n")
            .unwrap();
        (head, body)
    });
    let mut req = request("POST", &addr, "/submit");
    req.body = b"payload".to_vec();
    let response = UreqClient::new().send(req).unwrap();
    assert_eq!(response.status, 201);
    let (head, body) = server.join().unwrap();
    assert!(head.starts_with("POST /submit"), "{head}");
    assert_eq!(body, b"payload");
}

#[test]
fn error_statuses_are_responses() {
    for status in [404u16, 503] {
        let (addr, server) = serve(move |mut s| {
            read_request(&mut s);
            let msg = format!("HTTP/1.1 {status} X\r\ncontent-length: 4\r\n\r\nnope");
            s.write_all(msg.as_bytes()).unwrap();
        });
        let response = UreqClient::new().send(request("GET", &addr, "/")).unwrap();
        assert_eq!(response.status, status);
        assert_eq!(read_body(response).unwrap(), b"nope");
        server.join().unwrap();
    }
}

#[test]
fn redirects_are_not_followed() {
    let (addr, server) = serve(|mut s| {
        read_request(&mut s);
        s.write_all(
            b"HTTP/1.1 302 Found\r\nlocation: http://127.0.0.1:1/\r\ncontent-length: 0\r\n\r\n",
        )
        .unwrap();
    });
    let response = UreqClient::new().send(request("GET", &addr, "/")).unwrap();
    assert_eq!(response.status, 302);
    let location = response
        .headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("location"));
    assert_eq!(location.unwrap().1, b"http://127.0.0.1:1/");
    server.join().unwrap();
}

#[test]
fn connection_refused_is_a_client_error() {
    let addr = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().to_string()
    };
    assert!(UreqClient::new().send(request("GET", &addr, "/")).is_err());
}

#[test]
fn invalid_method_or_header_is_a_client_error() {
    let mut req = request("BAD METHOD", "127.0.0.1:1", "/");
    assert!(UreqClient::new().send(req.clone()).is_err());
    req.method = "GET".into();
    req.headers.push(("bad name".into(), b"x".to_vec()));
    assert!(UreqClient::new().send(req).is_err());
}

#[test]
fn first_byte_timeout_fires() {
    let (addr, server) = serve(|mut s| {
        read_request(&mut s);
        thread::sleep(Duration::from_millis(1500));
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
    });
    let mut req = request("GET", &addr, "/");
    req.first_byte_timeout = Some(200_000_000);
    let started = Instant::now();
    assert!(UreqClient::new().send(req).is_err());
    assert!(started.elapsed() < Duration::from_millis(1200));
    server.join().unwrap();
}

#[test]
fn between_bytes_timeout_fires_on_a_stalled_body() {
    let (addr, server) = serve(|mut s| {
        read_request(&mut s);
        s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\nabc")
            .unwrap();
        thread::sleep(Duration::from_millis(1500));
    });
    let mut req = request("GET", &addr, "/");
    req.between_bytes_timeout = Some(200_000_000);
    let response = UreqClient::new().send(req).unwrap();
    let started = Instant::now();
    assert!(read_body(response).is_err());
    assert!(started.elapsed() < Duration::from_millis(1200));
    server.join().unwrap();
}

#[test]
fn connect_timeout_is_applied() {
    // 10.255.255.1 is unroutable on most networks; if the sandbox refuses at once that is an
    // error too, so only the bound on the time is asserted.
    let mut req = request("GET", "10.255.255.1:81", "/");
    req.connect_timeout = Some(300_000_000);
    let started = Instant::now();
    assert!(UreqClient::new().send(req).is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn extension_methods_are_sent() {
    let (addr, server) = serve(|mut s| {
        let (head, _) = read_request(&mut s);
        s.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        head
    });
    let response = UreqClient::new()
        .send(request("PURGE", &addr, "/x"))
        .unwrap();
    assert_eq!(response.status, 204);
    assert!(server.join().unwrap().starts_with("PURGE /x HTTP/1.1"));
}

#[test]
fn a_body_on_head_or_connect_is_refused_before_sending() {
    for method in ["HEAD", "CONNECT"] {
        // nothing listens: a refusal must come before the connection is tried
        let mut req = request(method, "127.0.0.1:1", "/");
        req.body = b"x".to_vec();
        let error = UreqClient::new().send(req).expect_err("refused");
        assert!(error.to_string().contains(method), "{error}");
    }
}

#[test]
fn an_empty_body_on_head_is_fine() {
    let (addr, server) = serve(|mut s| {
        let (head, _) = read_request(&mut s);
        s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
            .unwrap();
        head
    });
    let response = UreqClient::new().send(request("HEAD", &addr, "/")).unwrap();
    assert_eq!(response.status, 200);
    assert!(server.join().unwrap().starts_with("HEAD / HTTP/1.1"));
}
