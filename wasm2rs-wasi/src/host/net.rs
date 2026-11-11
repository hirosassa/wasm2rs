//! `wasi:sockets`: TCP and UDP sockets, address checks and the network error mapping.

use super::*;

// Sockets: the types and helpers below use `std` only.

/// The `wasi:sockets` `error-code`s this host returns; `From<Net> for NetErrorCode` below.
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) enum Net {
    Unknown,
    AccessDenied,
    NotSupported,
    InvalidArgument,
    Timeout,
    NotInProgress,
    WouldBlock,
    InvalidState,
    AddressNotBindable,
    AddressInUse,
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    DatagramTooLarge,
    PermanentResolverFailure,
    NameUnresolvable,
}

pub(super) fn net_error(e: &std::io::Error) -> Net {
    use std::io::ErrorKind::*;
    match e.kind() {
        AddrInUse => Net::AddressInUse,
        AddrNotAvailable => Net::AddressNotBindable,
        PermissionDenied => Net::AccessDenied,
        ConnectionRefused => Net::ConnectionRefused,
        ConnectionReset => Net::ConnectionReset,
        ConnectionAborted => Net::ConnectionAborted,
        TimedOut => Net::Timeout,
        WouldBlock => Net::WouldBlock,
        InvalidInput => Net::InvalidArgument,
        NotConnected => Net::InvalidState,
        _ => Net::Unknown,
    }
}

/// Where a TCP socket is in its life (see the interface's state diagram). The socket
/// is a `std` listener from `finish-bind` on, a stream once connected.
pub(super) enum NetState {
    Unbound,
    BindStarted(SocketAddr),
    /// Bound: `std` has bound and listened, but the guest has not asked to listen yet.
    Bound(TcpListener),
    ListenStarted(TcpListener),
    /// Listening (non-blocking); `pending` is a connection a `subscribe` poll took off the
    /// queue to find out that one had arrived, for `accept` to return.
    Listening {
        listener: TcpListener,
        pending: std::cell::RefCell<Option<TcpStream>>,
    },
    /// `start-connect` has connected (or failed): `finish-connect` reports it.
    ConnectStarted(Result<TcpStream, Net>),
    /// Connected: `out` is the slot in `tcp_out` of the send queue of the output stream
    /// opened with the connection, and that queue's `id` (the slot is reused once the
    /// stream is dropped, by another connection's queue).
    Connected {
        stream: TcpStream,
        out: (usize, u64),
    },
    /// A connection attempt failed.
    Closed,
}

/// The TCP socket options a guest can set and read back. They are only recorded: `std::net`
/// has no API for most of them, so none reaches the OS socket. The defaults are the usual
/// ones of a Linux socket.
#[derive(Clone)]
pub(super) struct TcpOptions {
    pub(super) keep_alive_enabled: bool,
    /// Nanoseconds.
    pub(super) keep_alive_idle_time: u64,
    /// Nanoseconds.
    pub(super) keep_alive_interval: u64,
    pub(super) keep_alive_count: u32,
    pub(super) hop_limit: u8,
    pub(super) receive_buffer_size: u64,
    pub(super) send_buffer_size: u64,
}

impl Default for TcpOptions {
    fn default() -> Self {
        TcpOptions {
            keep_alive_enabled: false,
            keep_alive_idle_time: 7200 * 1_000_000_000,
            keep_alive_interval: 75 * 1_000_000_000,
            keep_alive_count: 9,
            hop_limit: 64,
            receive_buffer_size: 65536,
            send_buffer_size: 65536,
        }
    }
}

pub(super) struct NetSocket {
    pub(super) ipv6: bool,
    pub(super) state: NetState,
    pub(super) options: TcpOptions,
}

/// How many bytes a connection's send queue holds at most.
pub(super) const SEND_QUEUE_CAPACITY: usize = 64 * 1024;

