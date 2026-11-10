//! A reference model of the request reader, written from the rules (RFC 9112 subset the
//! server documents), not from its code: the same bytes must give the same verdict.

use crate::api::{self, MAX_BODY, MAX_HEAD};
use crate::respond::Entries;
use crate::{find, is_token, net};
use std::time::{Duration, Instant};
use wasm2rs_wasi::IncomingRequestParts;
use wasm2rs_wasi::wasi_http_types::{Method, Scheme};

/// What the model says a request is.
#[derive(Debug, PartialEq)]
pub struct Parsed {
    pub method: String,
    pub target: String,
    pub http10: bool,
    pub headers: Entries,
    pub authority: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Accept(Parsed),
    Reject(u16),
}

pub struct Model {
    pub verdict: Verdict,
    /// The head limit is checked after each read of up to 4096 bytes, so a head whose end
    /// lies within one read of the limit may be taken or refused (413) depending on how the
    /// bytes were split by the socket: both are right.
    pub head_limit_band: bool,
}

const READ_CHUNK: usize = 4096;

pub fn model(input: &[u8]) -> Model {
    let reject = |status| Model {
        verdict: Verdict::Reject(status),
        head_limit_band: false,
    };
    let Some(end) = find(input, b"\r\n\r\n") else {
        // read to EOF without a blank line: over the limit the check fires first
        return reject(if input.len() > MAX_HEAD { 413 } else { 400 });
    };
    // the longest prefix a read loop can have seen without the terminator is `end + 3`
    let unseen = end + 3;
    if unseen >= MAX_HEAD + READ_CHUNK {
        return reject(413);
    }
    let head_limit_band = unseen > MAX_HEAD;
    let verdict = parse(&input[..end], &input[end + 4..]);
    Model {
        verdict,
        head_limit_band,
    }
}

fn parse(head: &[u8], rest: &[u8]) -> Verdict {
    use Verdict::Reject;
    let Ok(head) = std::str::from_utf8(head) else {
        return Reject(400);
    };
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let fields: Vec<&str> = request_line.split(' ').collect();
    let [method, target, version] = fields[..] else {
        return Reject(400);
    };
    let http10 = match version {
        "HTTP/1.0" => true,
        "HTTP/1.1" => false,
        _ => return Reject(400),
    };
    if !is_token(method) || !(target.starts_with('/') || target == "*") {
        return Reject(400);
    }
    let mut headers = Vec::new();
    let mut length: Option<usize> = None;
    let mut authority = None;
    for line in lines {
        let Some(colon) = line.find(':') else {
            return Reject(400);
        };
        let (name, value) = (&line[..colon], &line[colon + 1..]);
        if !is_token(name) {
            return Reject(400);
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Reject(501);
        }
        if name.eq_ignore_ascii_case("content-length") {
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Reject(400);
            }
            let Ok(n) = value.parse::<usize>() else {
                return Reject(400);
            };
            if length.is_some_and(|l| l != n) {
                return Reject(400);
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
        return Reject(413);
    }
    if rest.len() < length {
        // the peer closed before the declared end
        return Reject(400);
    }
    Verdict::Accept(Parsed {
        method: method.to_string(),
        target: target.to_string(),
        http10,
        headers,
        authority,
        body: rest[..length].to_vec(),
    })
}

/// The method a request line's `name` stands for.
pub fn method_of(name: &str) -> Method {
    match name {
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

/// Feeds `input` to the real reader over a socket and checks it against the model.
/// Returns what the reader said. Panics on a disagreement.
pub fn check_request(input: &[u8]) -> Result<(IncomingRequestParts, bool), api::Rejection> {
    let started = Instant::now();
    // a generous deadline: the peer closes, so a 408 would mean the reader hung on EOF
    let deadline = started + Duration::from_secs(30);
    let actual = net::serve_bytes(input, |s| api::read_request(s, deadline));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "reading {} bytes took {:?}",
        input.len(),
        started.elapsed()
    );
    let expected = model(input);
    match (&actual, &expected.verdict) {
        (Ok((parts, http10)), Verdict::Accept(p)) => {
            assert_eq!(parts.method, method_of(&p.method), "method");
            assert_eq!(parts.path_with_query.as_deref(), Some(p.target.as_str()));
            assert_eq!(parts.scheme, Some(Scheme::HTTP));
            assert_eq!(parts.authority, p.authority, "authority");
            assert_eq!(parts.headers, p.headers, "headers");
            assert_eq!(parts.body, p.body, "body");
            assert!(parts.trailers.is_none());
            assert_eq!(*http10, p.http10, "version flag");
        }
        (Err((413, _)), _) if expected.head_limit_band => {}
        (Err((status, why)), Verdict::Reject(want)) => {
            assert_eq!(status, want, "status ({why})");
            assert!(!why.is_empty() && !why.contains(['\r', '\n']));
        }
        _ => panic!(
            "reader said {:?}, model said {:?}",
            actual
                .as_ref()
                .map(|(p, v)| (&p.method, &p.path_with_query, v)),
            expected.verdict
        ),
    }
    actual
}
