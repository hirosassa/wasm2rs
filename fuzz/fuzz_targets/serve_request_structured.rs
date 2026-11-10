#![no_main]
//! Structured requests: mostly well-formed ones built from parts, so the fuzzer gets past
//! the request line quickly. A request built only from valid parts must be accepted with
//! exactly the parts it was built from (method, target, headers, body, version flag); one
//! with a corrupted byte only has to agree with the reference model.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use wasm2rs_fuzz_serve::model::{check_request, method_of};
use wasm2rs_wasi::wasi_http_types::Scheme;

const TCHARS: &[u8] =
    b"!#$%&'*+-.^_`|~0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

fn token(raw: &[u8], max: usize) -> String {
    let mut s: String = raw
        .iter()
        .take(max)
        .map(|b| TCHARS[usize::from(*b) % TCHARS.len()] as char)
        .collect();
    if s.is_empty() {
        s.push('x');
    }
    s
}

/// A header value: visible ASCII, spaces and tabs inside (the reader trims the ends).
fn value(raw: &[u8], max: usize) -> Vec<u8> {
    let v: Vec<u8> = raw
        .iter()
        .take(max)
        .map(|b| match b % 40 {
            0 => b'\t',
            1 | 2 => b' ',
            n => 0x21 + (b.wrapping_add(n)) % 0x5e,
        })
        .collect();
    // the only whitespace `v` can hold is SP and HTAB, so this trims exactly those
    v.trim_ascii().to_vec()
}

#[derive(Arbitrary, Debug)]
enum MethodPick {
    Known(u8),
    Custom(Vec<u8>),
}

#[derive(Arbitrary, Debug)]
enum Length {
    /// `Content-Length` equal to the body, written `times` times (1 to 3).
    Exact { times: u8, pad: bool },
    /// No header: the body must be empty.
    Absent,
}

#[derive(Arbitrary, Debug)]
struct Request {
    method: MethodPick,
    target: Vec<u8>,
    http10: bool,
    headers: Vec<(Vec<u8>, Vec<u8>)>,
    host: Option<Vec<u8>>,
    length: Length,
    body: Vec<u8>,
    /// Pipelined bytes after the body.
    trailing: Vec<u8>,
    corrupt: Option<(u16, u8)>,
}

const KNOWN: &[&str] = &[
    "GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH",
];

fuzz_target!(|req: Request| {
    let method_text = match &req.method {
        MethodPick::Known(i) => KNOWN[usize::from(*i) % KNOWN.len()].to_string(),
        MethodPick::Custom(raw) => token(raw, 16),
    };
    // a custom method that spells a known one is that one
    let method = method_of(&method_text);
    let mut target = String::from("/");
    target.extend(
        req.target
            .iter()
            .take(64)
            .map(|b| (0x21 + b % 0x5d) as char),
    );
    let body: Vec<u8> = req.body.iter().take(4096).copied().collect();
    let mut headers: Vec<(String, Vec<u8>)> = Vec::new();
    for (n, v) in req.headers.iter().take(16) {
        let name = token(n, 12);
        // the structured request exercises the plain path: the framing headers are ours
        if ["transfer-encoding", "content-length", "host"]
            .iter()
            .any(|framing| name.eq_ignore_ascii_case(framing))
        {
            continue;
        }
        headers.push((name, value(v, 48)));
    }
    let host = req
        .host
        .as_ref()
        .map(|h| String::from_utf8(value(h, 24)).unwrap_or_default());
    if let Some(h) = &host {
        headers.push(("Host".to_string(), h.clone().into_bytes()));
    }
    let mut wire_headers = headers.clone();
    let body_sent: &[u8] = match req.length {
        Length::Exact { times, pad } => {
            for _ in 0..=(times % 3) {
                let text = if pad {
                    format!(" \t{}\t ", body.len())
                } else {
                    body.len().to_string()
                };
                wire_headers.push(("Content-Length".to_string(), text.into_bytes()));
            }
            &body
        }
        Length::Absent => &[],
    };

    let mut bytes = format!(
        "{method_text} {target} HTTP/1.{}\r\n",
        if req.http10 { 0 } else { 1 }
    )
    .into_bytes();
    for (n, v) in &wire_headers {
        bytes.extend_from_slice(n.as_bytes());
        bytes.extend_from_slice(b": ");
        bytes.extend_from_slice(v);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(body_sent);
    bytes.extend(req.trailing.iter().take(256));

    if let Some((at, byte)) = req.corrupt {
        let i = usize::from(at) % bytes.len();
        bytes[i] = byte;
        let _ = check_request(&bytes);
        return;
    }

    // Built only from valid parts: it must come out as it went in. (The reader keeps the
    // header values trimmed, and a padded Content-Length is a header like any other.)
    let (parts, http10) = check_request(&bytes).expect("a well-formed request is accepted");
    assert_eq!(parts.method, method);
    assert_eq!(parts.path_with_query.as_deref(), Some(target.as_str()));
    assert_eq!(parts.scheme, Some(Scheme::HTTP));
    assert_eq!(parts.authority, host);
    assert_eq!(parts.body, body_sent);
    assert_eq!(http10, req.http10);
    let got: Vec<_> = parts
        .headers
        .iter()
        .filter(|(n, _)| !n.eq_ignore_ascii_case("content-length"))
        .cloned()
        .collect();
    assert_eq!(got, headers);
});
