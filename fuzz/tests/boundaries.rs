//! The limits and the rejection codes, pinned with fixed inputs (the seeds of the fuzz
//! corpus, and the shapes a regression test for a finding goes next to).

use wasm2rs_fuzz_serve::api::{MAX_BODY, MAX_HEAD};
use wasm2rs_fuzz_serve::model::check_request;

fn status(bytes: &[u8]) -> u16 {
    match check_request(bytes) {
        Ok(_) => 200,
        Err((s, _)) => s,
    }
}

#[test]
fn the_plain_cases() {
    assert_eq!(status(b"GET / HTTP/1.1\r\nhost: a\r\n\r\n"), 200);
    assert_eq!(status(b"GET * HTTP/1.0\r\n\r\n"), 200);
    assert_eq!(
        status(b"POST /p HTTP/1.1\r\ncontent-length: 3\r\n\r\nabcEXTRA"),
        200
    );
    assert_eq!(status(b"GET / HTTP/1.1\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/2.0\r\n\r\n"), 400);
    assert_eq!(status(b"GET http://a/ HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(status(b"GET  / HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(status(b"G@T / HTTP/1.1\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\n folded: x\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\nno colon\r\n\r\n"), 400);
    assert_eq!(status(b"GET / HTTP/1.1\r\n\xff: x\r\n\r\n"), 400);
}

#[test]
fn content_length_edges() {
    let with = |v: &str| format!("POST / HTTP/1.1\r\ncontent-length: {v}\r\n\r\n").into_bytes();
    assert_eq!(status(&with("+3")), 400);
    assert_eq!(status(&with("")), 400);
    assert_eq!(status(&with("0x3")), 400);
    assert_eq!(status(&with("99999999999999999999999999")), 400);
    assert_eq!(status(&with(&(MAX_BODY + 1).to_string())), 413);
    // the limit itself is allowed, but nothing follows: the body is cut short
    assert_eq!(status(&with(&MAX_BODY.to_string())), 400);
    assert_eq!(
        status(b"POST / HTTP/1.1\r\ncontent-length: 1\r\ncontent-length: 2\r\n\r\nab"),
        400
    );
    assert_eq!(
        status(b"POST / HTTP/1.1\r\ncontent-length: 2\r\ncontent-length: 2\r\n\r\nab"),
        200
    );
}

#[test]
fn a_body_of_exactly_the_limit_is_taken() {
    let mut req = format!("POST / HTTP/1.1\r\ncontent-length: {MAX_BODY}\r\n\r\n").into_bytes();
    req.extend(std::iter::repeat_n(b'z', MAX_BODY));
    assert_eq!(status(&req), 200);
}

#[test]
fn transfer_encoding_is_501_in_any_case_unless_an_earlier_line_is_malformed() {
    assert_eq!(
        status(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
        501
    );
    assert_eq!(
        status(b"POST / HTTP/1.1\r\ntRaNsFeR-eNcOdInG: x\r\n\r\n"),
        501
    );
    assert_eq!(
        status(b"POST / HTTP/1.1\r\nbad line\r\ntransfer-encoding: chunked\r\n\r\n"),
        400
    );
}

#[test]
fn the_head_limit() {
    let head = |len: usize| {
        // a request line plus one header padded so that the head (without the blank line)
        // is `len` bytes
        let base = "GET / HTTP/1.1\r\nx: ";
        let mut v = base.as_bytes().to_vec();
        v.extend(std::iter::repeat_n(b'a', len - base.len()));
        v.extend_from_slice(b"\r\n\r\n");
        v
    };
    // far enough below the limit that no split of the reads can refuse it
    assert_eq!(status(&head(MAX_HEAD - 8)), 200);
    // no blank line, over the limit
    let mut open = head(MAX_HEAD + 10);
    open.truncate(MAX_HEAD + 10);
    assert_eq!(status(&open), 413);
    // well over the limit, with its blank line
    assert_eq!(status(&head(MAX_HEAD + 5000)), 413);
}

#[test]
fn no_header_value_reaches_the_answer_with_a_line_break() {
    use wasm2rs_fuzz_serve::api::{self, Frame};
    // `head` itself does not sanitize: it relies on the host refusing CR/LF in a field value
    // (`wasm2rs-wasi` `is_header_value`). This pins both halves of that contract.
    let raw = api::head(
        200,
        &[("x".into(), b"a\r\nset-cookie: s".to_vec())],
        Some(Frame::Length(0)),
    );
    assert!(
        raw.windows(2).filter(|w| *w == b"\r\n").count() > 4,
        "head passes values through"
    );
    let mut host = wasm2rs_fuzz_serve::respond::host();
    use wasm2rs_wasi::Host;
    let fields = host.wasi_http_types_constructor_fields();
    assert!(
        host.wasi_http_types_method_fields_append(
            fields,
            "x".into(),
            b"a\r\nset-cookie: s".to_vec()
        )
        .is_err()
    );
}
