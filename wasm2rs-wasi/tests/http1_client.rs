//! `Http1Client` (feature `http-client`) against a real loopback HTTP server: trailers travel in
//! both directions.
#![cfg(feature = "http-client")]
#![allow(clippy::unwrap_used, reason = "test code")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use wasm2rs_wasi::{Http1Client, HttpClient, HttpClientErrorKind, HttpRequest};

/// Accepts one connection, reads the request off it and answers with `reply`.
fn serve(reply: &'static [u8]) -> (String, JoinHandle<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = thread::spawn(move || {
        let mut stream = listener.accept().unwrap().0;
        let seen = read_request(&stream);
        stream.write_all(reply).unwrap();
        seen
    });
    (addr, handle)
}

fn line(reader: &mut impl BufRead) -> String {
    let mut s = String::new();
    reader.read_line(&mut s).unwrap();
    s.trim_end_matches(['\r', '\n']).to_string()
}

/// Header or trailer lines up to the empty line.
fn fields(reader: &mut impl BufRead) -> Vec<String> {
    std::iter::from_fn(|| Some(line(reader)).filter(|l| !l.is_empty())).collect()
}

/// What the server saw: the head lines, the decoded body and the trailer lines.
struct Seen {
    head: Vec<String>,
    body: Vec<u8>,
    trailers: Vec<String>,
}

fn read_request(stream: &TcpStream) -> Seen {
    let mut reader = BufReader::new(stream);
    let head = fields(&mut reader);
    let header = |name: &str| {
        head.iter().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    let mut body = Vec::new();
    let mut trailers = Vec::new();
    if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        loop {
            let size = usize::from_str_radix(line(&mut reader).trim(), 16).unwrap();
            if size == 0 {
                break;
            }
            let mut chunk = vec![0; size];
            reader.read_exact(&mut chunk).unwrap();
            body.extend(chunk);
            assert_eq!(line(&mut reader), "");
        }
        trailers = fields(&mut reader);
    } else if let Some(len) = header("content-length") {
        body = vec![0; len.parse().unwrap()];
        reader.read_exact(&mut body).unwrap();
    }
    Seen {
        head,
        body,
        trailers,
    }
}

fn request(method: &str, addr: &str) -> HttpRequest {
    HttpRequest {
        method: method.to_string(),
        url: format!("http://{addr}/p?q=1"),
        headers: Vec::new(),
        body: Vec::new(),
        trailers: Vec::new(),
        connect_timeout: None,
        first_byte_timeout: None,
        between_bytes_timeout: None,
    }
}

fn read_body(mut body: impl Read) -> Vec<u8> {
    let mut out = Vec::new();
    body.read_to_end(&mut out).unwrap();
    out
}

fn pair(k: &str, v: &str) -> (String, Vec<u8>) {
    (k.to_string(), v.as_bytes().to_vec())
}

