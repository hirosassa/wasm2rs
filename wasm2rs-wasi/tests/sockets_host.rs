//! TCP, UDP and name lookup of `WasiHost` over real loopback sockets, driven through the
//! `Host` trait: the state machine of a socket, which address each policy lets through, data
//! across a connection, and the pollables that report readiness.

mod common;

use common::{builder, traps};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use wasm2rs_wasi::wasi_io_streams::{InputStream, OutputStream, StreamError};
use wasm2rs_wasi::wasi_sockets_network::{
    ErrorCode as Net, IpAddress, IpAddressFamily, IpSocketAddress, Ipv4SocketAddress,
    Ipv6SocketAddress, Network,
};
use wasm2rs_wasi::wasi_sockets_tcp::{ShutdownType, TcpSocket};
use wasm2rs_wasi::wasi_sockets_udp::{
    IncomingDatagram, IncomingDatagramStream, OutgoingDatagram, UdpSocket,
};
use wasm2rs_wasi::{Host, NetworkPolicy, WasiHost};

const NET: Network = Network(0);

fn host(policy: NetworkPolicy) -> WasiHost {
    builder().network(policy).build().unwrap()
}

fn v4(address: (u8, u8, u8, u8), port: u16) -> IpSocketAddress {
    IpSocketAddress::Ipv4(Ipv4SocketAddress { port, address })
}

fn lo(port: u16) -> IpSocketAddress {
    v4((127, 0, 0, 1), port)
}

fn lo6(port: u16) -> IpSocketAddress {
    IpSocketAddress::Ipv6(Ipv6SocketAddress {
        port,
        flow_info: 0,
        address: (0, 0, 0, 0, 0, 0, 0, 1),
        scope_id: 0,
    })
}

fn port_of(addr: IpSocketAddress) -> u16 {
    match addr {
        IpSocketAddress::Ipv4(a) => a.port,
        IpSocketAddress::Ipv6(a) => a.port,
    }
}

fn tcp(host: &mut WasiHost) -> TcpSocket {
    host.wasi_sockets_tcp_create_socket_create_tcp_socket(IpAddressFamily::Ipv4)
        .unwrap()
}

/// Held while a test needs a loopback port nobody else may take: the tests run in parallel
/// and an OS (Windows above all) hands a freed ephemeral port to the next listener at once.
static PORT_WINDOW: Mutex<()> = Mutex::new(());

fn port_window() -> MutexGuard<'static, ()> {
    // a test that panicked inside the window leaves nothing to repair
    PORT_WINDOW.lock().unwrap_or_else(|e| e.into_inner())
}

/// A listener on an ephemeral loopback port: the socket and its port.
fn listener(host: &mut WasiHost) -> (TcpSocket, u16) {
    let _window = port_window();
    listener_in_window(host)
}

/// For a caller that already holds `port_window` (the lock is not reentrant).
fn listener_in_window(host: &mut WasiHost) -> (TcpSocket, u16) {
    let s = tcp(host);
    host.wasi_sockets_tcp_method_tcp_socket_start_bind(s, NET, lo(0))
        .unwrap();
    host.wasi_sockets_tcp_method_tcp_socket_finish_bind(s)
        .unwrap();
    host.wasi_sockets_tcp_method_tcp_socket_start_listen(s)
        .unwrap();
    host.wasi_sockets_tcp_method_tcp_socket_finish_listen(s)
        .unwrap();
    let port = port_of(
        host.wasi_sockets_tcp_method_tcp_socket_local_address(s)
            .unwrap(),
    );
    (s, port)
}

fn accept_blocking(
    host: &mut WasiHost,
    server: TcpSocket,
) -> (TcpSocket, InputStream, OutputStream) {
    let p = host.wasi_sockets_tcp_method_tcp_socket_subscribe(server);
    host.wasi_io_poll_method_pollable_block(p);
    host.wasi_sockets_tcp_method_tcp_socket_accept(server)
        .unwrap()
}

/// A connected pair over one host: the client socket and its streams, the accepted socket
/// and its streams, and the listener's port.
struct Pair {
    host: WasiHost,
    port: u16,
    client: TcpSocket,
    c_in: InputStream,
    c_out: OutputStream,
    accepted: TcpSocket,
    s_in: InputStream,
    s_out: OutputStream,
}

fn pair() -> Pair {
    let mut host = host(NetworkPolicy::Loopback);
    let (server, port) = listener(&mut host);
    let client = tcp(&mut host);
    host.wasi_sockets_tcp_method_tcp_socket_start_connect(client, NET, lo(port))
        .unwrap();
    let (c_in, c_out) = host
        .wasi_sockets_tcp_method_tcp_socket_finish_connect(client)
        .unwrap();
    let (accepted, s_in, s_out) = accept_blocking(&mut host, server);
    Pair {
        host,
        port,
        client,
        c_in,
        c_out,
        accepted,
        s_in,
        s_out,
    }
}

// ---- TCP state machine ------------------------------------------------------------------

