//! The answer side: build a real `ServedResponse` through the typed host (the way a guest's
//! `handle` does), send it with the server's writer, and check the bytes with a strict
//! HTTP/1.1 response parser.

use crate::{api, find, is_field_byte, is_token, net};
use std::time::Duration;
use wasm2rs_wasi::wasi_http_types::{Fields, OutgoingResponse};
use wasm2rs_wasi::{Host, ServedResponse, WasiHost};

/// Header names and values the host accepts, plus (so far) all of what it refuses.
pub type Entries = Vec<(String, Vec<u8>)>;

/// What a guest wants to answer with.
#[derive(Debug, Default, Clone)]
pub struct Plan {
    pub status: u16,
    pub headers: Entries,
    pub body: Vec<u8>,
    pub trailers: Option<Entries>,
    /// Leave the body unfinished (the guest keeps it open).
    pub open: bool,
}

pub fn host() -> WasiHost {
    WasiHost::new(Vec::new())
}

/// A `fields` resource holding the `entries` the host accepts.
fn fields(h: &mut WasiHost, entries: &Entries) -> Fields {
    let fields = h.wasi_http_types_constructor_fields();
    for (name, value) in entries {
        let _ = h.wasi_http_types_method_fields_append(fields, name.clone(), value.clone());
    }
    fields
}

/// The response the host hands the embedder for `plan`. Entries the host refuses (bad syntax,
/// forbidden names) are dropped, as a guest's failed `append` leaves them out, and a status it
/// refuses is ignored (it stays 200). `None` when the body calls (`body`, `write`, `finish`) or
/// `take_response` fail.
pub fn served(h: &mut WasiHost, plan: &Plan) -> Option<ServedResponse> {
    let headers = fields(h, &plan.headers);
    let response: OutgoingResponse = h.wasi_http_types_constructor_outgoing_response(headers);
    let _ = h.wasi_http_types_method_outgoing_response_set_status_code(response, plan.status);
    let body = h
        .wasi_http_types_method_outgoing_response_body(response)
        .ok()?;
    let stream = h.wasi_http_types_method_outgoing_body_write(body).ok()?;
    if !plan.body.is_empty() {
        h.wasi_io_streams_method_output_stream_write(stream, plan.body.clone())
            .ok()?;
    }
    h.wasi_io_streams_resource_drop_output_stream(stream);
    if !plan.open {
        let trailers = plan.trailers.as_ref().map(|entries| fields(h, entries));
        h.wasi_http_types_static_outgoing_body_finish(body, trailers)
            .ok()?;
    }
    let out = h.new_response_outparam();
    h.wasi_http_types_static_response_outparam_set(out, Ok(response));
    h.take_response(out)?.ok()
}

/// What `respond` wrote for `response`.
pub fn wire(response: ServedResponse, head_only: bool, http10: bool, idle: Duration) -> Vec<u8> {
    net::capture(|s| api::respond(s, head_only, http10, response, idle)).1
}

#[derive(Debug)]
pub struct Wire {
    pub status: u16,
    pub reason: String,
    pub headers: Entries,
    pub payload: Vec<u8>,
}

/// Splits a `name: value` line whose name is a token; `what` names the line in errors.
fn split_field<'a>(line: &'a [u8], what: &str) -> Result<(&'a str, &'a [u8]), String> {
    let colon = find(line, b": ")
        .ok_or_else(|| format!("{what} line {:?}", String::from_utf8_lossy(line)))?;
    let name = std::str::from_utf8(&line[..colon]).map_err(|_| format!("{what} name"))?;
    if !is_token(name) {
        return Err(format!("{what} name {name:?}"));
    }
    Ok((name, &line[colon + 2..]))
}

/// Parses an answer strictly: any CR/LF outside a line end, an odd status line or a header
/// that is not `token: value` fails. Returns the head's parts and everything after it.
pub fn parse_head(bytes: &[u8]) -> Result<Wire, String> {
    let end = find(bytes, b"\r\n\r\n").ok_or("no blank line ends the head")?;
    // header values may hold obs-text (0x80 and up): the head is bytes, not UTF-8
    let head = &bytes[..end];
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut rest = head;
    while let Some(at) = find(rest, b"\r\n") {
        lines.push(&rest[..at]);
        rest = &rest[at + 2..];
    }
    lines.push(rest);
    // every CR and LF left in a line would be a smuggled line break
    if lines
        .iter()
        .any(|l| l.iter().any(|&b| b == b'\r' || b == b'\n'))
    {
        return Err("a bare CR or LF inside a line".into());
    }
    let mut lines = lines.into_iter();
    let status_line = std::str::from_utf8(lines.next().ok_or("empty head")?)
        .map_err(|_| "status line is not UTF-8")?;
    let (version, rest) = status_line.split_once(' ').ok_or("bad status line")?;
    if version != "HTTP/1.1" {
        return Err(format!("version {version:?}"));
    }
    let (code, reason) = rest
        .split_once(' ')
        .ok_or("status line without reason slot")?;
    if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("status code {code:?}"));
    }
    if !reason.bytes().all(is_field_byte) {
        return Err(format!("reason {reason:?}"));
    }
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = split_field(line, "header")?;
        if !value.iter().copied().all(is_field_byte) {
            return Err(format!("header value {:?}", String::from_utf8_lossy(value)));
        }
        headers.push((name.to_string(), value.to_vec()));
    }
    Ok(Wire {
        status: code.parse().map_err(|_| "status")?,
        reason: reason.to_string(),
        headers,
        payload: bytes[end + 4..].to_vec(),
    })
}

