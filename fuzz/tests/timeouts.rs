//! The time limits of the server, which the fuzz targets cannot reach (their peer always
//! closes): a client that stalls, trickles or keeps sending must be cut off.

use std::io::{Read, Write};
use std::time::{Duration, Instant};
use wasm2rs_fuzz_serve::respond::{self, Plan};
use wasm2rs_fuzz_serve::{api, net};

/// Runs `read_request` with `deadline_ms` against a peer driven by `peer`, which keeps the
/// connection open until the reader is done.
fn read_with(
    deadline_ms: u64,
    peer: impl FnOnce(&mut std::net::TcpStream) + Send,
) -> (Result<(), api::Rejection>, Duration) {
    let (mut client, mut server) = net::pair();
    std::thread::scope(|scope| {
        let driver = scope.spawn(move || {
            peer(&mut client);
            let mut sink = Vec::new();
            let _ = client.read_to_end(&mut sink);
        });
        let start = Instant::now();
        let result =
            api::read_request(&mut server, start + Duration::from_millis(deadline_ms)).map(|_| ());
        let took = start.elapsed();
        drop(server);
        driver.join().expect("peer thread");
        (result, took)
    })
}

#[test]
fn a_silent_client_is_a_408() {
    let (result, took) = read_with(200, |_| {});
    assert_eq!(result.unwrap_err().0, 408);
    assert!(
        took >= Duration::from_millis(150) && took < Duration::from_secs(3),
        "{took:?}"
    );
}

#[test]
fn a_stalled_head_is_a_408() {
    let (result, took) = read_with(200, |c| {
        c.write_all(b"GET / HTT").unwrap();
    });
    assert_eq!(result.unwrap_err().0, 408);
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn a_stalled_body_is_a_408() {
    let (result, took) = read_with(200, |c| {
        c.write_all(b"POST / HTTP/1.1\r\ncontent-length: 10\r\n\r\nabc")
            .unwrap();
    });
    assert_eq!(result.unwrap_err().0, 408);
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn a_trickle_does_not_extend_the_deadline() {
    // a byte every 20 ms forever never completes the head: the deadline is on the whole
    // request, not on each read
    let (result, took) = read_with(300, |c| {
        for b in b"GET /slow HTTP/1.1\r\nx: ".iter().cycle().take(400) {
            if c.write_all(&[*b]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    assert_eq!(result.unwrap_err().0, 408);
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[test]
fn draining_a_rejected_client_is_bounded() {
    // the client goes on sending (and never closes): `drain` gives up after DRAIN_TIME
    let (mut client, mut server) = net::pair();
    let sender = std::thread::spawn(move || {
        let chunk = [b'x'; 1024];
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(4) {
            if client.write_all(&chunk).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let start = Instant::now();
    api::drain(&mut server);
    let took = start.elapsed();
    drop(server);
    sender.join().unwrap();
    assert!(
        took >= api::DRAIN_TIME - Duration::from_millis(100)
            && took < api::DRAIN_TIME + Duration::from_secs(1),
        "{took:?}"
    );
}

#[test]
fn an_open_guest_body_is_cut_after_the_idle_time_without_the_closing_chunk() {
    let plan = Plan {
        status: 200,
        body: b"partial".to_vec(),
        open: true,
        ..Plan::default()
    };
    let served = respond::served(&mut respond::host(), &plan).expect("response");
    let start = Instant::now();
    let bytes = respond::wire(served, false, false, Duration::from_millis(150));
    assert!(start.elapsed() < Duration::from_secs(3));
    let wire = respond::parse_head(&bytes).unwrap();
    assert!(
        wire.headers
            .iter()
            .any(|(n, v)| n == "transfer-encoding" && v == b"chunked")
    );
    // one chunk, then the truncation (no `0` chunk) is the signal
    assert_eq!(wire.payload, b"7\r\npartial\r\n");
}

#[test]
fn a_late_but_in_time_request_is_taken() {
    let (result, _) = read_with(2000, |c| {
        c.write_all(b"GET / HTTP/1.1\r\n").unwrap();
        std::thread::sleep(Duration::from_millis(150));
        c.write_all(b"\r\n").unwrap();
    });
    assert!(result.is_ok());
}