#[test]
fn a_tcp_socket_must_go_through_bind_listen_in_order() {
    // its listener takes an ephemeral port: see `port_window`
    let _window = port_window();
    let mut h = host(NetworkPolicy::Loopback);
    let s = tcp(&mut h);
    let start_bind =
        |h: &mut WasiHost, s| h.wasi_sockets_tcp_method_tcp_socket_start_bind(s, NET, lo(0));
    let finish_bind = |h: &mut WasiHost, s| h.wasi_sockets_tcp_method_tcp_socket_finish_bind(s);
    let start_listen = |h: &mut WasiHost, s| h.wasi_sockets_tcp_method_tcp_socket_start_listen(s);
    let finish_listen = |h: &mut WasiHost, s| h.wasi_sockets_tcp_method_tcp_socket_finish_listen(s);

    // a step out of order leaves the socket where it was
    assert_eq!(finish_bind(&mut h, s), Err(Net::NotInProgress));
    assert_eq!(start_listen(&mut h, s), Err(Net::InvalidState));
    assert_eq!(finish_listen(&mut h, s), Err(Net::NotInProgress));
    assert!(!h.wasi_sockets_tcp_method_tcp_socket_is_listening(s));
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_local_address(s),
        Err(Net::InvalidState),
        "not bound yet"
    );

    assert_eq!(start_bind(&mut h, s), Ok(()));
    assert_eq!(start_bind(&mut h, s), Err(Net::InvalidState), "twice");
    assert_eq!(
        start_listen(&mut h, s),
        Err(Net::InvalidState),
        "not bound yet"
    );
    assert_eq!(finish_bind(&mut h, s), Ok(()));
    assert_eq!(finish_bind(&mut h, s), Err(Net::NotInProgress), "twice");
    assert_eq!(finish_listen(&mut h, s), Err(Net::NotInProgress));
    assert!(
        h.wasi_sockets_tcp_method_tcp_socket_local_address(s)
            .is_ok()
    );
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_remote_address(s),
        Err(Net::InvalidState)
    );
    assert_eq!(start_listen(&mut h, s), Ok(()));
    assert_eq!(start_listen(&mut h, s), Err(Net::InvalidState), "twice");
    assert_eq!(finish_listen(&mut h, s), Ok(()));
    assert!(h.wasi_sockets_tcp_method_tcp_socket_is_listening(s));
    // a listener cannot connect, bind again or be shut down
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_connect(s, NET, lo(1)),
        Err(Net::InvalidState)
    );
    assert_eq!(start_bind(&mut h, s), Err(Net::InvalidState));
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_shutdown(s, ShutdownType::Both),
        Err(Net::InvalidState)
    );
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_finish_connect(s)
            .map(|_| ()),
        Err(Net::NotInProgress)
    );
}

#[test]
fn accept_without_a_pending_connection_would_block_and_a_non_listener_is_invalid() {
    let mut h = host(NetworkPolicy::Loopback);
    let (server, _) = listener(&mut h);
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_accept(server)
            .map(|_| ()),
        Err(Net::WouldBlock)
    );
    let idle = tcp(&mut h);
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_accept(idle)
            .map(|_| ()),
        Err(Net::InvalidState)
    );
}

#[test]
fn a_second_bind_to_a_taken_port_is_address_in_use_and_a_retry_is_possible() {
    let mut h = host(NetworkPolicy::Loopback);
    let (_server, port) = listener(&mut h);
    // the retry binds an ephemeral port: see `port_window`
    let _window = port_window();
    let s = tcp(&mut h);
    h.wasi_sockets_tcp_method_tcp_socket_start_bind(s, NET, lo(port))
        .unwrap();
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_finish_bind(s),
        Err(Net::AddressInUse)
    );
    // the failed bind left the socket unbound: it may try another address
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_bind(s, NET, lo(0)),
        Ok(())
    );
    assert_eq!(h.wasi_sockets_tcp_method_tcp_socket_finish_bind(s), Ok(()));
}

#[test]
fn connecting_to_a_closed_port_is_refused_at_finish_connect() {
    let mut h = host(NetworkPolicy::Loopback);
    // `connect` runs inside `start-connect`: until then no other listener may take the freed
    // port, or the connection would reach it (and the other test would accept this one)
    let window = port_window();
    let (server, port) = listener_in_window(&mut h);
    h.wasi_sockets_tcp_resource_drop_tcp_socket(server);
    let c = tcp(&mut h);
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_connect(c, NET, lo(port)),
        Ok(()),
        "the refusal shows in finish-connect"
    );
    drop(window);
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_finish_connect(c)
            .map(|_| ()),
        Err(Net::ConnectionRefused)
    );
}

#[test]
fn start_connect_validates_the_peer_address_before_connecting() {
    let mut h = host(NetworkPolicy::Loopback);
    let s = tcp(&mut h);
    let connect =
        |h: &mut WasiHost, a| h.wasi_sockets_tcp_method_tcp_socket_start_connect(s, NET, a);
    // port 0 and the unspecified address are invalid whatever the policy says
    assert_eq!(connect(&mut h, lo(0)), Err(Net::InvalidArgument));
    assert_eq!(
        connect(&mut h, v4((0, 0, 0, 0), 80)),
        Err(Net::InvalidArgument)
    );
    // a multicast address and a family that is not the socket's
    assert_eq!(
        connect(&mut h, v4((224, 0, 0, 1), 80)),
        Err(Net::InvalidArgument)
    );
    assert_eq!(connect(&mut h, lo6(80)), Err(Net::InvalidArgument));
    // an IPv4-mapped IPv6 address is not an IPv6 address for a v6 socket
    let v6 = h
        .wasi_sockets_tcp_create_socket_create_tcp_socket(IpAddressFamily::Ipv6)
        .unwrap();
    let mapped = IpSocketAddress::Ipv6(Ipv6SocketAddress {
        port: 80,
        flow_info: 0,
        address: (0, 0, 0, 0, 0, 0xffff, 0x7f00, 1),
        scope_id: 0,
    });
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_connect(v6, NET, mapped),
        Err(Net::InvalidArgument)
    );
    // outside the policy
    assert_eq!(
        connect(&mut h, v4((192, 0, 2, 1), 80)),
        Err(Net::AccessDenied)
    );
    // none of the refusals moved the socket out of `unbound`
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_finish_connect(s)
            .map(|_| ()),
        Err(Net::NotInProgress)
    );
    // a socket with a bind in progress cannot connect
    let b = tcp(&mut h);
    h.wasi_sockets_tcp_method_tcp_socket_start_bind(b, NET, lo(0))
        .unwrap();
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_connect(b, NET, lo(9)),
        Err(Net::NotSupported)
    );
}