/// The output side of a connection: the non-blocking socket and the host's send queue in front
/// of it. Bytes a guest writes are appended to the queue and moved to the socket as far as it
/// takes them, so a peer that does not read fills the queue (`check-write` reports 0, the
/// pollable is not ready) instead of making `write` wait. A failure of the socket (a dead
/// peer) marks the stream failed and drops the queue; the first operation that finds out answers
/// `last-operation-failed` (with an `error` handle), then the stream answers `closed`. The
/// flushing methods (`flush_waiting`, `flush_until`) wait for the peer to read, so they do not
/// return while a live peer never does. The queue is behind a `RefCell` because a pollable
/// asks for readiness through `&WasiHost`.
pub(super) struct TcpOut {
    /// Unique among the host's connections (see `NetState::Connected`).
    pub(super) id: u64,
    pub(super) stream: TcpStream,
    pub(super) queue: std::cell::RefCell<VecDeque<u8>>,
    /// Why the socket failed: the stream is marked failed (this is `Some`) and the queue was
    /// dropped. The first operation that finds out takes it and answers
    /// `last-operation-failed`.
    pub(super) failure: std::cell::RefCell<Option<String>>,
    /// The failure was reported: from then on the stream answers `closed`.
    pub(super) reported: std::cell::Cell<bool>,
    /// A `flush` found bytes still queued: until they are all sent, `check-write` is 0.
    pub(super) flush_pending: std::cell::Cell<bool>,
}

impl TcpOut {
    pub(super) fn new(id: u64, stream: TcpStream) -> Self {
        TcpOut {
            id,
            stream,
            queue: std::cell::RefCell::new(VecDeque::new()),
            failure: std::cell::RefCell::new(None),
            reported: std::cell::Cell::new(false),
            flush_pending: std::cell::Cell::new(false),
        }
    }

    /// Moves as much of the queue to the socket as it takes now.
    pub(super) fn drain(&self) {
        use std::io::ErrorKind;
        let mut queue = self.queue.borrow_mut();
        while !queue.is_empty() {
            let result = (&self.stream).write(queue.as_slices().0);
            match result {
                Ok(0) => {
                    return self.fail(&mut queue, "the connection accepts no more bytes".into());
                }
                Ok(n) => drop(queue.drain(..n)),
                Err(e) if e.kind() == ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return self.fail(&mut queue, e.to_string()),
            }
        }
        self.flush_pending.set(false);
    }

    pub(super) fn fail(&self, queue: &mut VecDeque<u8>, why: String) {
        queue.clear();
        *self.failure.borrow_mut() = Some(why);
    }

    /// Why the socket failed, once: the first call after the failure returns the reason (the
    /// caller answers `last-operation-failed`), then the stream is `closed`.
    pub(super) fn take_failure(&self) -> Option<String> {
        let why = self.failure.borrow_mut().take();
        if why.is_some() {
            self.reported.set(true);
        }
        why
    }

    /// Whether the socket failed (reported or not).
    pub(super) fn failed(&self) -> bool {
        self.failure.borrow().is_some() || self.reported.get()
    }

    /// What `check-write` permits after a drain: 4096 at most, less with bytes queued, 0 while
    /// the queue is full or a flush is pending; `None` once the stream failed.
    pub(super) fn permit(&self) -> Option<u64> {
        self.drain();
        if self.failed() {
            return None;
        }
        if self.flush_pending.get() {
            return Some(0);
        }
        let room = SEND_QUEUE_CAPACITY.saturating_sub(self.queue.borrow().len());
        Some(room.min(4096) as u64)
    }

    /// Appends `bytes` behind the ones queued, then drains. Never waits.
    pub(super) fn push(&self, bytes: &[u8]) {
        if !self.failed() {
            self.queue.borrow_mut().extend(bytes);
            self.drain();
        }
    }

    /// `flush`: drains as far as the socket takes; if bytes remain the flush is pending.
    pub(super) fn flush(&self) {
        self.drain();
        if !self.failed() && !self.queue.borrow().is_empty() {
            self.flush_pending.set(true);
        }
    }