/// Decodes a chunked payload: the body and the trailer lines after the last chunk. The
/// payload must end right after the trailers' blank line.
pub fn dechunk(mut p: &[u8]) -> Result<(Vec<u8>, Entries), String> {
    let mut body = Vec::new();
    loop {
        let nl = find(p, b"\r\n").ok_or("chunk size line is unterminated")?;
        let size = std::str::from_utf8(&p[..nl]).map_err(|_| "chunk size")?;
        if size.is_empty() || !size.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("chunk size {size:?}"));
        }
        let size = usize::from_str_radix(size, 16).map_err(|_| "chunk size overflow")?;
        p = &p[nl + 2..];
        if size == 0 {
            break;
        }
        if p.len() < size + 2 || &p[size..size + 2] != b"\r\n" {
            return Err("chunk data is not followed by CRLF".into());
        }
        body.extend_from_slice(&p[..size]);
        p = &p[size + 2..];
    }
    let mut trailers = Vec::new();
    loop {
        let nl = find(p, b"\r\n").ok_or("the trailer section is unterminated")?;
        if nl == 0 {
            p = &p[2..];
            break;
        }
        let (name, value) = split_field(&p[..nl], "trailer")?;
        trailers.push((name.to_string(), value.to_vec()));
        p = &p[nl + 2..];
    }
    if !p.is_empty() {
        return Err(format!("{} bytes after the last chunk", p.len()));
    }
    Ok((body, trailers))
}

/// Checks `bytes`, what `respond` wrote for `response`'s plan (a finished body), against the
/// rules: the guest's `Content-Length`/`Connection`/`Transfer-Encoding` are replaced by the
/// server's, 1xx/204 carry no `content-length`, HEAD/1xx/204/304 no payload, a body with
/// trailers is chunked (HTTP/1.1) and ends with them, HTTP/1.0 is never chunked.
pub fn check_wire(
    bytes: &[u8],
    served: &Entries,
    status: u16,
    body: &[u8],
    trailers: Option<&Entries>,
    head_only: bool,
    http10: bool,
) -> Result<(), String> {
    let wire = parse_head(bytes)?;
    if wire.status != status {
        return Err(format!("status {} for {status}", wire.status));
    }
    if wire.reason != api::reason(status) {
        return Err(format!("reason {:?}", wire.reason));
    }
    let replaced = |n: &str| {
        ["content-length", "connection", "transfer-encoding"]
            .iter()
            .any(|h| n.eq_ignore_ascii_case(h))
    };
    let mut want: Entries = served
        .iter()
        .filter(|(n, _)| !replaced(n))
        .cloned()
        .collect();
    let bodyless = head_only || status < 200 || status == 204 || status == 304;
    let may_have_framing = status >= 200 && status != 204;
    let chunked = trailers.is_some() && !bodyless && !http10;
    // The server's own header lines, in the order it writes them.
    if may_have_framing {
        if trailers.is_none() {
            want.push(("content-length".into(), body.len().to_string().into_bytes()));
        } else if chunked {
            want.push(("transfer-encoding".into(), b"chunked".to_vec()));
        }
        // trailers on a bodyless or HTTP/1.0 answer: no framing header (streamed or empty)
    }
    want.push(("connection".into(), b"close".to_vec()));
    if wire.headers != want {
        return Err(format!("headers {:?}, wanted {want:?}", wire.headers));
    }
    let payload = &wire.payload;
    if bodyless {
        if !payload.is_empty() {
            return Err(format!(
                "{} payload bytes on a bodyless answer",
                payload.len()
            ));
        }
    } else if chunked {
        let (got, got_trailers) = dechunk(payload)?;
        if got != body {
            return Err("chunked body differs".into());
        }
        if Some(&got_trailers) != trailers {
            return Err(format!("trailers {got_trailers:?}"));
        }
    } else if payload != body {
        return Err("body differs".into());
    }
    Ok(())
}