#[test]
fn address_family_and_the_options_a_socket_reports() {
    let mut h = host(NetworkPolicy::Loopback);
    let v4s = tcp(&mut h);
    let v6s = h
        .wasi_sockets_tcp_create_socket_create_tcp_socket(IpAddressFamily::Ipv6)
        .unwrap();
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_address_family(v4s),
        IpAddressFamily::Ipv4
    );
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_address_family(v6s),
        IpAddressFamily::Ipv6
    );
    // an address of the other family cannot be bound
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_bind(v4s, NET, lo6(0)),
        Err(Net::InvalidArgument)
    );
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_bind(v6s, NET, lo(0)),
        Err(Net::InvalidArgument)
    );

    // each setter reads back, and zero is refused without changing the value
    macro_rules! option {
        ($get:ident, $set:ident, $value:expr) => {{
            let before = h.$get(v4s).unwrap();
            assert_eq!(h.$set(v4s, 0), Err(Net::InvalidArgument), stringify!($set));
            assert_eq!(
                h.$get(v4s),
                Ok(before),
                "{} left it alone",
                stringify!($set)
            );
            assert_eq!(h.$set(v4s, $value), Ok(()));
            assert_eq!(h.$get(v4s), Ok($value), stringify!($get));
        }};
    }
    option!(
        wasi_sockets_tcp_method_tcp_socket_keep_alive_idle_time,
        wasi_sockets_tcp_method_tcp_socket_set_keep_alive_idle_time,
        7_000_000_000u64
    );
    option!(
        wasi_sockets_tcp_method_tcp_socket_keep_alive_interval,
        wasi_sockets_tcp_method_tcp_socket_set_keep_alive_interval,
        3_000_000_000u64
    );
    option!(
        wasi_sockets_tcp_method_tcp_socket_keep_alive_count,
        wasi_sockets_tcp_method_tcp_socket_set_keep_alive_count,
        9u32
    );
    option!(
        wasi_sockets_tcp_method_tcp_socket_hop_limit,
        wasi_sockets_tcp_method_tcp_socket_set_hop_limit,
        200u8
    );
    option!(
        wasi_sockets_tcp_method_tcp_socket_receive_buffer_size,
        wasi_sockets_tcp_method_tcp_socket_set_receive_buffer_size,
        12_345u64
    );
    option!(
        wasi_sockets_tcp_method_tcp_socket_send_buffer_size,
        wasi_sockets_tcp_method_tcp_socket_set_send_buffer_size,
        54_321u64
    );
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_keep_alive_enabled(v4s),
        Ok(false)
    );
    h.wasi_sockets_tcp_method_tcp_socket_set_keep_alive_enabled(v4s, true)
        .unwrap();
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_keep_alive_enabled(v4s),
        Ok(true)
    );
    // a backlog of zero is invalid; any other is accepted while the socket is not connected
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_set_listen_backlog_size(v4s, 0),
        Err(Net::InvalidArgument)
    );
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_set_listen_backlog_size(v4s, u64::MAX),
        Ok(())
    );
}

#[test]
fn an_accepted_socket_inherits_the_listeners_options() {
    let mut h = host(NetworkPolicy::Loopback);
    let (server, port) = listener(&mut h);
    h.wasi_sockets_tcp_method_tcp_socket_set_hop_limit(server, 77)
        .unwrap();
    let c = tcp(&mut h);
    h.wasi_sockets_tcp_method_tcp_socket_start_connect(c, NET, lo(port))
        .unwrap();
    h.wasi_sockets_tcp_method_tcp_socket_finish_connect(c)
        .unwrap();
    let (accepted, ..) = accept_blocking(&mut h, server);
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_hop_limit(accepted),
        Ok(77)
    );
    // a connected socket can no longer change its backlog
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_set_listen_backlog_size(accepted, 5),
        Err(Net::InvalidState)
    );
}

// ---- data over a connection -------------------------------------------------------------

#[test]
fn bytes_cross_a_loopback_connection_in_both_directions_and_shutdown_ends_the_peer() {
    let Pair {
        host: mut h,
        port,
        client,
        c_in,
        c_out,
        accepted,
        s_in,
        s_out,
    } = pair();

    // the two ends name each other
    let c_local = h
        .wasi_sockets_tcp_method_tcp_socket_local_address(client)
        .unwrap();
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_remote_address(accepted),
        Ok(c_local)
    );
    assert_eq!(
        port_of(
            h.wasi_sockets_tcp_method_tcp_socket_remote_address(client)
                .unwrap()
        ),
        port
    );

    // client -> server
    h.wasi_io_streams_method_output_stream_blocking_write_and_flush(c_out, b"ping".to_vec())
        .unwrap();
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(s_in, 16),
        Ok(b"ping".to_vec())
    );
    // server -> client
    h.wasi_io_streams_method_output_stream_blocking_write_and_flush(s_out, b"pong!".to_vec())
        .unwrap();
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(c_in, 3),
        Ok(b"pon".to_vec())
    );
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_skip(c_in, 1),
        Ok(1)
    );
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(c_in, 8),
        Ok(b"!".to_vec())
    );
    // nothing more has arrived: a non-blocking read is empty, not closed
    assert_eq!(
        h.wasi_io_streams_method_input_stream_read(c_in, 8),
        Ok(vec![])
    );
    assert_eq!(h.wasi_io_streams_method_input_stream_skip(c_in, 8), Ok(0));

    // writes queued without a flush still arrive, in order, once the stream is flushed
    h.wasi_io_streams_method_output_stream_write(c_out, b"a".to_vec())
        .unwrap();
    h.wasi_io_streams_method_output_stream_write(c_out, b"b".to_vec())
        .unwrap();
    h.wasi_io_streams_method_output_stream_blocking_flush(c_out)
        .unwrap();
    let mut got = Vec::new();
    while got.len() < 2 {
        got.extend(
            h.wasi_io_streams_method_input_stream_blocking_read(s_in, 8)
                .unwrap(),
        );
    }
    assert_eq!(got, b"ab");

    // shutting the send side ends the peer's input after the bytes before it
    h.wasi_io_streams_method_output_stream_write(c_out, b"last".to_vec())
        .unwrap();
    h.wasi_sockets_tcp_method_tcp_socket_shutdown(client, ShutdownType::Send_)
        .unwrap();
    let mut tail = Vec::new();
    loop {
        match h.wasi_io_streams_method_input_stream_blocking_read(s_in, 8) {
            Ok(bytes) => tail.extend(bytes),
            Err(StreamError::Closed) => break,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!(tail, b"last");
    // the other direction still works
    h.wasi_io_streams_method_output_stream_blocking_write_and_flush(s_out, b"still".to_vec())
        .unwrap();
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(c_in, 8),
        Ok(b"still".to_vec())
    );
}