    /// Drains the whole queue, waiting while the socket is full, until `deadline` if there is
    /// one (what is left stays queued).
    pub(super) fn flush_until(&self, deadline: Option<Instant>) {
        loop {
            self.drain();
            if self.failed() || self.queue.borrow().is_empty() {
                return;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(super) fn flush_waiting(&self) {
        self.flush_until(None);
    }
}

impl WasiHost {
    /// Lets the guest use addresses outside loopback (the default is 127.0.0.0/8 and ::1).
    pub fn with_external_network(mut self) -> Self {
        self.network = NetworkPolicy::External;
        self
    }

    /// Whether `accept` on the socket `r` refers to would not block (or the socket is not
    /// listening, so `accept` answers at once). A connection found here is kept for `accept`.
    pub(super) fn accept_ready(&self, r: SlotRef) -> bool {
        let Some(NetSocket {
            state: NetState::Listening { listener, pending },
            ..
        }) = self.sockets.get_ref(r)
        else {
            return true;
        };
        if pending.borrow().is_some() {
            return true;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                *pending.borrow_mut() = Some(stream);
                true
            }
            Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
        }
    }

    /// Whether the input stream in `tcp_in` that `r` refers to has data, is at its end or failed.
    pub(super) fn readable(&self, r: SlotRef) -> bool {
        let Some(stream) = self.tcp_in.get_ref(r) else {
            return true;
        };
        match stream.peek(&mut [0u8; 1]) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
        }
    }

    /// What `check-write` of the output stream in `tcp_out` that `r` refers to permits (see
    /// `TcpOut::permit`); `None` when the stream is dropped or failed.
    pub(super) fn tcp_permit(&self, slot: usize) -> Option<u64> {
        self.tcp_out.get(slot)?.permit()
    }

    /// Best effort, at exit and when the host is dropped: sends what the connections' send
    /// queues still hold, waiting for the peers to read for up to 5 seconds in all.
    pub(super) fn flush_all_connections(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        for conn in self.tcp_out.slots.iter().flatten() {
            conn.flush_until(Some(deadline));
        }
    }

    /// Whether the output stream in `tcp_out` that `r` refers to is writable (or failed or dropped).
    pub(super) fn tcp_writable(&self, r: SlotRef) -> bool {
        self.tcp_out.get_ref(r).is_none() || self.tcp_permit(r.slot) != Some(0)
    }

    /// The socket behind a handle.
    pub(super) fn socket(&mut self, handle: u32) -> &mut NetSocket {
        self.sockets
            .get_mut(handle as usize)
            .expect("unknown or dropped tcp-socket")
    }

    /// `check_address` for the address of a peer to connect or send to: also not port 0 nor an
    /// unspecified address (`0.0.0.0`, `::`), which is `invalid-argument` whether or not the
    /// network is allowed. Every refusal of `check_address` that is not about the policy is
    /// `invalid-argument` too, so the order between the two sets of checks cannot be seen.
    pub(super) fn check_peer(&self, ipv6: bool, addr: &SocketAddr) -> Result<(), Net> {
        if addr.port() == 0 || addr.ip().is_unspecified() {
            return Err(Net::InvalidArgument);
        }
        self.check_address(ipv6, addr)
    }

    /// `Ok` if the guest may use `addr` on a socket of the family `ipv6`: it must be of
    /// that family, a unicast address that is not an IPv4-mapped IPv6 one, and loopback
    /// unless the host allows the network.
    pub(super) fn check_address(&self, ipv6: bool, addr: &SocketAddr) -> Result<(), Net> {
        let mapped = matches!(addr, SocketAddr::V6(a) if a.ip().to_ipv4_mapped().is_some());
        if addr.is_ipv6() != ipv6 || mapped || addr.ip().is_multicast() {
            return Err(Net::InvalidArgument);
        }
        let allowed = match self.network {
            NetworkPolicy::None => false,
            NetworkPolicy::Loopback => addr.ip().is_loopback(),
            NetworkPolicy::External => true,
        };
        if allowed {
            Ok(())
        } else {
            Err(Net::AccessDenied)
        }
    }

    /// The slots in `tcp_in` and `tcp_out` of new streams over duplicates of `stream`, and
    /// the `Connected` state that owns `stream`.
    pub(super) fn open_tcp_streams(
        &mut self,
        stream: TcpStream,
    ) -> Result<(usize, usize, NetState), Net> {
        let dup = |s: &TcpStream| s.try_clone().map_err(|e| net_error(&e));
        let (input, output) = (dup(&stream)?, dup(&stream)?);
        let id = self.next_tcp_out_id;
        self.next_tcp_out_id += 1;
        let out = self.tcp_out.insert(TcpOut::new(id, output));
        let state = NetState::Connected {
            stream,
            out: (out, id),
        };
        Ok((
            TCP_STREAM + self.tcp_in.insert(input),
            TCP_STREAM + out,
            state,
        ))
    }

    pub(super) fn tcp_start_bind(&mut self, handle: u32, addr: SocketAddr) -> Result<(), Net> {
        let socket = self.socket(handle);
        if !matches!(socket.state, NetState::Unbound) {
            return Err(Net::InvalidState);
        }
        let ipv6 = socket.ipv6;
        self.check_address(ipv6, &addr)?;
        self.socket(handle).state = NetState::BindStarted(addr);
        Ok(())
    }

    pub(super) fn tcp_finish_bind(&mut self, handle: u32) -> Result<(), Net> {
        let socket = self.socket(handle);
        match std::mem::replace(&mut socket.state, NetState::Unbound) {
            NetState::BindStarted(addr) => {
                // a failed bind leaves the socket unbound, to try again
                socket.state = NetState::Bound(TcpListener::bind(addr).map_err(|e| net_error(&e))?);
                Ok(())
            }
            other => {
                socket.state = other;
                Err(Net::NotInProgress)
            }
        }
    }

    pub(super) fn tcp_start_listen(&mut self, handle: u32) -> Result<(), Net> {
        let socket = self.socket(handle);
        match std::mem::replace(&mut socket.state, NetState::Unbound) {
            NetState::Bound(listener) => {
                socket.state = NetState::ListenStarted(listener);
                Ok(())
            }
            other => {
                socket.state = other;
                Err(Net::InvalidState)
            }
        }
    }

    pub(super) fn tcp_finish_listen(&mut self, handle: u32) -> Result<(), Net> {
        let socket = self.socket(handle);
        match std::mem::replace(&mut socket.state, NetState::Unbound) {
            NetState::ListenStarted(listener) => {
                let nonblocking = listener.set_nonblocking(true);
                socket.state = NetState::Listening {
                    listener,
                    pending: std::cell::RefCell::new(None),
                };
                nonblocking.map_err(|e| net_error(&e))
            }
            other => {
                socket.state = other;
                Err(Net::NotInProgress)
            }
        }
    }

    /// Connects now (see the header): the result waits in the socket for `finish-connect`.
    pub(super) fn tcp_start_connect(&mut self, handle: u32, addr: SocketAddr) -> Result<(), Net> {
        let socket = self.socket(handle);
        let ipv6 = socket.ipv6;
        match socket.state {
            NetState::Unbound => {}
            NetState::BindStarted(_) | NetState::Bound(_) => return Err(Net::NotSupported),
            _ => return Err(Net::InvalidState),
        }
        self.check_peer(ipv6, &addr)?;
        let connected = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
            .and_then(|stream| stream.set_nonblocking(true).map(|()| stream))
            .map_err(|e| net_error(&e));
        self.socket(handle).state = NetState::ConnectStarted(connected);
        Ok(())
    }

    /// The slots of the connection's input and output streams (see `TCP_STREAM`).
    pub(super) fn tcp_finish_connect(&mut self, handle: u32) -> Result<(usize, usize), Net> {
        let socket = self.socket(handle);
        let stream = match std::mem::replace(&mut socket.state, NetState::Closed) {
            NetState::ConnectStarted(Ok(stream)) => stream,
            NetState::ConnectStarted(Err(c)) => return Err(c),
            other => {
                socket.state = other;
                return Err(Net::NotInProgress);
            }
        };
        let (input, output, state) = self.open_tcp_streams(stream)?;
        self.socket(handle).state = state;
        Ok((input, output))
    }

    /// The new socket's slot and the stream handles (see `TCP_STREAM`).
    pub(super) fn tcp_accept(&mut self, handle: u32) -> Result<(usize, usize, usize), Net> {
        let socket = self.socket(handle);
        let ipv6 = socket.ipv6;
        let options = socket.options.clone();
        let NetState::Listening { listener, pending } = &socket.state else {
            return Err(Net::InvalidState);
        };
        let queued = pending.borrow_mut().take();
        let stream = match queued {
            Some(stream) => stream,
            None => listener
                .accept()
                .map(|(stream, _)| stream)
                .map_err(|e| net_error(&e))?,
        };
        stream.set_nonblocking(true).map_err(|e| net_error(&e))?;
        let (input, output, state) = self.open_tcp_streams(stream)?;
        let slot = self.sockets.insert(NetSocket {
            ipv6,
            state,
            options,
        });
        Ok((slot, input, output))
    }

    /// The local address of a bound socket, the remote one (`remote`) of a connected one.
    pub(super) fn tcp_address(&mut self, handle: u32, remote: bool) -> Result<SocketAddr, Net> {
        match (&self.socket(handle).state, remote) {
            (NetState::Connected { stream, .. }, true) => stream.peer_addr(),
            (NetState::Connected { stream, .. }, false) => stream.local_addr(),
            (
                NetState::Bound(l)
                | NetState::ListenStarted(l)
                | NetState::Listening { listener: l, .. },
                false,
            ) => l.local_addr(),
            _ => Err(std::io::ErrorKind::NotConnected.into()),
        }
        .map_err(|e| net_error(&e))
    }

    pub(super) fn tcp_shutdown(&mut self, handle: u32, how: Shutdown) -> Result<(), Net> {
        let (stream, out) = match &self.socket(handle).state {
            NetState::Connected { stream, out } => {
                (stream.try_clone().map_err(|e| net_error(&e))?, *out)
            }
            _ => return Err(Net::InvalidState),
        };
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            // what the guest wrote before still reaches the peer, in order (a dead peer
            // discards it); the stream may be dropped already
            if let Some(conn) = self.tcp_out.get(out.0).filter(|c| c.id == out.1) {
                conn.flush_waiting();
            }
        }
        stream.shutdown(how).map_err(|e| net_error(&e))
    }
}

