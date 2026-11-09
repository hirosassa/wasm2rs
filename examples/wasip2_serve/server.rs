// The server half of `examples/wasip2_serve.rs`, appended to the generated module and glue
// (it uses their `Instance`, `Glue` and `Exports`). Not a module of the crate: the example
// includes it as text.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};
use wasm2rs_wasi::wasi_http_types::{ErrorCode, Method, Scheme};
use wasm2rs_wasi::{IncomingRequestParts, ServedResponse, WasiHost};

/// Request line and headers together.
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 1024 * 1024;
/// A client has this long, in all, to send its request (the server is serial); a rejected
/// request's rest is drained for at most `DRAIN_TIME`.
const REQUEST_TIME: Duration = Duration::from_secs(5);
const DRAIN_TIME: Duration = Duration::from_secs(1);
/// A response body the guest leaves open is cut after this long without a byte (`--body-timeout`).
const BODY_IDLE: Duration = Duration::from_secs(30);

/// A rejected request: the status and the reason sent back.
struct Reject(u16, &'static str);

fn new_instance() -> Instance<Glue<WasiHost>> {
    Instance::new(Glue(WasiHost::new(Vec::new())))
}

/// One read that gives up (408) when `deadline` has passed, however slowly the bytes come.
fn read_some(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> Result<usize, Reject> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(Reject(408, "request timed out"));
    }
    let _ = stream.set_read_timeout(Some(left));
    match stream.read(buf) {
        Ok(n) => Ok(n),
        Err(e) if is_timeout(&e) => Err(Reject(408, "request timed out")),
        Err(_) => Err(Reject(400, "read error")),
    }
}

/// Reads until the blank line ending the head; returns the head (without it) and the body
/// bytes that came with it.
fn read_head(stream: &mut TcpStream, deadline: Instant) -> Result<(Vec<u8>, Vec<u8>), Reject> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            buf.truncate(end);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            return Err(Reject(413, "request head too large"));
        }
        match read_some(stream, &mut chunk, deadline)? {
            0 => return Err(Reject(400, "connection closed inside the request head")),
            n => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn parse_method(s: &str) -> Method {
    match s {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "DELETE" => Method::Delete,
        "CONNECT" => Method::Connect,
        "OPTIONS" => Method::Options,
        "TRACE" => Method::Trace,
        "PATCH" => Method::Patch,
        other => Method::Other(other.to_string()),
    }
}

/// Reads one request: HTTP/1.x, `Content-Length` bodies only.
/// The flag tells an HTTP/1.0 request (its answer cannot be chunked).
fn read_request(
    stream: &mut TcpStream,
    deadline: Instant,
) -> Result<(IncomingRequestParts, bool), Reject> {
    let (head, mut body) = read_head(stream, deadline)?;
    let head = std::str::from_utf8(&head).map_err(|_| Reject(400, "head is not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Reject(400, "malformed request line"));
    };
    if !is_token(method) {
        return Err(Reject(400, "malformed method"));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(Reject(400, "unsupported HTTP version"));
    }
    if !(target.starts_with('/') || target == "*") {
        return Err(Reject(
            400,
            "only origin-form request targets are supported",
        ));
    }
    let mut headers: Vec<(String, Vec<u8>)> = Vec::new();
    let mut length: Option<usize> = None;
    let mut authority = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(Reject(400, "malformed header line"));
        };
        // A name must be a token with nothing before the colon (this also rejects obsolete
        // line folding, which starts with whitespace).
        if !is_token(name) {
            return Err(Reject(400, "malformed header name"));
        }
        let value = value.trim_matches(|c| c == ' ' || c == '\t');
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(Reject(501, "Transfer-Encoding is not supported"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            // digits only: `parse` alone would take a leading `+`
            let n: usize = Some(value)
                .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|v| v.parse().ok())
                .ok_or(Reject(400, "malformed Content-Length"))?;
            if length.is_some_and(|l| l != n) {
                return Err(Reject(400, "conflicting Content-Length"));
            }
            length = Some(n);
        }
        if name.eq_ignore_ascii_case("host") {
            authority = Some(value.to_string());
        }
        headers.push((name.to_string(), value.as_bytes().to_vec()));
    }
    let length = length.unwrap_or(0);
    if length > MAX_BODY {
        return Err(Reject(413, "request body too large"));
    }
    if body.len() > length {
        // pipelined bytes: one request per connection
        body.truncate(length);
    }
    let mut chunk = [0u8; 4096];
    while body.len() < length {
        let want = (length - body.len()).min(chunk.len());
        match read_some(stream, &mut chunk[..want], deadline)? {
            0 => return Err(Reject(400, "connection closed inside the request body")),
            n => body.extend_from_slice(&chunk[..n]),
        }
    }
    Ok((
        IncomingRequestParts {
        method: parse_method(method),
        path_with_query: Some(target.to_string()),
        scheme: Some(Scheme::HTTP),
        authority,
        headers,
        body,
        trailers: None,
        },
        version == "HTTP/1.0",
    ))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Content Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        _ => "",
    }
}