#[test]
fn splice_between_connection_streams_moves_nothing_while_empty_and_relays_when_data_arrives() {
    let Pair {
        host: mut h,
        c_in,
        c_out,
        s_in,
        s_out,
        ..
    } = pair();
    // nothing has arrived: `splice` moves nothing and does not wait
    assert_eq!(
        h.wasi_io_streams_method_output_stream_splice(c_out, s_in, 10),
        Ok(0)
    );
    h.wasi_io_streams_method_output_stream_blocking_write_and_flush(s_out, b"relay".to_vec())
        .unwrap();
    // wait for the bytes on the client's side, then relay them back to the server's stream
    let moved = h
        .wasi_io_streams_method_output_stream_blocking_splice(c_out, c_in, 10)
        .unwrap();
    assert_eq!(moved, 5);
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(s_in, 10),
        Ok(b"relay".to_vec())
    );
}

#[test]
fn dropping_an_output_stream_still_delivers_what_was_written() {
    let Pair {
        host: mut h,
        c_out,
        s_in,
        ..
    } = pair();
    h.wasi_io_streams_method_output_stream_write(c_out, b"queued".to_vec())
        .unwrap();
    h.wasi_io_streams_resource_drop_output_stream(c_out);
    assert_eq!(
        h.wasi_io_streams_method_input_stream_blocking_read(s_in, 16),
        Ok(b"queued".to_vec())
    );
}

#[test]
fn a_tcp_check_write_is_at_most_4096_and_a_longer_write_traps() {
    let Pair {
        host: mut h, c_out, ..
    } = pair();
    let permit = h
        .wasi_io_streams_method_output_stream_check_write(c_out)
        .unwrap();
    assert!((1..=4096).contains(&permit), "{permit}");
    assert!(
        traps(|| h.wasi_io_streams_method_output_stream_write(c_out, vec![0; 4097])),
        "4097 bytes is over any check-write"
    );
}

// ---- UDP --------------------------------------------------------------------------------

fn udp_bound(host: &mut WasiHost, family: IpAddressFamily) -> (UdpSocket, u16) {
    let s = host
        .wasi_sockets_udp_create_socket_create_udp_socket(family)
        .unwrap();
    let addr = match family {
        IpAddressFamily::Ipv4 => lo(0),
        IpAddressFamily::Ipv6 => lo6(0),
    };
    host.wasi_sockets_udp_method_udp_socket_start_bind(s, NET, addr)
        .unwrap();
    host.wasi_sockets_udp_method_udp_socket_finish_bind(s)
        .unwrap();
    let port = port_of(
        host.wasi_sockets_udp_method_udp_socket_local_address(s)
            .unwrap(),
    );
    (s, port)
}

fn datagram(data: &[u8], to: Option<IpSocketAddress>) -> OutgoingDatagram {
    OutgoingDatagram {
        data: data.to_vec(),
        remote_address: to,
    }
}

fn wait_receive(
    host: &mut WasiHost,
    stream: IncomingDatagramStream,
    max: u64,
) -> Vec<IncomingDatagram> {
    let p = host.wasi_sockets_udp_method_incoming_datagram_stream_subscribe(stream);
    host.wasi_io_poll_method_pollable_block(p);
    host.wasi_sockets_udp_method_incoming_datagram_stream_receive(stream, max)
        .unwrap()
}

#[test]
fn udp_state_machine_and_unsupported_options() {
    let mut h = host(NetworkPolicy::Loopback);
    let s = h
        .wasi_sockets_udp_create_socket_create_udp_socket(IpAddressFamily::Ipv4)
        .unwrap();
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_address_family(s),
        IpAddressFamily::Ipv4
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_finish_bind(s),
        Err(Net::NotInProgress)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_local_address(s),
        Err(Net::InvalidState)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_stream(s, None)
            .map(|_| ()),
        Err(Net::InvalidState),
        "not bound"
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_start_bind(s, NET, v4((192, 0, 2, 1), 0)),
        Err(Net::AccessDenied)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_start_bind(s, NET, lo6(0)),
        Err(Net::InvalidArgument)
    );
    h.wasi_sockets_udp_method_udp_socket_start_bind(s, NET, lo(0))
        .unwrap();
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_start_bind(s, NET, lo(0)),
        Err(Net::InvalidState)
    );
    h.wasi_sockets_udp_method_udp_socket_finish_bind(s).unwrap();
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_finish_bind(s),
        Err(Net::NotInProgress)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_remote_address(s),
        Err(Net::InvalidState),
        "no default peer yet"
    );
    // a peer that fails the address rules refuses the stream and keeps the socket as it was
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_stream(s, Some(lo(0)))
            .map(|_| ()),
        Err(Net::InvalidArgument)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_stream(s, Some(v4((192, 0, 2, 1), 9)))
            .map(|_| ()),
        Err(Net::AccessDenied)
    );
    // the buffer sizes and hop limit are not adjustable
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_unicast_hop_limit(s),
        Err(Net::NotSupported)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_set_unicast_hop_limit(s, 5),
        Err(Net::NotSupported)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_receive_buffer_size(s),
        Err(Net::NotSupported)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_set_receive_buffer_size(s, 5),
        Err(Net::NotSupported)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_send_buffer_size(s),
        Err(Net::NotSupported)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_set_send_buffer_size(s, 5),
        Err(Net::NotSupported)
    );
}

