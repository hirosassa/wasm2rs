//! `examples/wasip2_serve.rs` end to end: a `proxy`-like component is served on a real TCP
//! port by the example (a real process) and answers real requests.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
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

fn serve_with(core: &str, tag: &str, extra: &[&str]) -> Server {
    let dir = work_dir(tag);
    let wasm = dir.join("component.wasm");
    std::fs::write(&wasm, proxy_component(core)).unwrap();
    serve_component(&wasm, &dir, extra)
}

fn work_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wasm2rs-serve-example-{}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serves the component file `wasm`, building in `dir`.
#[allow(
    clippy::panic,
    reason = "a test helper: a server that does not start fails the test"
)]
fn serve_component(wasm: &Path, dir: &Path, extra: &[&str]) -> Server {
    // `cargo run` and the example both `exec` the next program on unix: the child is the server
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["run", "--quiet", "--example", "wasip2_serve", "--"])
        .arg(wasm)
        .args(["--listen", "127.0.0.1:0"])
        .args(extra)
        .env("WASIP2_WORK_DIR", dir.join("work"))
        .stdout(Stdio::piped());
    crate::common::share_example_build(&mut cmd);
    let mut child = cmd.spawn().unwrap();
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
/// A connection to `server` that gives up reading after 30 s rather than hanging the test.
fn connect(server: &Server) -> TcpStream {
    let stream = TcpStream::connect(&server.addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    stream
}

fn exchange(server: &Server, request: &[u8]) -> Vec<u8> {
    let mut stream = connect(server);
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

#[test]
fn an_http10_client_gets_a_streamed_body_without_chunk_framing() {
    let server = serve(&streaming_guest(true), "http10");
    let request = "POST / HTTP/1.0\r\nContent-Length: 0\r\n\r\n";
    let (status, head, body) = parse(&exchange(&server, request.as_bytes()));
    assert_eq!(status, 200, "{head}");
    assert!(!head.contains("transfer-encoding"), "{head}");
    // delimited by the close; the trailers are dropped
    assert_eq!(body, b"accept");
}

#[test]
fn requests_the_parser_must_refuse_are_400_501_or_413_and_the_server_goes_on() {
    let server = serve(&guest(&echo_handle("")), "refuse");
    for (request, expected) in [
        // method is a token
        ("G@T / HTTP/1.1\r\n\r\n".to_string(), 400),
        // only origin-form (or `*`) targets: the example's limit (RFC 9112 3.2.2 would have a
        // server accept absolute-form too)
        ("GET http://example.test/ HTTP/1.1\r\n\r\n".to_string(), 400),
        ("GET relative HTTP/1.1\r\n\r\n".to_string(), 400),
        // obsolete line folding and a space before the colon are not header names
        (
            "GET / HTTP/1.1\r\nX-A: 1\r\n folded\r\n\r\n".to_string(),
            400,
        ),
        ("GET / HTTP/1.1\r\nX-A : 1\r\n\r\n".to_string(), 400),
        // two different lengths are a smuggling vector
        (
            "POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab".to_string(),
            400,
        ),
        // transfer-encoding in any case or value
        (
            "POST / HTTP/1.1\r\ntransfer-ENCODING: identity\r\n\r\n".to_string(),
            501,
        ),
        // exactly at the head limit's far side
        (
            format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(70_000)),
            413,
        ),
        // a length one past the body limit (1 MiB)
        (
            format!(
                "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                1024 * 1024 + 1
            ),
            413,
        ),
    ] {
        let (status, ..) = parse(&exchange(&server, request.as_bytes()));
        assert_eq!(status, expected, "{:?}", &request[..request.len().min(80)]);
    }
    // a head that is not UTF-8
    let (status, ..) = parse(&exchange(&server, b"GET / HTTP/1.1\r\nX: \xff\xfe\r\n\r\n"));
    assert_eq!(status, 400);
    assert_eq!(
        parse(&exchange(&server, post("/ok", "x").as_bytes())).0,
        202
    );
}

#[test]
fn a_request_cut_short_is_400_not_a_hang() {
    let server = serve(&guest(&echo_handle("")), "cut");
    for partial in [
        // closes inside the head
        &b"GET / HTTP/1.1\r\nHost: h\r\n"[..],
        // closes inside a body that announced 10 bytes
        &b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc"[..],
    ] {
        let mut stream = connect(&server);
        stream.write_all(partial).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).unwrap();
        assert_eq!(parse(&answer).0, 400, "{partial:?}");
    }
}

#[test]
fn a_client_that_stalls_inside_the_head_is_408() {
    let server = serve(&guest(&echo_handle("")), "stall");
    let mut stream = connect(&server);
    stream.write_all(b"GET / HTTP/1.1\r\nHost: h\r\n").unwrap();
    // no more bytes, and no close: the server gives up on its own after its request time
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    assert_eq!(parse(&answer).0, 408);
    assert_eq!(
        parse(&exchange(&server, post("/ok", "x").as_bytes())).0,
        202
    );
}

