//! [`Http1Client`]: an [`HttpClient`] that speaks HTTP/1.1 over `std::net` (feature
//! `http-client`), for the one thing `UreqClient` cannot do: send and receive trailers.

use crate::ureq_client::io_kind;
use crate::{HttpClient, HttpClientError, HttpClientErrorKind, HttpRequest, HttpResponse};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Entries = Vec<(String, Vec<u8>)>;

/// Longest head or trailer section accepted, against a peer that never ends a line.
const MAX_HEAD: usize = 64 * 1024;
const MAX_INTERIM: u8 = 16;
const MAX_HEAD_U64: u64 = 64 * 1024;

/// Executes `wasi:http` requests over plain `http://`, with trailers in both directions.
///
/// A request with trailers is sent chunked, the trailers after the last chunk (the guest's
/// `content-length` and `transfer-encoding` are replaced); a chunked response's trailers are
/// handed over in [`HttpResponse::trailers`] once its body has been read.
///
/// Limits: **no TLS** (`https` is refused; use `UreqClient` for it), no proxy, no redirects, no
/// connection reuse (every request is sent with `connection: close`), HTTP/1.1 only. The body is
/// neither decompressed nor otherwise rewritten. A `HEAD` or `CONNECT` request with a body is
/// refused.
///
/// `request-options`: `connect-timeout` bounds each connection attempt, `first-byte-timeout` the
/// wait for the response head and `between-bytes-timeout` every read of the body.
#[derive(Debug, Clone, Default)]
pub struct Http1Client {
    _private: (),
}

impl Http1Client {
    pub fn new() -> Self {
        Self::default()
    }
}

fn client_error(message: impl Into<String>) -> HttpClientError {
    HttpClientError::new(message)
}

fn protocol_error(message: impl Into<String>) -> HttpClientError {
    HttpClientError::with_kind(HttpClientErrorKind::HttpProtocol, message)
}

fn io_error(error: io::Error) -> HttpClientError {
    HttpClientError::with_kind(io_kind(error.kind()), error.to_string())
}

fn nanos(timeout: Option<u64>) -> Option<Duration> {
    // a zero timeout is an error for `std`, and means "no time at all" to the guest
    timeout.map(|n| Duration::from_nanos(n.max(1)))
}

/// The host, the port and the request target of a `http://` URL.
fn split_url(url: &str) -> Result<(&str, u16, String), HttpClientError> {
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(client_error(if url.starts_with("https://") {
            "Http1Client does not support https (no TLS); use UreqClient".to_string()
        } else {
            format!("unsupported url `{url}`")
        }));
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let tail = tail.split('#').next().unwrap_or_default();
    let target = match tail {
        "" => "/".to_string(),
        t if t.starts_with('?') => format!("/{t}"),
        t => t.to_string(),
    };
    // userinfo is not sent: `host:` carries the host and port only
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, "")) if !h.ends_with(':') => (h, 80),
        Some((h, p)) if !p.contains(']') => (
            h,
            p.parse::<u16>()
                .map_err(|_| client_error(format!("invalid port in `{url}`")))?,
        ),
        _ => (authority, 80),
    };
    if host.is_empty() {
        return Err(client_error(format!("no host in `{url}`")));
    }
    Ok((host, port, target))
}

fn check_field(name: &str, value: &[u8]) -> Result<(), HttpClientError> {
    let bad_name = name.is_empty()
        || name
            .bytes()
            .any(|b| b <= b' ' || b == b':' || b == 0x7f || !b.is_ascii());
    let bad_value = value.iter().any(|&b| matches!(b, b'\r' | b'\n' | 0));
    if bad_name || bad_value {
        return Err(client_error(format!("invalid header `{name}`")));
    }
    Ok(())
}

fn connect(host: &str, port: u16, timeout: Option<Duration>) -> Result<TcpStream, HttpClientError> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| HttpClientError::with_kind(HttpClientErrorKind::DnsFailure, e.to_string()))?;
    let mut last = None;
    for addr in addrs {
        let result = match timeout {
            Some(t) => TcpStream::connect_timeout(&addr, t),
            None => TcpStream::connect(addr),
        };
        match result {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(last.map_or_else(
        || HttpClientError::with_kind(HttpClientErrorKind::DnsFailure, "no address for the host"),
        io_error,
    ))
}

fn push_field(out: &mut Vec<u8>, name: &str, value: &[u8]) {
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value);
    out.extend_from_slice(b"\r\n");
}