#[test]
fn an_unconnected_udp_stream_sends_to_each_address_and_receives_from_anyone() {
    let mut h = host(NetworkPolicy::Loopback);
    let (a, a_port) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (b, b_port) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (_, a_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(a, None)
        .unwrap();
    let (b_in, _) = h
        .wasi_sockets_udp_method_udp_socket_stream(b, None)
        .unwrap();

    assert_eq!(
        h.wasi_sockets_udp_method_outgoing_datagram_stream_check_send(a_out),
        Ok(1024)
    );
    let sent = h
        .wasi_sockets_udp_method_outgoing_datagram_stream_send(
            a_out,
            vec![
                datagram(b"one", Some(lo(b_port))),
                datagram(b"two", Some(lo(b_port))),
            ],
        )
        .unwrap();
    assert_eq!(sent, 2);
    // `max_results` bounds one call: of the two queued datagrams only the first is returned
    let first = wait_receive(&mut h, b_in, 1);
    assert_eq!(first.len(), 1);
    assert_eq!(
        h.wasi_sockets_udp_method_incoming_datagram_stream_receive(b_in, 0),
        Ok(vec![]),
        "0 returns nothing and takes nothing"
    );
    let second = wait_receive(&mut h, b_in, 10);
    // UDP on loopback keeps order for a single sender
    let got: Vec<_> = first.into_iter().chain(second).collect();
    assert_eq!(
        got.iter().map(|d| d.data.as_slice()).collect::<Vec<_>>(),
        [b"one".as_slice(), b"two".as_slice()]
    );
    assert!(got.iter().all(|d| d.remote_address == lo(a_port)));
    // an unconnected stream needs an address for every datagram
    assert_eq!(
        h.wasi_sockets_udp_method_outgoing_datagram_stream_send(a_out, vec![datagram(b"x", None)]),
        Err(Net::InvalidArgument)
    );
    // nothing queued: an empty list, not an error
    let (c, _) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (c_in, _) = h
        .wasi_sockets_udp_method_udp_socket_stream(c, None)
        .unwrap();
    assert_eq!(
        h.wasi_sockets_udp_method_incoming_datagram_stream_receive(c_in, 5),
        Ok(vec![])
    );
}

#[test]
fn a_connected_udp_stream_drops_other_senders_and_rejects_other_destinations() {
    let mut h = host(NetworkPolicy::Loopback);
    let (a, a_port) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (peer, peer_port) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (stranger, stranger_port) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (a_in, a_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(a, Some(lo(peer_port)))
        .unwrap();
    assert_eq!(
        h.wasi_sockets_udp_method_udp_socket_remote_address(a),
        Ok(lo(peer_port))
    );
    let (_, p_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(peer, None)
        .unwrap();
    let (_, s_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(stranger, None)
        .unwrap();

    // the stranger writes first, then the peer: only the peer's datagram is delivered
    h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
        s_out,
        vec![datagram(b"stranger", Some(lo(a_port)))],
    )
    .unwrap();
    h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
        p_out,
        vec![datagram(b"peer", Some(lo(a_port)))],
    )
    .unwrap();
    let got = wait_receive(&mut h, a_in, 10);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].data, b"peer");
    assert_eq!(got[0].remote_address, lo(peer_port));

    // the connected stream sends to its peer without an address, and refuses any other one
    assert_eq!(
        h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
            a_out,
            vec![datagram(b"x", Some(lo(stranger_port)))]
        ),
        Err(Net::InvalidArgument)
    );
    assert_eq!(
        h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
            a_out,
            vec![datagram(b"x", Some(lo(peer_port)))]
        ),
        Ok(1),
        "the peer's own address is allowed"
    );
    assert_eq!(
        h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
            a_out,
            vec![datagram(b"direct", None)]
        ),
        Ok(1)
    );
}

#[test]
fn udp_datagram_size_limits_follow_the_family() {
    let mut h = host(NetworkPolicy::Loopback);
    let (a, _) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (_, b_port) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (_, a_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(a, None)
        .unwrap();
    let send = |h: &mut WasiHost, len| {
        h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
            a_out,
            vec![datagram(&vec![0; len], Some(lo(b_port)))],
        )
    };
    assert_eq!(send(&mut h, 65_508), Err(Net::DatagramTooLarge));
    // (a send of exactly the IPv4 maximum may still be refused by the OS buffer on some
    // hosts; what must hold is that it is not rejected by the size rule)
    assert_ne!(send(&mut h, 65_507), Err(Net::DatagramTooLarge));
    // a later datagram that fails the rules ends the batch instead of failing it
    assert_eq!(
        h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
            a_out,
            vec![
                datagram(b"ok", Some(lo(b_port))),
                datagram(&vec![0; 65_508], Some(lo(b_port))),
                datagram(b"never", Some(lo(b_port)))
            ]
        ),
        Ok(1)
    );
}

