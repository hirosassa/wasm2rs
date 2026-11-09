//! `examples/wasip2_serve.rs` end to end: a `proxy`-like component is served on a real TCP
//! port by the example (a real process) and answers real requests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::wasip2_typed_http_incoming::{
    echo_handle, guest, proxy_component, refusing_guest, silent_guest, streaming_guest,
};

/// The example, running; killed on drop.
struct Server {
    child: Child,
    addr: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts `cargo run --example wasip2_serve` on `core`'s component (listening on a free
/// loopback port) and waits for its `listening on` line: the build comes first.
fn serve(core: &str, tag: &str) -> Server {
    serve_with(core, tag, &[])
}

#[allow(
    clippy::panic,
    reason = "a test helper: a server that does not start fails the test"
)]
fn serve_with(core: &str, tag: &str, extra: &[&str]) -> Server {
    let dir = std::env::temp_dir().join(format!(
        "wasm2rs-serve-example-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = dir.join("component.wasm");
    std::fs::write(&wasm, proxy_component(core)).unwrap();
    // `cargo run` and the example both `exec` the next program on unix: the child is the server
    let mut child = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["run", "--quiet", "--example", "wasip2_serve", "--"])
        .arg(&wasm)
        .args(["--listen", "127.0.0.1:0"])
        .args(extra)
        .env("WASIP2_WORK_DIR", dir.join("work"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut server = Server {
        child,
        addr: String::new(),
    };
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx
        .recv_timeout(Duration::from_secs(300))
        .unwrap_or_default();
    server.addr = line
        .trim()
        .strip_prefix("listening on ")
        .unwrap_or_else(|| panic!("the server did not start (printed {line:?})"))
        .to_string();
    server
}

/// Sends `request` (raw bytes) and reads the answer until the server closes.
fn exchange(server: &Server, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(&server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    // the server may answer and close before it has read all of a request it rejects
    let _ = stream.write_all(request);
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    answer
}

/// The status, the lowercased head and the body of an answer.
#[allow(
    clippy::panic,
    reason = "a test helper: a malformed answer fails the test"
)]
fn parse(answer: &[u8]) -> (u16, String, Vec<u8>) {
    let end = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no head in {:?}", String::from_utf8_lossy(answer)));
    let head = String::from_utf8_lossy(&answer[..end]).into_owned();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (
        status,
        head.to_ascii_lowercase(),
        answer[end + 4..].to_vec(),
    )
}

fn post(path: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: example.test\r\nAccept: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn a_response_has_the_guests_status_headers_and_body_and_the_post_body_is_echoed() {
    let server = serve(&guest(&echo_handle("")), "echo");
    let answer = exchange(&server, post("/items?id=7", "hello").as_bytes());
    let (status, head, body) = parse(&answer);
    // the guest answers 200 + post (2) + http (0)
    assert_eq!(status, 202, "{head}");
    assert!(head.contains("x-path: /items?id=7"), "{head}");
    assert!(head.contains("x-auth: example.test"), "{head}");
    assert!(head.contains("x-accept: text/plain"), "{head}");
    assert!(head.contains("content-length: 5"), "{head}");
    assert!(head.contains("connection: close"), "{head}");
    assert_eq!(body, b"hello");
    // the server goes on to the next connection
    let (status, _, body) = parse(&exchange(&server, post("/again", "second").as_bytes()));
    assert_eq!((status, body), (202, b"second".to_vec()));
}

#[test]
fn a_malformed_request_is_400_and_the_server_goes_on() {
    let server = serve(&guest(&echo_handle("")), "bad");
    for bad in [
        "GET /only-two-parts\r\n\r\n",
        "GET / HTTP/9.9\r\n\r\n",
        "GET / HTTP/1.1\r\nno colon here\r\n\r\n",
        "POST / HTTP/1.1\r\nContent-Length: nope\r\n\r\n",
        "POST / HTTP/1.1\r\nContent-Length: +5\r\n\r\nhello",
    ] {
        let (status, _, _) = parse(&exchange(&server, bad.as_bytes()));
        assert_eq!(status, 400, "{bad:?}");
    }
    let (status, ..) = parse(&exchange(&server, post("/ok", "x").as_bytes()));
    assert_eq!(status, 202);
}

#[test]
fn a_body_over_the_limit_is_413_and_chunked_is_501() {
    let server = serve(&guest(&echo_handle("")), "limits");
    let huge = "POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 99999999\r\n\r\n";
    assert_eq!(parse(&exchange(&server, huge.as_bytes())).0, 413);
    let big_head = format!("GET / HTTP/1.1\r\nX-Pad: {}\r\n\r\n", "a".repeat(100_000));
    assert_eq!(parse(&exchange(&server, big_head.as_bytes())).0, 413);
    let chunked = "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
    assert_eq!(parse(&exchange(&server, chunked.as_bytes())).0, 501);
}

#[test]
fn the_server_answers_after_a_guest_trap() {
    // a DELETE (method case 4) traps; anything else is the echo
    let trap = "(call $method (local.get $req) (i32.const 100))
      (if (i32.eq (i32.load8_u (i32.const 100)) (i32.const 4)) (then unreachable))";
    let server = serve(&guest(&echo_handle(trap)), "trap");
    let delete = "DELETE / HTTP/1.1\r\nHost: h\r\nAccept: a\r\n\r\n";
    assert_eq!(parse(&exchange(&server, delete.as_bytes())).0, 500);
    let (status, _, body) = parse(&exchange(&server, post("/", "alive").as_bytes()));
    assert_eq!((status, body), (202, b"alive".to_vec()));
    assert_eq!(parse(&exchange(&server, delete.as_bytes())).0, 500);
}

#[test]
fn no_response_is_500_and_an_error_code_is_502() {
    let silent = serve(&silent_guest(), "silent");
    assert_eq!(parse(&exchange(&silent, post("/", "").as_bytes())).0, 500);
    let refusing = serve(&refusing_guest(), "refusing");
    let (status, _, body) = parse(&exchange(&refusing, post("/", "").as_bytes()));
    assert_eq!(status, 502);
    assert!(String::from_utf8_lossy(&body).contains("ConnectionRefused"));
}

#[test]
fn a_body_with_trailers_is_chunked_and_the_trailers_follow_the_last_chunk() {
    let server = serve(&streaming_guest(true), "trailers");
    let answer = exchange(&server, post("/", "").as_bytes());
    let (status, head, body) = parse(&answer);
    assert_eq!(status, 200, "{head}");
    assert!(head.contains("transfer-encoding: chunked"), "{head}");
    assert!(!head.contains("content-length"), "{head}");
    assert_eq!(body, b"6\r\naccept\r\n0\r\nx-path: accept\r\n\r\n");
}

#[test]
fn a_body_the_guest_leaves_open_is_streamed_then_cut_without_the_last_chunk() {
    let server = serve_with(&streaming_guest(false), "open", &["--body-timeout", "1"]);
    let answer = exchange(&server, post("/", "").as_bytes());
    let (status, head, body) = parse(&answer);
    assert_eq!(status, 200, "{head}");
    assert!(head.contains("transfer-encoding: chunked"), "{head}");
    // what was written arrived, and the chunked body was never terminated
    assert_eq!(body, b"6\r\naccept\r\n");
}