/// Where a UDP socket is in its life: unbound, `start-bind` called, or bound (a non-blocking
/// `std` socket, shared with the streams made from it; `remote` is the default peer of the
/// last `stream` call).
pub(super) enum UdpState {
    Unbound,
    BindStarted(SocketAddr),
    Bound {
        socket: Arc<StdUdpSocket>,
        remote: Option<SocketAddr>,
    },
}

pub(super) struct UdpSock {
    pub(super) ipv6: bool,
    pub(super) state: UdpState,
    /// How many times `stream` was called; only the streams of the last call work (see `UdpEnd`).
    pub(super) generation: Arc<AtomicU64>,
}

/// One datagram stream over a bound socket: the peer it was made for (`stream(Some(..))`).
pub(super) struct UdpEnd {
    pub(super) ipv6: bool,
    pub(super) socket: Arc<StdUdpSocket>,
    pub(super) remote: Option<SocketAddr>,
    /// The socket's `generation` counter, and its value when this stream was made: a stream is
    /// stale once `stream` was called again.
    pub(super) current: Arc<AtomicU64>,
    pub(super) generation: u64,
}

impl UdpEnd {
    pub(super) fn is_stale(&self) -> bool {
        self.current.load(Ordering::SeqCst) != self.generation
    }

    /// Traps (the spec lets a replaced stream trap) if `stream` was called again since.
    pub(super) fn require_current(&self) {
        assert!(
            !self.is_stale(),
            "stale datagram stream: `stream` was called again"
        );
    }
}