fn write_request(
    stream: &mut TcpStream,
    request: &HttpRequest,
    authority: &str,
    target: &str,
) -> Result<io::Result<()>, HttpClientError> {
    let chunked = !request.trailers.is_empty();
    let mut out = format!("{} {target} HTTP/1.1\r\n", request.method).into_bytes();
    let mut has_host = false;
    for (name, value) in request.headers.iter().chain(&request.trailers) {
        check_field(name, value)?;
    }
    for (name, value) in &request.headers {
        let lower = name.to_ascii_lowercase();
        has_host |= lower == "host";
        // ours: the message is framed here, and every request ends the connection
        if matches!(
            lower.as_str(),
            "connection" | "content-length" | "transfer-encoding"
        ) {
            continue;
        }
        push_field(&mut out, name, value);
    }
    if !has_host {
        out.extend_from_slice(format!("host: {authority}\r\n").as_bytes());
    }
    out.extend_from_slice(b"connection: close\r\n");
    if chunked {
        out.extend_from_slice(b"transfer-encoding: chunked\r\n\r\n");
        if !request.body.is_empty() {
            out.extend_from_slice(format!("{:x}\r\n", request.body.len()).as_bytes());
            out.extend_from_slice(&request.body);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n");
        for (name, value) in &request.trailers {
            push_field(&mut out, name, value);
        }
        out.extend_from_slice(b"\r\n");
    } else {
        // the guest's own length is not trusted: it may not match the bytes it wrote
        if !request.body.is_empty() || matches!(request.method.as_str(), "POST" | "PUT" | "PATCH") {
            out.extend_from_slice(format!("content-length: {}\r\n", request.body.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&request.body);
    }
    // A server may answer (413, 401) without reading the body and close: the write error is
    // then kept back, so that its response is still read.
    Ok(stream.write_all(&out))
}

fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut raw = Vec::new();
    let n = reader
        .by_ref()
        .take(MAX_HEAD_U64)
        .read_until(b'\n', &mut raw)?;
    if n == 0 {
        return Ok(None);
    }
    if !raw.ends_with(b"\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            if n >= MAX_HEAD {
                "line too long"
            } else {
                "unterminated line"
            },
        ));
    }
    raw.pop();
    if raw.ends_with(b"\r") {
        raw.pop();
    }
    Ok(Some(raw))
}

/// Header or trailer lines up to the empty line.
fn read_fields(reader: &mut impl BufRead) -> io::Result<Entries> {
    let mut fields = Vec::new();
    let mut total = 0usize;
    loop {
        let line =
            read_line(reader)?.ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        if line.is_empty() {
            return Ok(fields);
        }
        total = total.saturating_add(line.len());
        if total > MAX_HEAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fields too large",
            ));
        }
        let malformed = || io::Error::new(io::ErrorKind::InvalidData, "malformed field line");
        let colon = line.iter().position(|&b| b == b':').ok_or_else(malformed)?;
        let (name, value) = line.split_at(colon);
        // the name is ASCII (HTTP's token); the value may hold any byte (obs-text)
        let name = std::str::from_utf8(name)
            .ok()
            .filter(|n| n.is_ascii() && !n.is_empty() && !n.contains([' ', '\t']))
            .ok_or_else(malformed)?;
        let value = value.get(1..).unwrap_or_default().trim_ascii();
        fields.push((name.to_string(), value.to_vec()));
    }
}

/// Every value of `name`, joined with commas as a repeated header is read (`None`: absent).
fn find(fields: &Entries, name: &str) -> Option<Vec<u8>> {
    let mut values = fields
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_slice())
        .peekable();
    values.peek()?;
    Some(values.collect::<Vec<_>>().join(&b", "[..]))
}

/// The one value of a `content-length` (possibly repeated, or a list of equal values).
fn content_length(value: &[u8]) -> Option<u64> {
    let mut lengths = value.split(|&b| b == b',').map(|v| {
        std::str::from_utf8(v.trim_ascii())
            .ok()?
            .parse::<u64>()
            .ok()
    });
    let first = lengths.next()??;
    lengths.all(|l| l == Some(first)).then_some(first)
}

/// Decodes a chunked body and keeps the trailers it ends with.
struct ChunkedBody<R> {
    reader: R,
    left: u64,
    done: bool,
    trailers: Arc<Mutex<Entries>>,
}

impl<R: BufRead> ChunkedBody<R> {
    fn next_chunk(&mut self) -> io::Result<()> {
        let size_line = read_line(&mut self.reader)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        // chunk extensions are ignored
        let size = size_line.split(|&b| b == b';').next().unwrap_or_default();
        let size = std::str::from_utf8(size).unwrap_or_default().trim();
        self.left = u64::from_str_radix(size, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid chunk size"))?;
        if self.left == 0 {
            let fields = read_fields(&mut self.reader)?;
            *self.trailers.lock().unwrap_or_else(|p| p.into_inner()) = fields;
            self.done = true;
        }
        Ok(())
    }
}

impl<R: BufRead> Read for ChunkedBody<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while !self.done && self.left == 0 {
            self.next_chunk()?;
        }
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        let cap = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let Some((window, _)) = buf.split_at_mut_checked(cap) else {
            return Ok(0);
        };
        let n = self.reader.read(window)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.left = self
            .left
            .saturating_sub(u64::try_from(n).unwrap_or(u64::MAX));
        if self.left == 0 {
            let end = read_line(&mut self.reader)?;
            if end.as_deref() != Some(b"") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "missing chunk end",
                ));
            }
        }
        Ok(n)
    }
}

/// A body of `length` bytes that ends early only with an error.
struct SizedBody<R> {
    reader: io::Take<R>,
}

