//! Loopback plumbing: a connected pair of `TcpStream`s, one fed by a thread.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::thread;

fn listener() -> &'static TcpListener {
    static LISTENER: OnceLock<TcpListener> = OnceLock::new();
    LISTENER.get_or_init(|| TcpListener::bind("127.0.0.1:0").expect("bind loopback"))
}

/// `SO_LINGER` with a zero timeout (`TcpStream::set_linger` is nightly-only).
fn abortive_close(s: &TcpStream) {
    use std::os::fd::AsRawFd;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: a valid socket fd and a `linger` of the size passed.
    let rc = unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&raw const linger).cast(),
            size_of::<libc::linger>() as libc::socklen_t,
        )
    };
    assert_eq!(rc, 0, "setsockopt(SO_LINGER)");
}

/// A connected pair `(client, server)`. Closing is abortive (linger 0): a fuzzer makes
/// thousands of connections a second, and TIME_WAIT would exhaust the ephemeral ports.
pub fn pair() -> (TcpStream, TcpStream) {
    // one connect+accept at a time: tests run in parallel and would cross their pairs
    static PAIRING: Mutex<()> = Mutex::new(());
    let _pairing = PAIRING.lock().unwrap_or_else(|e| e.into_inner());
    let l = listener();
    let client = TcpStream::connect(l.local_addr().expect("addr")).expect("connect");
    let (server, _) = l.accept().expect("accept");
    for s in [&client, &server] {
        abortive_close(s);
        s.set_nodelay(true).expect("nodelay");
    }
    (client, server)
}

/// Gives `server_side` a server stream whose peer sends `bytes` and then half-closes; returns
/// what `server_side` returns. Writing from a thread keeps a large input from blocking.
pub fn serve_bytes<R>(bytes: &[u8], server_side: impl FnOnce(&mut TcpStream) -> R) -> R {
    let (mut client, mut server) = pair();
    thread::scope(move |scope| {
        scope.spawn(move || {
            // the server may reject early and close: a failed write is fine
            let _ = client.write_all(bytes);
            let _ = client.shutdown(Shutdown::Write);
            // keep the socket until the server is done with it
            let mut sink = Vec::new();
            let _ = client.read_to_end(&mut sink);
        });
        let out = server_side(&mut server);
        // closed here, not when the scope is left: the peer thread waits for it
        drop(server);
        out
    })
}

/// Runs `server_side` on a server stream and returns everything the peer received until the
/// server side closed.
pub fn capture<R>(server_side: impl FnOnce(&mut TcpStream) -> R) -> (R, Vec<u8>) {
    let (mut client, mut server) = pair();
    thread::scope(move |scope| {
        let reader = scope.spawn(move || {
            let mut out = Vec::new();
            let _ = client.read_to_end(&mut out);
            out
        });
        let result = server_side(&mut server);
        // a graceful FIN first: with linger 0 a plain close would reset the connection and
        // drop what the peer has not read yet, cutting the capture short
        let _ = server.shutdown(Shutdown::Write);
        let received = reader.join().expect("reader");
        drop(server);
        (result, received)
    })
}