/// Whether two addresses name the same endpoint: the IP and the port. (`SocketAddr`'s `==`
/// also compares an IPv6 address's flow info and scope id, which a sender's address may lack.)
pub(super) fn same_endpoint(a: SocketAddr, b: SocketAddr) -> bool {
    a.ip() == b.ip() && a.port() == b.port()
}

/// Whether a datagram from `from` is for a stream made for `remote` (any sender if none).
pub(super) fn from_peer(remote: Option<SocketAddr>, from: SocketAddr) -> bool {
    remote.is_none_or(|peer| same_endpoint(peer, from))
}

/// The most datagrams one `send` takes (what `check-send` permits).
pub(super) const UDP_SEND_BATCH: u64 = 1024;

/// The most bytes in a UDP datagram over IPv4 and over IPv6.
pub(super) const UDP_MAX_PAYLOAD: usize = 65507;
pub(super) const UDP_MAX_PAYLOAD_V6: usize = 65527;

/// Whether `name` (without a trailing dot) is a syntactically valid host name: dot-separated
/// labels of 1 to 63 letters, digits, `-` and `_` that do not start or end with `-`. A name of
/// only digits and dots is not (an address that failed to parse is not a name).
pub(super) fn is_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.chars().all(|c| c.is_ascii_digit() || c == '.')
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