impl<R: Read> Read for SizedBody<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read(buf)?;
        if n == 0 && self.reader.limit() > 0 && !buf.is_empty() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(n)
    }
}

/// Applies `between-bytes-timeout` to the body's reads.
struct Timed {
    reader: Box<dyn Read + Send>,
    stream: TcpStream,
    timeout: Option<Duration>,
    armed: bool,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.armed {
            self.stream.set_read_timeout(self.timeout)?;
            self.armed = true;
        }
        self.reader.read(buf)
    }
}

fn take_trailers(shared: Arc<Mutex<Entries>>) -> Box<dyn FnOnce() -> Entries + Send> {
    Box::new(move || std::mem::take(&mut *shared.lock().unwrap_or_else(|p| p.into_inner())))
}

impl HttpClient for Http1Client {
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        if !request.body.is_empty() && matches!(request.method.as_str(), "HEAD" | "CONNECT") {
            return Err(client_error(format!(
                "a {} request cannot have a body",
                request.method
            )));
        }
        if request.method.is_empty()
            || request
                .method
                .bytes()
                .any(|b| b <= b' ' || b == 0x7f || !b.is_ascii())
        {
            return Err(client_error("invalid method"));
        }
        let (host, port, target) = split_url(&request.url)?;
        let authority = if port == 80 {
            host.to_string()
        } else {
            format!("{host}:{port}")
        };
        let mut stream = connect(host, port, nanos(request.connect_timeout))?;
        // `request-options` has no write timeout: the wait for the head bounds the write too
        stream
            .set_write_timeout(nanos(request.first_byte_timeout))
            .map_err(io_error)?;
        let write_result = write_request(&mut stream, &request, &authority, &target)?;

        let head_timeout = nanos(request.first_byte_timeout);
        stream.set_read_timeout(head_timeout).map_err(io_error)?;
        let mut reader = BufReader::new(stream.try_clone().map_err(io_error)?);
        let head_error = |e: io::Error| match e.kind() {
            // the server stopped reading the request: that is the more telling error
            _ if write_result.is_err() => io_error(
                write_result
                    .as_ref()
                    .err()
                    .map_or_else(|| io::Error::from(e.kind()), |w| io::Error::from(w.kind())),
            ),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => HttpClientError::with_kind(
                HttpClientErrorKind::ResponseTimeout,
                "timed out waiting for the response head",
            ),
            io::ErrorKind::InvalidData => protocol_error(e.to_string()),
            _ => io_error(e),
        };
        let mut interim = 0u8;
        let (status, headers) = loop {
            let status_line =
                read_line(&mut reader)
                    .map_err(head_error)?
                    .ok_or_else(|| match &write_result {
                        Err(w) => io_error(io::Error::from(w.kind())),
                        Ok(()) => HttpClientError::with_kind(
                            HttpClientErrorKind::ConnectionTerminated,
                            "the connection closed before the response head",
                        ),
                    })?;
            // the reason phrase may hold any byte and is not read
            let mut parts = status_line.splitn(3, |&b| b == b' ');
            let version = parts.next().unwrap_or_default();
            let status = parts
                .next()
                .and_then(|s| std::str::from_utf8(s).ok()?.parse::<u16>().ok());
            let (true, Some(status)) = (version.starts_with(b"HTTP/1."), status) else {
                return Err(protocol_error(format!(
                    "malformed status line `{}`",
                    String::from_utf8_lossy(&status_line)
                )));
            };
            let headers = read_fields(&mut reader).map_err(head_error)?;
            // interim responses are not the answer (101 would switch protocols: not supported)
            if (100..200).contains(&status) && status != 101 {
                interim = interim.saturating_add(1);
                if interim > MAX_INTERIM {
                    return Err(protocol_error("too many interim responses"));
                }
                continue;
            }
            break (status, headers);
        };

        let bodyless = request.method == "HEAD" || matches!(status, 101 | 204 | 304);
        let chunked = find(&headers, "transfer-encoding").is_some_and(|v| {
            String::from_utf8_lossy(&v)
                .to_ascii_lowercase()
                .contains("chunked")
        });
        let mut trailers = None;
        let body: Box<dyn Read + Send> = if bodyless {
            Box::new(io::empty())
        } else if chunked {
            let shared = Arc::new(Mutex::new(Entries::new()));
            trailers = Some(take_trailers(Arc::clone(&shared)));
            Box::new(ChunkedBody {
                reader,
                left: 0,
                done: false,
                trailers: shared,
            })
        } else if let Some(len) = find(&headers, "content-length") {
            let len =
                content_length(&len).ok_or_else(|| protocol_error("invalid content-length"))?;
            Box::new(SizedBody {
                reader: reader.take(len),
            })
        } else {
            Box::new(reader)
        };
        let body = Box::new(Timed {
            reader: body,
            stream,
            timeout: nanos(request.between_bytes_timeout),
            armed: false,
        });
        Ok(HttpResponse {
            status,
            headers,
            body,
            trailers,
        })
    }
}