/// How the body of an answer is delimited.
enum Framing {
    /// `content-length` of the known body.
    Length(usize),
    /// `transfer-encoding: chunked`, for a body still being written or with trailers.
    Chunked,
}

/// The status line and headers of an answer, ending with the blank line. The guest's own
/// `Content-Length`, `Connection` and `Transfer-Encoding` are replaced by ours, and a
/// `content-length` is left out where there is none (1xx and 204, RFC 9110 8.6).
fn head(status: u16, headers: &[(String, Vec<u8>)], framing: Option<Framing>) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} {}\r\n", reason(status)).into_bytes();
    for (name, value) in headers {
        if ["content-length", "connection", "transfer-encoding"]
            .iter()
            .any(|h| name.eq_ignore_ascii_case(h))
        {
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value);
        out.extend_from_slice(b"\r\n");
    }
    if status >= 200 && status != 204 {
        match framing {
            Some(Framing::Length(n)) => {
                out.extend_from_slice(format!("content-length: {n}\r\n").as_bytes());
            }
            Some(Framing::Chunked) => out.extend_from_slice(b"transfer-encoding: chunked\r\n"),
            None => {}
        }
    }
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out
}

/// Whether an answer to this status (and request method) carries no body.
fn bodyless(status: u16, head_only: bool) -> bool {
    head_only || status < 200 || matches!(status, 204 | 304)
}

/// Writes an answer whose body is all in hand and closes the connection (always
/// `Connection: close`).
fn respond_bytes(
    stream: &mut TcpStream,
    head_only: bool,
    status: u16,
    headers: &[(String, Vec<u8>)],
    body: &[u8],
) {
    let mut out = head(status, headers, Some(Framing::Length(body.len())));
    if !bodyless(status, head_only) {
        out.extend_from_slice(body);
    }
    // the peer may be gone: nothing to do about it
    let _ = stream.write_all(&out);
    let _ = stream.flush();
}

/// Sends the guest's answer. A body the guest finished (and without trailers) has a known
/// length and goes with a `content-length`; otherwise it is streamed as chunks while the guest
/// writes it, with the trailers after the last one. A body that fails or stalls for `idle`
/// ends the connection without the closing chunk, so the client sees it truncated.
fn respond(
    stream: &mut TcpStream,
    head_only: bool,
    http10: bool,
    mut response: ServedResponse,
    idle: Duration,
) {
    let status = response.status;
    let known = response.body.finished_len().filter(|_| response.body.trailers().is_none());
    if let Some(len) = known {
        let mut body = Vec::with_capacity(len);
        if response.body.read_to_end(&mut body).is_err() {
            body.clear();
        }
        respond_bytes(stream, head_only, status, &response.headers, &body);
        return;
    }
    if bodyless(status, head_only) {
        let _ = stream.write_all(&head(status, &response.headers, None));
        let _ = stream.flush();
        return;
    }
    response.body.set_idle_timeout(Some(idle));
    // HTTP/1.0 has no chunked coding: the body runs to the close (and drops the trailers)
    let framing = (!http10).then_some(Framing::Chunked);
    if stream
        .write_all(&head(status, &response.headers, framing))
        .is_err()
    {
        return;
    }
    let mut chunk = [0u8; 8192];
    loop {
        match response.body.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let mut out = Vec::new();
                if http10 {
                    out.extend_from_slice(&chunk[..n]);
                } else {
                    out.extend_from_slice(format!("{n:x}\r\n").as_bytes());
                    out.extend_from_slice(&chunk[..n]);
                    out.extend_from_slice(b"\r\n");
                }
                if stream.write_all(&out).is_err() {
                    return;
                }
                let _ = stream.flush();
            }
            // not terminated: the truncation is the signal
            Err(_) => return,
        }
    }
    if http10 {
        let _ = stream.flush();
        return;
    }
    let mut end = b"0\r\n".to_vec();
    for (name, value) in response.body.trailers().unwrap_or_default() {
        end.extend_from_slice(name.as_bytes());
        end.extend_from_slice(b": ");
        end.extend_from_slice(&value);
        end.extend_from_slice(b"\r\n");
    }
    end.extend_from_slice(b"\r\n");
    let _ = stream.write_all(&end);
    let _ = stream.flush();
}