impl WasiHost {
    /// The UDP socket behind a handle.
    pub(super) fn udp_socket(&mut self, handle: u32) -> &mut UdpSock {
        self.udp_sockets
            .get_mut(handle as usize)
            .expect("unknown or dropped udp-socket")
    }

    pub(super) fn udp_start_bind(&mut self, handle: u32, addr: SocketAddr) -> Result<(), Net> {
        let socket = self.udp_socket(handle);
        if !matches!(socket.state, UdpState::Unbound) {
            return Err(Net::InvalidState);
        }
        let ipv6 = socket.ipv6;
        self.check_address(ipv6, &addr)?;
        self.udp_socket(handle).state = UdpState::BindStarted(addr);
        Ok(())
    }

    pub(super) fn udp_finish_bind(&mut self, handle: u32) -> Result<(), Net> {
        let socket = self.udp_socket(handle);
        match std::mem::replace(&mut socket.state, UdpState::Unbound) {
            UdpState::BindStarted(addr) => {
                // a failed bind leaves the socket unbound, to try again
                let bound = StdUdpSocket::bind(addr).map_err(|e| net_error(&e))?;
                bound.set_nonblocking(true).map_err(|e| net_error(&e))?;
                socket.state = UdpState::Bound {
                    socket: Arc::new(bound),
                    remote: None,
                };
                Ok(())
            }
            other => {
                socket.state = other;
                Err(Net::NotInProgress)
            }
        }
    }

    /// The local address, or (`remote`) the default peer of a bound socket.
    pub(super) fn udp_address(&mut self, handle: u32, remote: bool) -> Result<SocketAddr, Net> {
        match &self.udp_socket(handle).state {
            UdpState::Bound { socket, .. } if !remote => {
                socket.local_addr().map_err(|e| net_error(&e))
            }
            UdpState::Bound {
                remote: Some(peer), ..
            } => Ok(*peer),
            _ => Err(Net::InvalidState),
        }
    }

    /// Makes the two streams of a bound socket (`remote`: the peer to talk to) and returns their
    /// slots in `udp_in` and `udp_out`.
    pub(super) fn udp_stream(
        &mut self,
        handle: u32,
        remote: Option<SocketAddr>,
    ) -> Result<(usize, usize), Net> {
        let ipv6 = self.udp_socket(handle).ipv6;
        if let Some(peer) = &remote {
            self.check_peer(ipv6, peer)?;
        }
        let sock = self.udp_socket(handle);
        let UdpState::Bound {
            socket,
            remote: default_peer,
        } = &mut sock.state
        else {
            return Err(Net::InvalidState);
        };
        *default_peer = remote;
        let socket = Arc::clone(socket);
        // the streams of earlier calls are stale from now on
        let current = Arc::clone(&sock.generation);
        let generation = current.fetch_add(1, Ordering::SeqCst) + 1;
        let end = || UdpEnd {
            ipv6,
            socket: Arc::clone(&socket),
            remote,
            current: Arc::clone(&current),
            generation,
        };
        let (input, output) = (end(), end());
        Ok((self.udp_in.insert(input), self.udp_out.insert(output)))
    }