#[test]
fn a_replaced_udp_stream_traps_and_its_subscription_wakes() {
    let mut h = host(NetworkPolicy::Loopback);
    let (a, _) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (old_in, old_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(a, None)
        .unwrap();
    let wait = h.wasi_sockets_udp_method_incoming_datagram_stream_subscribe(old_in);
    assert!(
        !h.wasi_io_poll_method_pollable_ready(wait),
        "nothing queued"
    );
    h.wasi_sockets_udp_method_udp_socket_stream(a, None)
        .unwrap();
    assert!(
        h.wasi_io_poll_method_pollable_ready(wait),
        "a stale stream is ready so that a waiting guest wakes up and traps"
    );
    assert!(traps(|| h
        .wasi_sockets_udp_method_incoming_datagram_stream_receive(
            old_in, 1
        )));
    assert!(traps(|| h
        .wasi_sockets_udp_method_outgoing_datagram_stream_check_send(
            old_out
        )));
    assert!(traps(|| h
        .wasi_sockets_udp_method_outgoing_datagram_stream_send(
            old_out,
            vec![]
        )));
}

#[test]
fn udp_over_ipv6_loopback_round_trips() {
    let mut h = host(NetworkPolicy::Loopback);
    let a = h
        .wasi_sockets_udp_create_socket_create_udp_socket(IpAddressFamily::Ipv6)
        .unwrap();
    if h.wasi_sockets_udp_method_udp_socket_start_bind(a, NET, lo6(0))
        .is_err()
        || h.wasi_sockets_udp_method_udp_socket_finish_bind(a).is_err()
    {
        return; // no IPv6 loopback on this host
    }
    let a_port = port_of(
        h.wasi_sockets_udp_method_udp_socket_local_address(a)
            .unwrap(),
    );
    let (b, _) = udp_bound(&mut h, IpAddressFamily::Ipv6);
    let (a_in, _) = h
        .wasi_sockets_udp_method_udp_socket_stream(a, None)
        .unwrap();
    let (_, b_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(b, None)
        .unwrap();
    h.wasi_sockets_udp_method_outgoing_datagram_stream_send(
        b_out,
        vec![datagram(b"six", Some(lo6(a_port)))],
    )
    .unwrap();
    let got = wait_receive(&mut h, a_in, 1);
    assert_eq!(got[0].data, b"six");
    assert!(matches!(got[0].remote_address, IpSocketAddress::Ipv6(_)));
}

// ---- name lookup ------------------------------------------------------------------------

fn resolve(h: &mut WasiHost, name: &str) -> Result<Vec<IpAddress>, Net> {
    let stream = h.wasi_sockets_ip_name_lookup_resolve_addresses(NET, name.into())?;
    let mut out = Vec::new();
    while let Some(a) = h
        .wasi_sockets_ip_name_lookup_method_resolve_address_stream_resolve_next_address(stream)
        .unwrap()
    {
        out.push(a);
    }
    // an exhausted stream stays exhausted, and its pollable is ready at once
    assert_eq!(
        h.wasi_sockets_ip_name_lookup_method_resolve_address_stream_resolve_next_address(stream),
        Ok(None)
    );
    let p = h.wasi_sockets_ip_name_lookup_method_resolve_address_stream_subscribe(stream);
    assert!(h.wasi_io_poll_method_pollable_ready(p));
    h.wasi_sockets_ip_name_lookup_resource_drop_resolve_address_stream(stream);
    Ok(out)
}

#[test]
fn name_lookup_resolves_literals_and_localhost_locally_and_rejects_bad_names() {
    let mut h = host(NetworkPolicy::None);
    assert_eq!(
        resolve(&mut h, "10.1.2.3"),
        Ok(vec![IpAddress::Ipv4((10, 1, 2, 3))])
    );
    assert_eq!(
        resolve(&mut h, "::1"),
        Ok(vec![IpAddress::Ipv6((0, 0, 0, 0, 0, 0, 0, 1))])
    );
    let expected = vec![
        IpAddress::Ipv4((127, 0, 0, 1)),
        IpAddress::Ipv6((0, 0, 0, 0, 0, 0, 0, 1)),
    ];
    assert_eq!(resolve(&mut h, "localhost"), Ok(expected.clone()));
    assert_eq!(resolve(&mut h, "LocalHost."), Ok(expected));
    // not a name at all: invalid-argument before any lookup
    for bad in [
        "",
        ".",
        "a..b",
        "-a.example",
        "a-.example",
        "has space",
        "under_ok.but/slash",
        "300.300.300.300",
        "1.2.3",
        &"a".repeat(64),
        &format!("{}.com", "a.".repeat(130)),
    ] {
        assert_eq!(resolve(&mut h, bad), Err(Net::InvalidArgument), "{bad:?}");
    }
    // a valid name outside localhost needs the external policy
    assert_eq!(
        resolve(&mut h, "some_host-1.example"),
        Err(Net::PermanentResolverFailure)
    );
    // a label of exactly 63 characters is a name (so it gets as far as the policy)
    assert_eq!(
        resolve(&mut h, &"a".repeat(63)),
        Err(Net::PermanentResolverFailure)
    );
}

#[test]
fn an_external_lookup_of_an_unresolvable_name_is_name_unresolvable() {
    let mut h = host(NetworkPolicy::External);
    // `.invalid` is reserved (RFC 6761) and never resolves
    assert_eq!(
        resolve(&mut h, "no-such-host.invalid"),
        Err(Net::NameUnresolvable)
    );
}

// ---- pollables --------------------------------------------------------------------------

#[test]
fn poll_returns_the_ready_indices_and_waits_for_the_earliest_deadline() {
    let mut h = host(NetworkPolicy::Loopback);
    let long = h.wasi_clocks_monotonic_clock_subscribe_duration(60_000_000_000);
    let soon = h.wasi_clocks_monotonic_clock_subscribe_duration(20_000_000);
    let start = Instant::now();
    assert_eq!(h.wasi_io_poll_poll(vec![long, soon]), vec![1]);
    let waited = start.elapsed();
    assert!(waited >= Duration::from_millis(15), "{waited:?}");
    assert!(waited < Duration::from_secs(10), "{waited:?}");
    // once ready, both of two ready pollables are listed in order
    let a = h.wasi_clocks_monotonic_clock_subscribe_duration(0);
    let b = h.wasi_clocks_monotonic_clock_subscribe_duration(0);
    assert_eq!(h.wasi_io_poll_poll(vec![b, a]), vec![0, 1]);
    // a block on a ready pollable returns at once
    let c = h.wasi_clocks_monotonic_clock_subscribe_duration(0);
    h.wasi_io_poll_method_pollable_block(c);
}

#[test]
fn polling_an_empty_list_traps() {
    let mut h = host(NetworkPolicy::Loopback);
    assert!(traps(|| h.wasi_io_poll_poll(vec![])));
}

#[test]
fn a_huge_duration_does_not_overflow_the_deadline() {
    let mut h = host(NetworkPolicy::Loopback);
    let p = h.wasi_clocks_monotonic_clock_subscribe_duration(u64::MAX);
    assert!(
        !h.wasi_io_poll_method_pollable_ready(p),
        "a far deadline is not reached"
    );
    // an instant far ahead of the clock is just as far, one in the past is ready at once
    let far = h.wasi_clocks_monotonic_clock_subscribe_instant(u64::MAX);
    assert!(!h.wasi_io_poll_method_pollable_ready(far));
    let past = h.wasi_clocks_monotonic_clock_subscribe_instant(0);
    assert!(h.wasi_io_poll_method_pollable_ready(past));
}

#[test]
fn accept_and_stream_pollables_turn_ready_when_data_arrives() {
    let mut h = host(NetworkPolicy::Loopback);
    let (server, port) = listener(&mut h);
    let accept_wait = h.wasi_sockets_tcp_method_tcp_socket_subscribe(server);
    assert!(!h.wasi_io_poll_method_pollable_ready(accept_wait));
    let client = tcp(&mut h);
    h.wasi_sockets_tcp_method_tcp_socket_start_connect(client, NET, lo(port))
        .unwrap();
    let (_, c_out) = h
        .wasi_sockets_tcp_method_tcp_socket_finish_connect(client)
        .unwrap();
    h.wasi_io_poll_method_pollable_block(accept_wait);
    // the connection that made the poll ready is the one `accept` returns
    let (_, s_in, s_out) = h.wasi_sockets_tcp_method_tcp_socket_accept(server).unwrap();
    // reading: not ready until bytes come
    let readable = h.wasi_io_streams_method_input_stream_subscribe(s_in);
    assert!(!h.wasi_io_poll_method_pollable_ready(readable));
    h.wasi_io_streams_method_output_stream_blocking_write_and_flush(c_out, b"x".to_vec())
        .unwrap();
    assert_eq!(h.wasi_io_poll_poll(vec![readable]), vec![0]);
    // writing: ready while the send queue has room
    let writable = h.wasi_io_streams_method_output_stream_subscribe(s_out);
    assert!(h.wasi_io_poll_method_pollable_ready(writable));
}

// ---- back-pressure and failure of a connection's output ---------------------------------

#[test]
fn a_peer_that_does_not_read_fills_the_send_queue_and_reading_frees_it() {
    let Pair {
        mut host,
        c_out,
        s_in,
        ..
    } = pair();
    let writable = host.wasi_io_streams_method_output_stream_subscribe(c_out);
    // write until the permit is 0: the socket buffers and the 64 KiB queue are full
    let mut written = 0usize;
    loop {
        let permit = host
            .wasi_io_streams_method_output_stream_check_write(c_out)
            .unwrap();
        assert!(permit <= 4096);
        if permit == 0 {
            break;
        }
        host.wasi_io_streams_method_output_stream_write(c_out, vec![0xAB; permit as usize])
            .unwrap();
        written += permit as usize;
        assert!(written < 64 << 20, "the queue never filled");
    }
    assert!(written >= 64 * 1024, "{written}");
    // the peer reads everything that was accepted; nothing is lost or reordered
    let mut received = 0usize;
    while received < written {
        let bytes = host
            .wasi_io_streams_method_input_stream_blocking_read(s_in, 4096)
            .unwrap();
        assert!(bytes.iter().all(|&b| b == 0xAB));
        received += bytes.len();
        // keep the sender's queue moving into the socket
        let _ = host.wasi_io_streams_method_output_stream_flush(c_out);
    }
    assert_eq!(received, written);
    host.wasi_io_poll_method_pollable_block(writable);
    assert!(
        host.wasi_io_streams_method_output_stream_check_write(c_out)
            .unwrap()
            > 0
    );
}

#[test]
fn a_dead_peer_fails_the_output_once_and_then_it_is_closed() {
    let Pair {
        mut host,
        c_out,
        accepted,
        s_in,
        s_out,
        ..
    } = pair();
    // the peer goes away: drop its streams and socket
    host.wasi_io_streams_resource_drop_input_stream(s_in);
    host.wasi_io_streams_resource_drop_output_stream(s_out);
    host.wasi_sockets_tcp_resource_drop_tcp_socket(accepted);
    // writes keep being accepted until the OS reports the reset; then exactly one operation
    // gets `last-operation-failed`, and every later one gets `closed`
    // (whichever call finds out first, `write` or `check-write`, gets the one report)
    let mut first_failure = None;
    for _ in 0..2000 {
        let outcome = host
            .wasi_io_streams_method_output_stream_check_write(c_out)
            .and_then(|permit| {
                if permit == 0 {
                    Ok(())
                } else {
                    host.wasi_io_streams_method_output_stream_write(c_out, vec![0; permit as usize])
                }
            });
        if let Err(e) = outcome {
            first_failure = Some(e);
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let Some(StreamError::LastOperationFailed(err)) = first_failure else {
        panic!("a dead peer never failed the stream: {first_failure:?}");
    };
    assert!(
        !host
            .wasi_io_error_method_error_to_debug_string(err)
            .is_empty()
    );
    assert_eq!(
        host.wasi_filesystem_types_filesystem_error_code(err),
        None,
        "a connection's failure is not a filesystem error"
    );
    assert_eq!(
        host.wasi_io_streams_method_output_stream_check_write(c_out),
        Err(StreamError::Closed)
    );
    assert_eq!(
        host.wasi_io_streams_method_output_stream_write(c_out, vec![1]),
        Err(StreamError::Closed)
    );
    assert_eq!(
        host.wasi_io_streams_method_output_stream_blocking_write_and_flush(c_out, vec![1]),
        Err(StreamError::Closed)
    );
}

#[test]
fn a_zero_length_read_succeeds_only_while_the_connection_is_open() {
    let Pair {
        mut host,
        c_in,
        accepted,
        ..
    } = pair();
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(c_in, 0),
        Ok(vec![])
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_skip(c_in, 0),
        Ok(0)
    );
    // the peer closes its side: the zero-length read now sees the end
    host.wasi_sockets_tcp_method_tcp_socket_shutdown(accepted, ShutdownType::Both)
        .unwrap();
    let p = host.wasi_io_streams_method_input_stream_subscribe(c_in);
    host.wasi_io_poll_method_pollable_block(p);
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(c_in, 0),
        Err(StreamError::Closed)
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_read(c_in, 8),
        Err(StreamError::Closed)
    );
    assert_eq!(
        host.wasi_io_streams_method_input_stream_blocking_read(c_in, 0),
        Err(StreamError::Closed)
    );
}

#[test]
fn shutdown_receive_is_accepted_on_a_connected_socket() {
    let Pair {
        mut host, client, ..
    } = pair();
    assert_eq!(
        host.wasi_sockets_tcp_method_tcp_socket_shutdown(client, ShutdownType::Receive),
        Ok(())
    );
    // an unconnected socket is `invalid-state`
    let idle = tcp(&mut host);
    assert_eq!(
        host.wasi_sockets_tcp_method_tcp_socket_shutdown(idle, ShutdownType::Receive),
        Err(Net::InvalidState)
    );
}

#[test]
fn the_instance_network_is_one_handle_that_can_be_dropped() {
    let mut h = host(NetworkPolicy::Loopback);
    let n = h.wasi_sockets_instance_network_instance_network();
    // the address policy is the host's: the handle itself is not looked at
    let s = tcp(&mut h);
    assert_eq!(
        h.wasi_sockets_tcp_method_tcp_socket_start_bind(s, n, lo(0)),
        Ok(())
    );
    h.wasi_sockets_network_resource_drop_network(n);
    // dropping a socket frees its slot for the next one
    let t = tcp(&mut h);
    h.wasi_sockets_tcp_resource_drop_tcp_socket(t);
    assert_eq!(tcp(&mut h).0, t.0);
    let (u, _) = udp_bound(&mut h, IpAddressFamily::Ipv4);
    let (u_in, u_out) = h
        .wasi_sockets_udp_method_udp_socket_stream(u, None)
        .unwrap();
    let out_ready = h.wasi_sockets_udp_method_outgoing_datagram_stream_subscribe(u_out);
    assert!(h.wasi_io_poll_method_pollable_ready(out_ready));
    h.wasi_sockets_udp_resource_drop_incoming_datagram_stream(u_in);
    h.wasi_sockets_udp_resource_drop_outgoing_datagram_stream(u_out);
    h.wasi_sockets_udp_resource_drop_udp_socket(u);
}

/// Writes to `out` until `check-write` permits nothing (the peer must not be reading); returns
/// the bytes written.
fn fill_send_queue(host: &mut WasiHost, out: OutputStream) -> usize {
    let mut written = 0;
    loop {
        let permit = host
            .wasi_io_streams_method_output_stream_check_write(out)
            .unwrap();
        if permit == 0 {
            return written;
        }
        host.wasi_io_streams_method_output_stream_write(out, vec![9; permit as usize])
            .unwrap();
        written += permit as usize;
    }
}

#[test]
fn splice_into_a_full_send_queue_moves_nothing_and_leaves_the_source_alone() {
    let Pair {
        mut host,
        c_in,
        c_out,
        accepted,
        ..
    } = pair();
    // the peer sends nothing more: `c_in` is at its end
    host.wasi_sockets_tcp_method_tcp_socket_shutdown(accepted, ShutdownType::Send_)
        .unwrap();
    // the peer does not read
    fill_send_queue(&mut host, c_out);
    // nothing can be written, so nothing is read from `src`, not even its end
    assert_eq!(
        host.wasi_io_streams_method_output_stream_splice(c_out, c_in, 10),
        Ok(0)
    );
}

// ---- what the send queue still holds when the connection ends --------------------------

/// A host connected to a plain `std` peer that has not read anything yet, with the send queue
/// filled until `check-write` permits nothing; returns the bytes written.
fn filled_connection() -> (WasiHost, TcpSocket, std::net::TcpStream, usize) {
    let listener = {
        let _window = port_window();
        std::net::TcpListener::bind("127.0.0.1:0").unwrap()
    };
    let port = listener.local_addr().unwrap().port();
    let mut host = host(NetworkPolicy::Loopback);
    let client = tcp(&mut host);
    host.wasi_sockets_tcp_method_tcp_socket_start_connect(client, NET, lo(port))
        .unwrap();
    let (_c_in, c_out) = host
        .wasi_sockets_tcp_method_tcp_socket_finish_connect(client)
        .unwrap();
    let (peer, _) = listener.accept().unwrap();
    let written = fill_send_queue(&mut host, c_out);
    (host, client, peer, written)
}

fn read_to_end_in_a_thread(mut peer: std::net::TcpStream) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        let mut total = 0;
        let mut buf = [0u8; 65536];
        loop {
            match std::io::Read::read(&mut peer, &mut buf).unwrap() {
                0 => return total,
                n => total += n,
            }
        }
    })
}

#[test]
fn dropping_the_host_sends_what_the_send_queue_still_holds() {
    let (host, _client, peer, written) = filled_connection();
    let reader = read_to_end_in_a_thread(peer);
    drop(host);
    assert_eq!(reader.join().unwrap(), written);
}

#[test]
fn shutdown_of_the_send_side_sends_the_queue_first() {
    let (mut host, client, peer, written) = filled_connection();
    let reader = read_to_end_in_a_thread(peer);
    host.wasi_sockets_tcp_method_tcp_socket_shutdown(client, ShutdownType::Send_)
        .unwrap();
    // the peer sees the end of the stream only after every byte that was accepted
    assert_eq!(reader.join().unwrap(), written);
}
