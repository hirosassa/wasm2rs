#![no_main]
//! Responses: a real `ServedResponse` is built through the typed host from fuzzed parts and
//! sent by the server's writer; the bytes must parse as strict HTTP/1.1 (no header injection,
//! one framing, the body and trailers as the guest wrote them). Also `head` on raw,
//! unvalidated entries: it must not panic, and for entries the host would have accepted it
//! must produce exactly one head.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use std::time::Duration;
use wasm2rs_fuzz_serve::api::{self, Frame};
use wasm2rs_fuzz_serve::respond::{self, Entries, Plan};
use wasm2rs_fuzz_serve::{is_field_byte, is_token};

#[derive(Arbitrary, Debug)]
enum Name {
    Pool(u8),
    Raw(Vec<u8>),
}

#[derive(Arbitrary, Debug)]
struct Resp {
    status: u16,
    head_only: bool,
    http10: bool,
    headers: Vec<(Name, Vec<u8>)>,
    body: Vec<u8>,
    trailers: Option<Vec<(Name, Vec<u8>)>>,
    raw_framing: u8,
}

const POOL: &[&str] = &[
    "Content-Length",
    "content-length",
    "Connection",
    "Transfer-Encoding",
    "X-A",
    "set-cookie",
    "Content-Type",
];

fn name(n: &Name) -> String {
    match n {
        Name::Pool(i) => POOL[usize::from(*i) % POOL.len()].to_string(),
        Name::Raw(raw) => String::from_utf8_lossy(raw).into_owned(),
    }
}

fn entries(list: &[(Name, Vec<u8>)]) -> Entries {
    list.iter()
        .take(24)
        .map(|(n, v)| (name(n), v.clone()))
        .collect()
}

fuzz_target!(|r: Resp| {
    let headers = entries(&r.headers);
    let trailers = r.trailers.as_deref().map(entries);

    // `head` on entries as they come, no host in between.
    let (framing, framing_line) = match r.raw_framing % 3 {
        0 => (None, None),
        1 => (
            Some(Frame::Length(r.body.len())),
            Some(("content-length", r.body.len().to_string().into_bytes())),
        ),
        _ => (
            Some(Frame::Chunked),
            Some(("transfer-encoding", b"chunked".to_vec())),
        ),
    };
    let raw = api::head(r.status, &headers, framing);
    assert!(raw.ends_with(b"\r\n\r\n"));
    let clean = headers
        .iter()
        .all(|(n, v)| is_token(n) && v.iter().copied().all(is_field_byte));
    if clean && (100..=599).contains(&r.status) {
        // exactly one blank line, at the end: nothing got in through a value
        assert_eq!(raw.windows(4).filter(|w| *w == b"\r\n\r\n").count(), 1);
        let parsed = respond::parse_head(&raw).expect("a head of clean entries is strict HTTP/1.1");
        // the guest's framing/connection headers are replaced, whatever their case, by ours
        let mut want: Entries = headers
            .iter()
            .filter(|(n, _)| {
                !["content-length", "connection", "transfer-encoding"]
                    .iter()
                    .any(|h| n.eq_ignore_ascii_case(h))
            })
            .cloned()
            .collect();
        if r.status >= 200 && r.status != 204 {
            want.extend(framing_line.map(|(n, v)| (n.to_string(), v)));
        }
        want.push(("connection".into(), b"close".to_vec()));
        assert_eq!(parsed.headers, want);
    }

    // The whole answer through the host.
    let plan = Plan {
        status: r.status,
        headers,
        body: r.body.iter().take(32 * 1024).copied().collect(),
        trailers,
        open: false,
    };
    let mut host = respond::host();
    let Some(served) = respond::served(&mut host, &plan) else {
        return;
    };
    let (status, kept) = (served.status, served.headers.clone());
    let bytes = respond::wire(served, r.head_only, r.http10, Duration::from_secs(2));
    // the trailers as the host holds them are what the writer sends
    let sent_trailers = plan.trailers.as_ref().map(|_| {
        // re-read through a second, identical response: the first was consumed by `wire`
        let again = respond::served(&mut respond::host(), &plan).expect("same plan");
        again.body.trailers().unwrap_or_default()
    });
    if let Err(why) = respond::check_wire(
        &bytes,
        &kept,
        status,
        &plan.body,
        sent_trailers.as_ref(),
        r.head_only,
        r.http10,
    ) {
        panic!("{why}\nwire: {:?}", String::from_utf8_lossy(&bytes));
    }
});