    /// Whether the incoming stream `r` refers to has a datagram `receive` would return, or failed.
    /// A datagram from another sender than a connected stream's peer is dropped here. A stale
    /// stream is always ready (so a waiting guest wakes up and traps) and takes nothing.
    pub(super) fn udp_readable(&self, r: SlotRef) -> bool {
        let Some(end) = self.udp_in.get_ref(r) else {
            return true;
        };
        if end.is_stale() {
            return true;
        }
        let mut byte = [0u8; 1];
        loop {
            match end.socket.peek_from(&mut byte) {
                Ok((_, from)) if from_peer(end.remote, from) => return true,
                Ok(_) => {
                    // not for this stream: take it off the queue
                    let _ = end.socket.recv_from(&mut byte);
                }
                Err(e) => return e.kind() != std::io::ErrorKind::WouldBlock,
            }
        }
    }

    /// At most `max` queued datagrams for the incoming stream at `slot`, with their sources; an
    /// error shows only if there is nothing to return.
    pub(super) fn udp_receive(
        &mut self,
        slot: u32,
        max: u64,
    ) -> Result<Vec<(Vec<u8>, SocketAddr)>, Net> {
        let end = self
            .udp_in
            .get(slot as usize)
            .expect("unknown or dropped incoming-datagram-stream");
        end.require_current();
        let mut got = Vec::new();
        let mut buf = vec![0u8; 65536];
        while (got.len() as u64) < max {
            match end.socket.recv_from(&mut buf) {
                Ok((n, from)) if from_peer(end.remote, from) => got.push((buf[..n].to_vec(), from)),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if got.is_empty() => return Err(net_error(&e)),
                Err(_) => break,
            }
        }
        Ok(got)
    }

    /// Traps if the outgoing stream at `slot` is stale (`check-send`).
    pub(super) fn udp_check_send(&self, slot: u32) {
        let end = self
            .udp_out
            .get(slot as usize)
            .expect("unknown or dropped outgoing-datagram-stream");
        end.require_current();
    }

    /// Sends the datagrams (data, address) over the outgoing stream at `slot` in order, stopping
    /// at the first the socket will not take now: the count sent. As the spec requires, once a
    /// datagram was sent no error is returned: a later datagram that fails the address or size
    /// rules ends the batch like a full buffer; if the first one fails, that is the error.
    pub(super) fn udp_send(
        &mut self,
        slot: u32,
        datagrams: Vec<(Vec<u8>, Option<SocketAddr>)>,
    ) -> Result<u64, Net> {
        assert!(
            datagrams.len() as u64 <= UDP_SEND_BATCH,
            "send: more datagrams than check-send permits"
        );
        let end = self
            .udp_out
            .get(slot as usize)
            .expect("unknown or dropped outgoing-datagram-stream");
        end.require_current();
        let max = if end.ipv6 {
            UDP_MAX_PAYLOAD_V6
        } else {
            UDP_MAX_PAYLOAD
        };
        let mut sent = 0u64;
        for (data, address) in datagrams {
            let destination = || -> Result<SocketAddr, Net> {
                let to = match (end.remote, address) {
                    (Some(peer), None) => peer,
                    (Some(peer), Some(a)) if same_endpoint(peer, a) => peer,
                    (Some(_), Some(_)) | (None, None) => return Err(Net::InvalidArgument),
                    (None, Some(a)) => a,
                };
                self.check_peer(end.ipv6, &to)?;
                if data.len() > max {
                    Err(Net::DatagramTooLarge)
                } else {
                    Ok(to)
                }
            };
            let to = match destination() {
                Ok(to) => to,
                Err(_) if sent > 0 => break,
                Err(e) => return Err(e),
            };
            match end.socket.send_to(&data, to) {
                Ok(_) => sent += 1,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if sent == 0 => return Err(net_error(&e)),
                Err(_) => break,
            }
        }
        Ok(sent)
    }