#[test]
fn request_trailers_are_sent_after_a_chunked_body() {
    let (addr, server) = serve(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
    let mut req = request("POST", &addr);
    req.headers.push(pair("content-length", "999"));
    req.body = b"hello".to_vec();
    req.trailers = vec![pair("x-sum", "42"), pair("x-sum", "43")];
    let response = Http1Client::new().send(req).unwrap();
    assert_eq!(response.status, 200);
    let seen = server.join().unwrap();
    assert_eq!(seen.body, b"hello");
    assert_eq!(seen.trailers, ["x-sum: 42", "x-sum: 43"]);
    assert!(seen.head[0].starts_with("POST /p?q=1 HTTP/1.1"));
    let lower: Vec<_> = seen.head.iter().map(|l| l.to_ascii_lowercase()).collect();
    assert!(lower.iter().any(|l| l == "transfer-encoding: chunked"));
    assert!(!lower.iter().any(|l| l.starts_with("content-length")));
    assert!(lower.iter().any(|l| l == &format!("host: {addr}")));
}

#[test]
fn a_body_without_trailers_keeps_its_length() {
    let (addr, server) = serve(b"HTTP/1.1 204 No Content\r\n\r\n");
    let mut req = request("POST", &addr);
    req.body = b"abc".to_vec();
    Http1Client::new().send(req).unwrap();
    let seen = server.join().unwrap();
    assert_eq!(seen.body, b"abc");
    assert!(seen.trailers.is_empty());
}

#[test]
fn response_trailers_are_read_after_the_body() {
    let (addr, server) = serve(
        b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\ntrailer: x-digest\r\n\r\n\
              5\r\nhello\r\n6\r\n world\r\n0\r\nx-digest: abc\r\nx-digest: def\r\n\r\n",
    );
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(read_body(response.body), b"hello world");
    let trailers = (response.trailers.unwrap())();
    assert_eq!(trailers, [pair("x-digest", "abc"), pair("x-digest", "def")]);
}

#[test]
fn a_chunked_response_without_trailers_has_none() {
    let (addr, server) =
        serve(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n");
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    assert_eq!(read_body(response.body), b"abc");
    assert!(response.trailers.is_none_or(|t| t().is_empty()));
}

#[test]
fn content_length_and_close_delimited_bodies_are_read() {
    let (addr, server) = serve(b"HTTP/1.1 200 OK\r\nx-a: 1\r\ncontent-length: 3\r\n\r\nabcEXTRA");
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    assert_eq!(
        response.headers,
        [pair("x-a", "1"), pair("content-length", "3")]
    );
    assert_eq!(read_body(response.body), b"abc");

    let (addr, server) = serve(b"HTTP/1.0 200 OK\r\n\r\nuntil close");
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    assert_eq!(read_body(response.body), b"until close");
}

#[test]
fn head_responses_have_no_body_and_interim_responses_are_skipped() {
    let (addr, server) =
        serve(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\n");
    let response = Http1Client::new().send(request("HEAD", &addr)).unwrap();
    server.join().unwrap();
    assert_eq!(response.status, 200);
    assert!(read_body(response.body).is_empty());
}

#[test]
fn a_truncated_chunked_body_is_an_error_not_an_end() {
    let (addr, server) = serve(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhel");
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    let mut reader = response.body;
    assert!(reader.read_to_end(&mut Vec::new()).is_err());
}

#[test]
fn https_is_refused_and_connection_refused_is_classified() {
    let mut req = request("GET", "127.0.0.1:1");
    req.url = "https://127.0.0.1:1/".into();
    let err = Http1Client::new().send(req).unwrap_err();
    assert!(err.to_string().contains("https"), "{err}");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    drop(listener);
    let err = Http1Client::new().send(request("GET", &addr)).unwrap_err();
    assert_eq!(err.kind(), &HttpClientErrorKind::ConnectionRefused);
}

#[test]
fn a_head_request_with_a_body_is_refused() {
    let mut req = request("HEAD", "127.0.0.1:1");
    req.body = b"x".to_vec();
    assert!(Http1Client::new().send(req).is_err());
}

#[test]
fn non_utf8_header_and_reason_bytes_are_kept() {
    let (addr, server) =
        serve(b"HTTP/1.1 200 Gr\xfc\xdf\r\nx-name: caf\xe9\r\ncontent-length: 0\r\n\r\n");
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    assert_eq!(
        response.headers[0],
        ("x-name".to_string(), b"caf\xe9".to_vec())
    );
}

#[test]
fn the_guests_content_length_never_disagrees_with_the_body() {
    let (addr, server) = serve(b"HTTP/1.1 204 No Content\r\n\r\n");
    let mut req = request("POST", &addr);
    req.headers.push(pair("content-length", "3"));
    req.body = b"0123456789".to_vec();
    Http1Client::new().send(req).unwrap();
    let seen = server.join().unwrap();
    assert_eq!(seen.body, b"0123456789");
}

#[test]
fn repeated_framing_headers_are_combined() {
    let (addr, server) = serve(
        b"HTTP/1.1 200 OK\r\ntransfer-encoding: gzip\r\ntransfer-encoding: chunked\r\n\r\n\
          3\r\nabc\r\n0\r\nx-t: 1\r\n\r\n",
    );
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    let trailers = response.trailers.unwrap();
    assert_eq!(read_body(response.body), b"abc");
    assert_eq!(trailers(), [pair("x-t", "1")]);

    let (addr, server) = serve(b"HTTP/1.1 200 OK\r\ncontent-length: 3, 3\r\n\r\nabc");
    let response = Http1Client::new().send(request("GET", &addr)).unwrap();
    server.join().unwrap();
    assert_eq!(read_body(response.body), b"abc");
}

#[test]
fn userinfo_and_an_empty_port_in_the_url() {
    let (addr, server) = serve(b"HTTP/1.1 204 No Content\r\n\r\n");
    let mut req = request("GET", &addr);
    req.url = format!("http://user:pw@{addr}/");
    Http1Client::new().send(req).unwrap();
    let seen = server.join().unwrap();
    assert!(seen.head.iter().any(|l| l == &format!("host: {addr}")));
}