#[test]
fn accepted_oddities_are_parsed_as_the_rfc_allows() {
    let server = serve(&guest(&echo_handle("")), "accepted");
    // HEAD: the head of the answer to a GET (its content-length), without the body
    let head = "HEAD /h HTTP/1.1\r\nHost: h\r\nAccept: a\r\nContent-Length: 5\r\n\r\nhello";
    let (status, head, body) = parse(&exchange(&server, head.as_bytes()));
    assert_eq!(status, 201, "{head}"); // 200 + head (1)
    assert!(head.contains("content-length: 5"), "{head}");
    assert!(body.is_empty());
    // the same length twice is one length; bytes past it (pipelining) are not the body
    let twice = "POST / HTTP/1.1\r\nHost: h\r\nAccept: a\r\nContent-Length: 5\r\ncontent-length: 5\r\n\r\nhelloGET /next HTTP/1.1\r\n\r\n";
    let (status, _, body) = parse(&exchange(&server, twice.as_bytes()));
    assert_eq!((status, body), (202, b"hello".to_vec()));
    // values are trimmed of spaces and tabs; `OPTIONS *` is asterisk-form
    let star = "OPTIONS * HTTP/1.1\r\nHost: \t spaced.test \t\r\nAccept: a\r\n\r\n";
    let (status, head, _) = parse(&exchange(&server, star.as_bytes()));
    assert_eq!(status, 206, "{head}"); // 200 + options (6)
    assert!(head.contains("x-path: *"), "{head}");
    assert!(head.contains("x-auth: spaced.test\r\n"), "{head}");
    // an unknown method is `other`
    let purge = "PURGE /p HTTP/1.1\r\nHost: h\r\nAccept: a\r\n\r\n";
    assert_eq!(parse(&exchange(&server, purge.as_bytes())).0, 209); // 200 + other (9)
    // a Content-Length of leading zeros is still its number
    let zeros = "POST / HTTP/1.1\r\nHost: h\r\nAccept: a\r\nContent-Length: 0005\r\n\r\nhello";
    assert_eq!(parse(&exchange(&server, zeros.as_bytes())).2, b"hello");
}

#[test]
fn the_body_limit_is_inclusive_and_the_head_limit_holds_either_side() {
    let server = serve(&guest(&echo_handle("")), "boundary");
    // a length of exactly 1 MiB is accepted (the body is cut short, so 400, not 413)
    let mut stream = connect(&server);
    stream
        .write_all(b"POST / HTTP/1.1\r\nContent-Length: 1048576\r\n\r\nabc")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    assert_eq!(parse(&answer).0, 400);
    // a head well under 64 KiB is served, one well over is 413
    let head = |pad: usize| {
        format!(
            "GET / HTTP/1.1\r\nHost: h\r\nAccept: a\r\nX: {}\r\n\r\n",
            "a".repeat(pad)
        )
    };
    assert_eq!(parse(&exchange(&server, head(60_000).as_bytes())).0, 200);
    assert_eq!(parse(&exchange(&server, head(70_000).as_bytes())).0, 413);
}

/// A component built by a real toolchain, not by this repository's tests: `wasm32-wasip2` Rust
/// (the `wasi` crate) exporting `wasi:http/incoming-handler@0.2.12`, which answers `200` with a
/// header, a body naming the request path, and a trailer. The file is not in the repository:
/// set `WASM2RS_HTTP_HELLO_COMPONENT` to it and run with `--ignored` (see
/// `docs/component-toolchain-survey.md`, "Reproduction").
#[test]
#[ignore = "needs WASM2RS_HTTP_HELLO_COMPONENT (a wasm32-wasip2 http handler component)"]
fn a_real_toolchain_http_handler_answers_with_its_header_body_and_trailer() {
    let path = std::env::var_os("WASM2RS_HTTP_HELLO_COMPONENT")
        .expect("WASM2RS_HTTP_HELLO_COMPONENT names the component");
    let server = serve_component(Path::new(&path), &work_dir("real-handler"), &[]);
    let request = "GET /foo?x=1 HTTP/1.1\r\nHost: example.test\r\n\r\n";
    let (status, head, body) = parse(&exchange(&server, request.as_bytes()));
    assert_eq!(status, 200, "{head}");
    assert!(head.contains("x-handler: rust-wasip2"), "{head}");
    assert!(head.contains("transfer-encoding: chunked"), "{head}");
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("hello from rust, path=/foo?x=1"), "{body}");
    // the trailer follows the last chunk
    assert!(body.contains("0\r\nx-trailer: done\r\n\r\n"), "{body}");
}