    /// The addresses of `name` for `resolve-addresses`: see the header.
    pub(super) fn resolve_name(&self, name: &str) -> Result<VecDeque<IpAddr>, Net> {
        if let Ok(ip) = name.parse::<IpAddr>() {
            return Ok(VecDeque::from([ip]));
        }
        let host = name.strip_suffix('.').unwrap_or(name);
        if !is_host_name(host) {
            return Err(Net::InvalidArgument);
        }
        if host.eq_ignore_ascii_case("localhost") {
            return Ok(VecDeque::from([
                Ipv4Addr::LOCALHOST.into(),
                Ipv6Addr::LOCALHOST.into(),
            ]));
        }
        if self.network != NetworkPolicy::External {
            return Err(Net::PermanentResolverFailure);
        }
        let mut found = VecDeque::new();
        for addr in (host, 0)
            .to_socket_addrs()
            .map_err(|_| Net::NameUnresolvable)?
        {
            if !found.contains(&addr.ip()) {
                found.push_back(addr.ip());
            }
        }
        if found.is_empty() {
            Err(Net::NameUnresolvable)
        } else {
            Ok(found)
        }
    }
}

impl From<Net> for NetErrorCode {
    fn from(e: Net) -> Self {
        match e {
            Net::Unknown => NetErrorCode::Unknown,
            Net::AccessDenied => NetErrorCode::AccessDenied,
            Net::NotSupported => NetErrorCode::NotSupported,
            Net::InvalidArgument => NetErrorCode::InvalidArgument,
            Net::Timeout => NetErrorCode::Timeout,
            Net::NotInProgress => NetErrorCode::NotInProgress,
            Net::WouldBlock => NetErrorCode::WouldBlock,
            Net::InvalidState => NetErrorCode::InvalidState,
            Net::AddressNotBindable => NetErrorCode::AddressNotBindable,
            Net::AddressInUse => NetErrorCode::AddressInUse,
            Net::ConnectionRefused => NetErrorCode::ConnectionRefused,
            Net::ConnectionReset => NetErrorCode::ConnectionReset,
            Net::ConnectionAborted => NetErrorCode::ConnectionAborted,
            Net::DatagramTooLarge => NetErrorCode::DatagramTooLarge,
            Net::PermanentResolverFailure => NetErrorCode::PermanentResolverFailure,
            Net::NameUnresolvable => NetErrorCode::NameUnresolvable,
        }
    }
}

pub(super) fn socket_addr(addr: IpSocketAddress) -> SocketAddr {
    match addr {
        IpSocketAddress::Ipv4(a) => {
            let (a0, a1, a2, a3) = a.address;
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a0, a1, a2, a3), a.port))
        }
        IpSocketAddress::Ipv6(a) => {
            let (a0, a1, a2, a3, a4, a5, a6, a7) = a.address;
            let ip = Ipv6Addr::new(a0, a1, a2, a3, a4, a5, a6, a7);
            SocketAddr::V6(SocketAddrV6::new(ip, a.port, a.flow_info, a.scope_id))
        }
    }
}

pub(super) fn ip_socket_address(addr: SocketAddr) -> IpSocketAddress {
    match addr {
        SocketAddr::V4(a) => {
            let [a0, a1, a2, a3] = a.ip().octets();
            IpSocketAddress::Ipv4(Ipv4SocketAddress {
                port: a.port(),
                address: (a0, a1, a2, a3),
            })
        }
        SocketAddr::V6(a) => {
            let [a0, a1, a2, a3, a4, a5, a6, a7] = a.ip().segments();
            IpSocketAddress::Ipv6(Ipv6SocketAddress {
                port: a.port(),
                flow_info: a.flowinfo(),
                address: (a0, a1, a2, a3, a4, a5, a6, a7),
                scope_id: a.scope_id(),
            })
        }
    }
}

pub(super) fn ip_address(addr: IpAddr) -> IpAddress {
    match addr {
        IpAddr::V4(a) => {
            let [a0, a1, a2, a3] = a.octets();
            IpAddress::Ipv4((a0, a1, a2, a3))
        }
        IpAddr::V6(a) => {
            let [a0, a1, a2, a3, a4, a5, a6, a7] = a.segments();
            IpAddress::Ipv6((a0, a1, a2, a3, a4, a5, a6, a7))
        }
    }
}