fn respond_text(stream: &mut TcpStream, status: u16, text: &str) {
    let headers = [("content-type".to_string(), b"text/plain".to_vec())];
    respond_bytes(stream, false, status, &headers, format!("{text}\n").as_bytes());
}

/// What came of calling the guest.
enum Served {
    Response(ServedResponse),
    NoResponse,
    Error(ErrorCode),
    /// The guest panicked (a trap, or a panic inside a host import it called).
    Trapped,
}

fn call_guest(inst: &mut Instance<Glue<WasiHost>>, parts: IncomingRequestParts) -> Served {
    let call = std::panic::AssertUnwindSafe(|| {
        let host = &mut inst.imports.as_mut().expect("imports are set").0;
        let request = host.new_incoming_request(parts);
        let outparam = host.new_response_outparam();
        Exports::export_wasi_http_incoming_handler_0_2_0_handle(inst, request, outparam);
        inst.imports
            .as_mut()
            .expect("imports are set")
            .0
            .take_response(outparam)
    });
    match std::panic::catch_unwind(call) {
        Ok(Some(Ok(response))) => Served::Response(response),
        Ok(Some(Err(code))) => Served::Error(code),
        Ok(None) => Served::NoResponse,
        Err(_) => Served::Trapped,
    }
}

/// The client of a rejected request may still be sending: reads some of it, briefly, so that
/// closing is less likely to reset the connection before it reads the answer (best effort).
fn drain(stream: &mut TcpStream) {
    let deadline = Instant::now() + DRAIN_TIME;
    let mut sink = [0u8; 4096];
    let mut drained = 0;
    while drained < MAX_BODY {
        match read_some(stream, &mut sink, deadline) {
            Ok(n) if n > 0 => drained += n,
            _ => break,
        }
    }
}

/// Serves one connection. Returns `false` when the guest trapped (its instance is gone).
fn serve_connection(
    inst: &mut Instance<Glue<WasiHost>>,
    mut stream: TcpStream,
    idle: Duration,
) -> bool {
    let _ = stream.set_write_timeout(Some(REQUEST_TIME));
    let (parts, http10) = match read_request(&mut stream, Instant::now() + REQUEST_TIME) {
        Ok(read) => read,
        Err(Reject(status, why)) => {
            respond_text(&mut stream, status, why);
            let _ = stream.shutdown(std::net::Shutdown::Write);
            drain(&mut stream);
            return true;
        }
    };
    let head_only = matches!(parts.method, Method::Head);
    let ok = match call_guest(inst, parts) {
        Served::Response(response) => {
            respond(&mut stream, head_only, http10, response, idle);
            true
        }
        Served::NoResponse => {
            respond_text(&mut stream, 500, "the component did not set a response");
            true
        }
        Served::Error(code) => {
            respond_text(
                &mut stream,
                502,
                &format!("the component answered with {code:?}"),
            );
            true
        }
        Served::Trapped => {
            respond_text(&mut stream, 500, "the component trapped");
            false
        }
    };
    let _ = stream.shutdown(std::net::Shutdown::Write);
    ok
}

fn main() {
    let mut listen = "127.0.0.1:8080".to_string();
    let mut idle = BODY_IDLE;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().expect("--listen needs an address"),
            "--body-timeout" => {
                let secs = args.next().and_then(|v| v.parse().ok());
                idle = Duration::from_secs(secs.expect("--body-timeout needs seconds"));
            }
            other => {
                eprintln!("wasip2_serve: unexpected argument `{other}`");
                std::process::exit(2);
            }
        }
    }
    let listener = TcpListener::bind(&listen).unwrap_or_else(|e| {
        eprintln!("wasip2_serve: cannot listen on {listen}: {e}");
        std::process::exit(2);
    });
    // the port may have been chosen by the system (`:0`): the line tells where to connect
    println!(
        "listening on {}",
        listener.local_addr().expect("local address")
    );
    let mut inst = new_instance();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if !serve_connection(&mut inst, stream, idle) {
                    // a trapped instance may be half-updated: start over
                    inst = new_instance();
                }
            }
            Err(e) => eprintln!("wasip2_serve: accept failed: {e}"),
        }
    }
}
