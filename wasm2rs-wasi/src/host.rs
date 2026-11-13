//! A WASI preview2 host written against the typed [`Host`](crate::Host) trait. It has no
//! canonical-ABI code: strings, lists, results and resources arrive as Rust values, and the
//! `Glue` that `wasm2rs --wit-host --glue --external-types=wasm2rs_wasi` generates per component lowers
//! them to the module's flat `Imports`. It starts with
//! the interfaces a `println!` program uses: stdout, the environment, the arguments
//! (`with_arguments`), exit, output streams and pollables, stderr and the terminal
//! queries (not a terminal), which a std program asks for at startup. Handles: 0 = stdin,
//! 1 = stdout, 2 = stderr; a pollable is an index into `pollables`: ready at once, at
//! the deadline a clock subscription gave it, or when stdin has data or is at its end.
//! The clocks: monotonic `now` (nanoseconds since the host was created), `subscribe-duration`,
//! `subscribe-instant` (an instant on that same clock) and wall-clock `now`. Stdin is the real
//! one: a thread reads it into a buffer, so `read` never blocks (an empty
//! list means "nothing yet", `closed` the end of input) and `poll` and `block` wait on the
//! buffer's condition variable with a timeout at the earliest deadline. It also serves `wasi:random` and
//! `wasi:filesystem`: read-only with `with_preopen`, writable with `with_writable_preopen`.
//! Reading: `preopens.get-directories`, `descriptor.open-at`, `read-via-stream`, `stat`,
//! `stat-at`, `get-flags`, `read-directory`, `readlink-at`, `metadata-hash` (and `-at`),
//! `directory-entry-stream.read-directory-entry` and file `input-stream` `read`, `blocking-read`,
//! `skip` and `blocking-skip`. Writing (`read-only` on a read-only
//! preopen): `open-at` with create/exclusive/truncate and `write`, `write-via-stream`,
//! `append-via-stream`, file `output-stream` `write`/`check-write`/`flush`/`blocking-flush`/
//! `blocking-write-and-flush`/`write-zeroes`/`blocking-write-zeroes-and-flush`,
//! `create-directory-at`, `remove-directory-at`, `unlink-file-at`, `rename-at`, `symlink-at`,
//! `link-at`, `set-size`, `sync`, `sync-data`, `set-times` and `set-times-at`. The semantics:
//! preopened directories under their guest names, descriptors and streams in
//! slabs (the preopens are the first descriptors and never freed; a file's input or output
//! stream is `FIRST_FILE_STREAM + slot` in its own slab, clear of the stdio handles), paths
//! resolved under the descriptor's preopen (absolute paths, `..` and symlinks that lead out are `not-permitted`).
//! It also serves TCP over loopback (`wasi:sockets` network, instance-network, tcp,
//! tcp-create-socket) with these semantics: an address outside
//! 127.0.0.0/8 and ::1 is `access-denied` unless `with_external_network` was called, `finish-bind`
//! binds and listens, `start-connect` connects at once, `read` never blocks (`blocking-read` and `blocking-skip` wait), `write` never waits (a connection's output stream has a 64 KiB send queue: `check-write` is 0 and its pollable not ready while a peer that does not read keeps it full; a write over `check-write` traps) and a socket's
//! pollable is polled every millisecond; a connection's streams are `TCP_STREAM + slot`.
//! UDP (`udp`, `udp-create-socket`) works the same way over loopback: `finish-bind` binds a
//! non-blocking `std` socket (an address outside loopback is `access-denied`), `stream` (callable on a
//! bound socket; a later call replaces the default remote, and the streams of earlier calls then trap) gives the datagram streams as `udp_in` and `udp_out` slots, and
//! `receive` and `send` never block (an empty list, or fewer datagrams sent, means "not now").
//! A stream made with `Some(remote)` drops datagrams from other senders and sends to `remote` (a
//! datagram's own address must then be absent or the same); an unconnected one needs an address in
//! each datagram (`invalid-argument` otherwise). A datagram stream's pollable is polled every
//! millisecond, an outgoing stream and a socket are always ready, and sending more than `check-send`
//! permits (1024) traps. Name lookup (`ip-name-lookup`): `localhost` (both loopback addresses) and
//! numeric IPv4 and IPv6 addresses resolve locally; any other name needs `with_external_network`
//! (then `std`'s resolver runs at `resolve-addresses`) and is `permanent-resolver-failure` without it;
//! a name that is not a host name is `invalid-argument`. The TCP socket options (keep-alive
//! enabled, idle time, interval and count, hop limit, receive and send buffer sizes) are only
//! recorded, not applied to the OS socket: a socket starts with the defaults, a set value is
//! read back, zero is `invalid-argument`, and an accepted socket inherits the listener's values.
//! The listen backlog size is only validated (zero is `invalid-argument`, `invalid-state` once a
//! connection is started or made, ignored while listening): it is not stored or read back. The UDP ones (hop limit, buffer sizes) answer
//! `not-supported`.
//!
//! The trait and its types are the library's own (`crate::Host`, `crate::wasi_filesystem_types`,
//! ...); a method this host does not implement keeps the trait's trapping default (today
//! there is none: `WasiHost` overrides every method of the 0.2.0 surface).
//!
//! Incoming HTTP (`incoming-handler#handle` exported by the guest) is driven by the embedder:
//! `new_incoming_request` and `new_response_outparam` mint the handles passed to `handle`, and
//! `take_response` returns what the guest set once `handle` has returned (status and headers as plain
//! data, the body as a `ServedBody` read while the guest writes it, with the trailers it finished
//! with).

use crate::wasi_cli_terminal_input::*;
use crate::wasi_cli_terminal_output::*;
use crate::wasi_clocks_wall_clock::*;
use crate::wasi_filesystem_types::ErrorCode as FsErrorCode;
use crate::wasi_filesystem_types::*;
use crate::wasi_http_types::{
    ErrorCode as HttpErrorCode, Fields, FutureIncomingResponse, IncomingBody, IncomingRequest,
    IncomingResponse, OutgoingBody, OutgoingRequest, OutgoingResponse, RequestOptions,
    ResponseOutparam,
};
use crate::wasi_io_error::*;
use crate::wasi_io_poll::*;
use crate::wasi_io_streams::*;
use crate::wasi_sockets_ip_name_lookup::*;
use crate::wasi_sockets_network::ErrorCode as NetErrorCode;
use crate::wasi_sockets_network::*;
use crate::wasi_sockets_tcp::*;
use crate::wasi_sockets_udp::*;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::Dir;
#[cfg(unix)]
use cap_std::fs::MetadataExt as _;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, SocketAddrV6, TcpListener,
    TcpStream, ToSocketAddrs, UdpSocket as StdUdpSocket,
};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

mod config;
mod http;
mod sources;

mod fs;
mod net;
mod slab;
mod stream;

pub use config::{Access, ConfigError, NetworkPolicy, WasiHostBuilder};
use fs::*;
pub use http::{IncomingRequestParts, ServedBody, ServedResponse};
use net::*;
use slab::*;
#[cfg(feature = "os-random")]
pub use sources::OsRandom;
pub use sources::{
    Clock, HttpClient, HttpClientError, HttpRequest, HttpResponse, InsecureRandom, RandomError,
    RandomSource, SystemClock,
};
use stream::*;

pub struct WasiHost {
    /// The program's arguments; set by `with_arguments`.
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// The time `wasi:clocks` reports; waits are in real time whatever it says.
    clock: Box<dyn Clock>,
    /// Filled by the stdin thread, which `start_stdin` spawns on first use.
    stdin: SharedStdin,
    /// What the stdin thread reads; `None` is the process's stdin. Taken when it starts.
    stdin_reader: Option<Box<dyn std::io::Read + Send>>,
    /// Where stdout goes; `None` is the process's stdout.
    stdout: Option<Box<dyn Write + Send>>,
    /// Where stderr goes; `None` is the process's stderr.
    stderr: Option<Box<dyn Write + Send>>,
    /// The messages behind the live `error` handles (`last-operation-failed`); a dropped
    /// handle frees its slot.
    errors: Slab<ErrorEntry>,
    /// The input streams (handles) whose failure was reported: they answer `closed` from then
    /// on. A dropped stream is forgotten.
    failed_in: std::collections::HashSet<u32>,
    /// Like `failed_in`, for the output streams that are not connections (a connection keeps
    /// it in its `TcpOut`).
    failed_out: std::collections::HashSet<u32>,
    /// Whether the stdin thread was started.
    stdin_started: bool,
    /// The source behind `wasi:random`; `None` only for a host made by `new` without the
    /// `os-random` feature, whose random calls then trap.
    random: Option<Box<dyn RandomSource>>,
    /// One entry per live subscription: what it waits for. A dropped pollable frees its slot.
    pollables: Slab<Wait>,
    /// The preopened directories; descriptor slot `i` is `preopens[i]`.
    preopens: Vec<Preopen>,
    /// The initial working directory if set; else "/" when a preopen is named "/".
    cwd: Option<Option<String>>,
    /// Open descriptors; slots `0..preopens.len()` are the preopens.
    descriptors: Slab<OpenDescriptor>,
    /// Streams behind the input-stream handles `FIRST_FILE_STREAM..`.
    files: Slab<FileStream>,
    /// Streams behind the output-stream handles `FIRST_FILE_STREAM + slot` (1 and 2 are
    /// stdout and stderr).
    outputs: Slab<FileStream>,
    /// Directory listings behind `directory-entry-stream` handles: the entries (type, name)
    /// still to be read.
    listings: Slab<std::vec::IntoIter<(DescriptorType, String)>>,
    // sockets
    /// Which addresses the guest may use.
    network: NetworkPolicy,
    /// TCP sockets behind `tcp-socket` handles.
    sockets: Slab<NetSocket>,
    /// Input streams of connections, behind the input-stream handles `TCP_STREAM..`.
    tcp_in: Slab<TcpStream>,
    /// Output streams of connections, behind the output-stream handles `TCP_STREAM..`, each
    /// with its send queue.
    tcp_out: Slab<TcpOut>,
    /// The `id` of the next `TcpOut`.
    next_tcp_out_id: u64,
    /// UDP sockets behind `udp-socket` handles.
    udp_sockets: Slab<UdpSock>,
    /// Incoming datagram streams, behind `incoming-datagram-stream` handles.
    udp_in: Slab<UdpEnd>,
    /// Outgoing datagram streams, behind `outgoing-datagram-stream` handles.
    udp_out: Slab<UdpEnd>,
    /// The addresses still to be read from `resolve-address-stream` handles.
    resolutions: Slab<VecDeque<IpAddr>>,
    /// The client for `outgoing-handler.handle`, as the builder set it. The first request moves
    /// it into `http_shared`, which the request threads lock.
    http_client: Option<Box<dyn HttpClient>>,
    http_shared: Option<Arc<Mutex<Box<dyn HttpClient>>>>,
    /// The `wasi:http` `fields` resources.
    http_fields: Slab<http::HttpFields>,
    http_options: Slab<http::HttpOptions>,
    http_requests: Slab<http::HttpOutgoingRequest>,
    http_futures: Slab<http::HttpFuture>,
    http_responses: Slab<http::HttpIncomingResponse>,
    /// The bodies behind `incoming-body` handles; the input stream of slot `i` is
    /// `HTTP_STREAM + i`.
    http_bodies: Slab<http::HttpIncomingBody>,
    /// The bodies behind `outgoing-body` handles; the output stream of slot `i` is
    /// `HTTP_OUT_STREAM + i`.
    http_out_bodies: Slab<http::HttpOutgoingBody>,
    /// The `incoming-request` resources the embedder minted for `incoming-handler#handle`.
    http_in_requests: Slab<http::HttpIncomingRequest>,
    /// The `outgoing-response` resources a guest built.
    http_out_responses: Slab<http::HttpOutgoingResponse>,
    /// The `response-outparam` resources the embedder minted: empty until the guest sets one.
    /// The entry is the embedder's, freed by `take_response` (the guest's drop leaves it).
    http_outparams: HashMap<u32, Option<Result<http::HttpOutgoingResponse, HttpErrorCode>>>,
    /// The id of the next `response-outparam`. Ids are never reused while the host lives (a
    /// wrapped counter skips the live ones), so an outparam a guest kept after `take_response`
    /// cannot write into another call's answer.
    next_outparam: u32,
    /// The most a request or response body may hold.
    http_max_body: usize,
    /// The `future-trailers` handed out by `incoming-body.finish`: the trailers of a request the
    /// embedder minted with some, or those the client read after a response body (`none` for
    /// no trailers). `None` once `get` answered.
    http_trailers: Slab<Option<Option<http::Entries>>>,
}

impl Drop for WasiHost {
    fn drop(&mut self) {
        self.flush_stdio();
        self.flush_all_connections();
    }
}

/// The slot of a `wasi:http` resource handle that the guest must still own: using a dropped
/// one traps, like any other bad handle.
fn http_missing(kind: &str) -> ! {
    panic!("wasi:http: unknown {kind} handle")
}

impl WasiHost {
    fn http_fields(&self, h: Fields) -> &http::HttpFields {
        self.http_fields
            .get(h.0 as usize)
            .unwrap_or_else(|| http_missing("fields"))
    }

    fn http_fields_mut(&mut self, h: Fields) -> &mut http::HttpFields {
        self.http_fields
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("fields"))
    }

    fn http_new_fields(&mut self, fields: http::HttpFields) -> Fields {
        Fields(self.http_fields.insert(fields) as u32)
    }

    /// Takes over the `fields` handle's contents, for a constructor the guest gave it to.
    fn http_take_fields(&mut self, h: Fields) -> http::HttpFields {
        self.http_fields
            .take(h.0 as usize)
            .unwrap_or_else(|| http_missing("fields"))
    }

    /// A new `outgoing-body` and the handle its owner keeps; `too_large` is the `error-code`
    /// of a body over `http_max_body`.
    fn http_new_out_body(
        &mut self,
        too_large: fn(Option<u64>) -> HttpErrorCode,
    ) -> (http::OutBodyHandle, OutgoingBody) {
        let body = http::HttpOutgoingBody::new(self.http_max_body, too_large);
        let handle = body.handle();
        let slot = self.http_out_bodies.insert(body);
        (handle, OutgoingBody(slot as u32))
    }

    fn http_option(&self, h: RequestOptions) -> &http::HttpOptions {
        self.http_options
            .get(h.0 as usize)
            .unwrap_or_else(|| http_missing("request-options"))
    }

    fn http_option_mut(&mut self, h: RequestOptions) -> &mut http::HttpOptions {
        self.http_options
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("request-options"))
    }

    fn http_request(&self, h: OutgoingRequest) -> &http::HttpOutgoingRequest {
        self.http_requests
            .get(h.0 as usize)
            .unwrap_or_else(|| http_missing("outgoing-request"))
    }

    fn http_request_mut(&mut self, h: OutgoingRequest) -> &mut http::HttpOutgoingRequest {
        self.http_requests
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("outgoing-request"))
    }

    fn http_future_mut(&mut self, h: FutureIncomingResponse) -> &mut http::HttpFuture {
        self.http_futures
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("future-incoming-response"))
    }

    fn http_in_request(&self, h: IncomingRequest) -> &http::HttpIncomingRequest {
        self.http_in_requests
            .get(h.0 as usize)
            .unwrap_or_else(|| http_missing("incoming-request"))
    }

    fn http_out_response(&self, h: OutgoingResponse) -> &http::HttpOutgoingResponse {
        self.http_out_responses
            .get(h.0 as usize)
            .unwrap_or_else(|| http_missing("outgoing-response"))
    }

    fn http_out_response_mut(&mut self, h: OutgoingResponse) -> &mut http::HttpOutgoingResponse {
        self.http_out_responses
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("outgoing-response"))
    }

    fn http_response(&self, h: IncomingResponse) -> &http::HttpIncomingResponse {
        self.http_responses
            .get(h.0 as usize)
            .unwrap_or_else(|| http_missing("incoming-response"))
    }

    fn http_response_mut(&mut self, h: IncomingResponse) -> &mut http::HttpIncomingResponse {
        self.http_responses
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("incoming-response"))
    }

    fn http_out_body_mut(&mut self, h: OutgoingBody) -> &mut http::HttpOutgoingBody {
        self.http_out_bodies
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("outgoing-body"))
    }

    fn http_body_mut(&mut self, h: IncomingBody) -> &mut http::HttpIncomingBody {
        self.http_bodies
            .get_mut(h.0 as usize)
            .unwrap_or_else(|| http_missing("incoming-body"))
    }

    /// Whether `get` of the future in `http_futures` that `r` refers to has an answer (a dropped one
    /// has: nothing is left to wait for).
    fn http_future_ready(&self, r: SlotRef) -> bool {
        self.http_futures
            .get_ref(r)
            .is_none_or(http::HttpFuture::is_ready)
    }

    /// Whether the body in `http_bodies` that `r` refers to has bytes or its end to read.
    fn http_body_readable(&self, r: SlotRef) -> bool {
        self.http_bodies
            .get_ref(r)
            .is_none_or(|body| body.shared().readable())
    }

    /// `outgoing-handler.handle`: checks the request, then runs the client on a thread. The
    /// request and the options are the guest's to give away, so they are consumed even when
    /// the call fails.
    fn http_handle(
        &mut self,
        handle: OutgoingRequest,
        options: Option<RequestOptions>,
    ) -> Result<FutureIncomingResponse, HttpErrorCode> {
        let mut request = self
            .http_requests
            .take(handle.0 as usize)
            .unwrap_or_else(|| http_missing("outgoing-request"));
        let options = options.map(|o| {
            self.http_options
                .take(o.0 as usize)
                .unwrap_or_else(|| http_missing("request-options"))
        });
        let authority = request
            .authority
            .as_deref()
            .ok_or(HttpErrorCode::HTTPRequestURIInvalid)?;
        if !http::destination_allowed(self.network, authority) {
            return Err(HttpErrorCode::HTTPRequestDenied);
        }
        if let Some(client) = self.http_client.take() {
            self.http_shared = Some(Arc::new(Mutex::new(client)));
        }
        let client = self.http_shared.clone().ok_or_else(|| {
            HttpErrorCode::InternalError(Some("no http client is configured".into()))
        })?;
        let scheme = request
            .scheme
            .as_ref()
            .map_or_else(|| "https".to_string(), http::scheme_name);
        let path = request.path_with_query.as_deref().unwrap_or("/");
        let separator = if path.starts_with('/') { "" } else { "/" };
        let options = options.unwrap_or_default();
        let body = request.body.take();
        let request = HttpRequest {
            method: http::method_name(&request.method),
            url: format!("{scheme}://{authority}{separator}{path}"),
            headers: request.headers.entries(),
            body: Vec::new(),
            trailers: Vec::new(),
            connect_timeout: options.connect,
            first_byte_timeout: options.first_byte,
            between_bytes_timeout: options.between_bytes,
        };
        let future = http::HttpFuture::spawn(client, request, body);
        Ok(FutureIncomingResponse(
            self.http_futures.insert(future) as u32
        ))
    }

    /// Mints the `incoming-request` for a call of `incoming-handler#handle` (see the `Exports`
    /// trait of the generated glue). The guest owns the handle from then on; one that is not
    /// consumed or dropped stays in the table.
    pub fn new_incoming_request(&mut self, parts: IncomingRequestParts) -> IncomingRequest {
        IncomingRequest(
            self.http_in_requests
                .insert(http::HttpIncomingRequest::new(parts)) as u32,
        )
    }

    /// Mints the `response-outparam` for a call of `incoming-handler#handle`; pass the same
    /// handle to `take_response` afterwards.
    pub fn new_response_outparam(&mut self) -> ResponseOutparam {
        while self.http_outparams.contains_key(&self.next_outparam) {
            self.next_outparam = self.next_outparam.wrapping_add(1);
        }
        let id = self.next_outparam;
        self.next_outparam = id.wrapping_add(1);
        self.http_outparams.insert(id, None);
        ResponseOutparam(id)
    }

    /// What the guest answered through `outparam`, once `handle` returned: `None` if it did
    /// not set one (or the handle is unknown), `Some(Err(code))` for an error it set or a
    /// response whose body failed already. The `outparam` is spent. A body the guest has not
    /// finished is not an error: `ServedResponse::body` is read as the guest writes it.
    pub fn take_response(
        &mut self,
        outparam: ResponseOutparam,
    ) -> Option<Result<ServedResponse, HttpErrorCode>> {
        let answer = self.http_outparams.remove(&outparam.0)??;
        Some(answer.and_then(http::HttpOutgoingResponse::serve))
    }

    pub fn new(env: Vec<(String, String)>) -> Self {
        WasiHost {
            args: Vec::new(),
            env,
            clock: Box::new(SystemClock::new()),
            stdin: SharedStdin::default(),
            stdin_reader: None,
            stdout: None,
            stderr: None,
            errors: Slab::new(),
            failed_in: std::collections::HashSet::new(),
            failed_out: std::collections::HashSet::new(),
            stdin_started: false,
            #[cfg(feature = "os-random")]
            random: Some(Box::new(OsRandom::new())),
            #[cfg(not(feature = "os-random"))]
            random: None,
            pollables: Slab::new(),
            preopens: Vec::new(),
            cwd: None,
            descriptors: Slab::new(),
            files: Slab::new(),
            outputs: Slab::new(),
            listings: Slab::new(),
            network: NetworkPolicy::default(),
            sockets: Slab::new(),
            tcp_in: Slab::new(),
            tcp_out: Slab::new(),
            next_tcp_out_id: 0,
            udp_sockets: Slab::new(),
            udp_in: Slab::new(),
            udp_out: Slab::new(),
            resolutions: Slab::new(),
            http_client: None,
            http_shared: None,
            http_fields: Slab::new(),
            http_options: Slab::new(),
            http_requests: Slab::new(),
            http_futures: Slab::new(),
            http_responses: Slab::new(),
            http_bodies: Slab::new(),
            http_out_bodies: Slab::new(),
            http_in_requests: Slab::new(),
            http_out_responses: Slab::new(),
            http_outparams: HashMap::new(),
            next_outparam: 0,
            http_max_body: DEFAULT_MAX_REQUEST_BODY,
            http_trailers: Slab::new(),
        }
    }

    /// Gives the guest these command-line arguments (none by default).
    pub fn with_arguments(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Gives the guest the directory `root` (as its "/"), read-only; see
    /// `with_writable_preopen`. Panics if the directory cannot be used; the builder
    /// returns the error instead.
    pub fn with_preopen(self, root: PathBuf) -> Self {
        self.legacy_preopen(root, Access::ReadOnly)
    }

    /// Like `with_preopen`, but the guest may create, write, append to and truncate
    /// files under `root`.
    pub fn with_writable_preopen(self, root: PathBuf) -> Self {
        self.legacy_preopen(root, Access::ReadWrite)
    }

    fn legacy_preopen(mut self, root: PathBuf, access: Access) -> Self {
        match self.add_preopen("/".to_string(), root, access) {
            Ok(()) => self,
            Err(ConfigError::Preopen { .. }) => panic!("preopen: directory does not exist"),
            Err(e) => panic!("preopen: {e}"),
        }
    }

    /// The open file behind a descriptor opened for `write`: `read-only` unless the preopen
    /// is writable, `is-directory` for a directory, `bad-descriptor` if it was not opened
    /// for `write`.
    fn writable_descriptor_file(&self, descriptor: u32) -> Result<&std::fs::File, Fs> {
        self.require_writable(descriptor)?;
        let d = self.open_descriptor(descriptor)?;
        let file = d.file.as_ref().ok_or(Fs::IsDirectory)?;
        if d.write {
            Ok(file)
        } else {
            Err(Fs::BadDescriptor)
        }
    }

    /// The open file behind a descriptor opened for reading: `is-directory` for a directory,
    /// `bad-descriptor` if it was not opened for reading.
    fn readable_descriptor_file(&self, descriptor: u32) -> Result<&std::fs::File, Fs> {
        let d = self.open_descriptor(descriptor)?;
        let file = d.file.as_ref().ok_or(Fs::IsDirectory)?;
        if d.read {
            Ok(file)
        } else {
            Err(Fs::BadDescriptor)
        }
    }

    /// A new output stream over `writable_descriptor_file`, from `pos` (`append`: always
    /// at its end): the handle.
    fn open_output_stream(&mut self, descriptor: u32, pos: u64, append: bool) -> Result<u32, Fs> {
        let stream = FileStream::over(self.writable_descriptor_file(descriptor)?, pos, append)?;
        Ok(FIRST_FILE_STREAM + self.outputs.insert(stream) as u32)
    }

    /// Runs `op` on the open file behind a descriptor, or on the directory's own open handle.
    fn with_descriptor_file<T>(
        &self,
        descriptor: u32,
        op: impl FnOnce(&std::fs::File) -> std::io::Result<T>,
    ) -> Result<T, Fs> {
        let d = self.open_descriptor(descriptor)?;
        let done = match (&d.file, &d.dir) {
            (Some(file), _) => op(file),
            (None, Some(dir)) => op(&dir.open(".").map_err(sandbox_error)?.into_std()),
            (None, None) => return Err(Fs::BadDescriptor),
        };
        done.map_err(io_error_code)
    }

    /// Flushes the file or directory behind a descriptor to disk (`data_only`: without its
    /// metadata).
    fn sync_descriptor(&self, descriptor: u32, data_only: bool) -> Result<(), Fs> {
        // Windows refuses to flush a handle without write access (`FlushFileBuffers`), and a
        // directory. Such a handle has no writes of its own to flush, so answer as Unix does.
        #[cfg(windows)]
        {
            let d = self.open_descriptor(descriptor)?;
            if d.dir.is_some() || !d.write {
                return Ok(());
            }
        }
        self.with_descriptor_file(descriptor, |file| {
            if data_only {
                file.sync_data()
            } else {
                file.sync_all()
            }
        })
    }

    /// `Ok` if the preopen that `descriptor` descends from is writable, else `read-only`
    /// (`bad-descriptor` for a handle that names no descriptor).
    fn require_writable(&self, descriptor: u32) -> Result<(), Fs> {
        let preopen = self.open_descriptor(descriptor)?.preopen;
        match self.preopens.get(preopen) {
            Some(p) if p.writable => Ok(()),
            _ => Err(Fs::ReadOnly),
        }
    }

    /// The descriptor behind a handle, or `bad-descriptor`.
    fn open_descriptor(&self, descriptor: u32) -> Result<&OpenDescriptor, Fs> {
        self.descriptors
            .get(descriptor as usize)
            .ok_or(Fs::BadDescriptor)
    }

    /// The open directory behind a descriptor, or `bad-descriptor` (`not-directory` for a
    /// file). A directory descriptor holds the directory itself, not its path, so renaming
    /// the directory or swapping a symlink into one of its ancestors cannot redirect it.
    fn directory(&self, descriptor: u32) -> Result<&Dir, Fs> {
        self.open_descriptor(descriptor)?
            .dir
            .as_ref()
            .ok_or(Fs::NotDirectory)
    }

    /// The metadata of a descriptor: of its open file or directory.
    fn descriptor_metadata(&self, descriptor: u32) -> Result<Stat, Fs> {
        let d = self.open_descriptor(descriptor)?;
        match (&d.file, &d.dir) {
            (Some(file), _) => file.metadata().map(|m| Stat::from(m).identified_by(file)),
            (None, Some(dir)) => dir.dir_metadata().map(|m| Stat::from(m).identified_by(dir)),
            (None, None) => return Err(Fs::BadDescriptor),
        }
        .map_err(sandbox_error)
    }

    /// The listing of a directory descriptor: a snapshot of the entries (without `.` and
    /// `..`), in the OS's order.
    fn list_directory(&self, descriptor: u32) -> Result<Vec<(DescriptorType, String)>, Fs> {
        self.directory(descriptor)?
            .entries()
            .and_then(|dir| {
                dir.map(|entry| {
                    let entry = entry?;
                    Ok((
                        descriptor_type(&entry.file_type()?),
                        entry.file_name().to_string_lossy().into_owned(),
                    ))
                })
                .collect()
            })
            .map_err(sandbox_error)
    }

    /// The metadata of what `path` names below a directory descriptor: with `symlink-follow`
    /// (or for a path that names a directory itself) what a symlink leads to, else the
    /// entry itself.
    fn path_metadata(&self, dir: u32, flags: PathFlags, path: &str) -> Result<Stat, Fs> {
        let dir = self.directory(dir)?;
        let (named, meta) = if flags.contains(PathFlags::SYMLINK_FOLLOW) || !names_an_entry(path) {
            let named = below(path)?;
            (named, dir.metadata(named))
        } else {
            let named = entry_path(path)?;
            (named, dir.symlink_metadata(named))
        };
        meta.map(|m| Stat::from(m).identified_below(dir, named))
            .map_err(sandbox_error)
    }

    /// The target of the symlink at `path`, if it is relative and stays under the preopen.
    fn read_link_target(&self, dir: u32, path: &str) -> Result<String, Fs> {
        let dir = self.directory(dir)?;
        let entry = entry_path(path)?;
        if !dir
            .symlink_metadata(entry)
            .map_err(sandbox_error)?
            .is_symlink()
        {
            return Err(Fs::Invalid);
        }
        let target = dir.read_link_contents(entry).map_err(sandbox_error)?;
        if !stays_below(&target) {
            return Err(Fs::NotPermitted);
        }
        target.into_os_string().into_string().map_err(|_| Fs::Io)
    }

    fn random_source(&mut self) -> &mut dyn RandomSource {
        match self.random.as_deref_mut() {
            Some(random) => random,
            None => panic!("wasi:random: no random source (enable `os-random` or use the builder)"),
        }
    }

    /// Fills `buf` with secure random bytes; a source that fails traps.
    fn fill_secure(&mut self, buf: &mut [u8]) {
        if let Err(e) = self.random_source().fill_random(buf) {
            panic!("wasi:random: the random source failed: {e}");
        }
    }

    /// `len` secure random bytes. A guest-chosen length above 64 MiB traps (as in wasmtime), so
    /// a guest cannot make the host allocate without bound; so does a source that fails.
    fn secure_bytes(&mut self, len: u64) -> Vec<u8> {
        assert!(len <= 64 << 20, "random bytes: length over 64 MiB");
        let mut bytes = vec![0; len as usize];
        self.fill_secure(&mut bytes);
        bytes
    }

    /// `len` insecure random bytes, with the same length limit.
    fn insecure_bytes(&mut self, len: u64) -> Vec<u8> {
        assert!(len <= 64 << 20, "random bytes: length over 64 MiB");
        let source = self.random_source();
        std::iter::repeat_with(|| source.insecure_u64().to_le_bytes())
            .flatten()
            .take(len as usize)
            .collect()
    }

    /// Starts the thread that reads the real stdin into the buffer (once).
    fn start_stdin(&mut self) {
        use std::io::Read;
        if self.stdin_started {
            return;
        }
        self.stdin_started = true;
        // weak: the thread must not keep the buffer alive, and a dropped host ends it
        let shared = Arc::downgrade(&self.stdin);
        let mut reader = self
            .stdin_reader
            .take()
            .unwrap_or_else(|| Box::new(std::io::stdin()));
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                let (n, error) = match reader.read(&mut chunk) {
                    Ok(n) => (n, None),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // the failure ends the input: `read_stream` reports it after the bytes
                    Err(e) => (0, Some(e.to_string())),
                };
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                let (buf, changed) = &*shared;
                let mut buf = buf.lock().unwrap();
                buf.error = error;
                buf.bytes.extend(&chunk[..n]);
                buf.eof = n == 0;
                changed.notify_all();
                if n == 0 {
                    return;
                }
            }
        });
    }

    fn subscribe(&mut self, deadline: Option<Instant>) -> Pollable {
        self.subscribe_wait(deadline.map_or(Wait::Ready, Wait::Deadline))
    }

    fn subscribe_wait(&mut self, wait: Wait) -> Pollable {
        Pollable(self.pollables.insert(wait) as u32)
    }

    /// What a pollable waits for; an unknown one is ready.
    fn wait_of(&self, pollable: Pollable) -> Wait {
        self.pollables
            .get(pollable.0 as usize)
            .copied()
            .unwrap_or(Wait::Ready)
    }

    /// The positions in `pollables` of the ones that are ready, waiting until at least one
    /// is: on the stdin buffer's condition variable, with a timeout at the earliest deadline
    /// (and every millisecond while a socket is among them, which nothing wakes it for).
    fn wait_ready(&self, pollables: &[Pollable]) -> Vec<u32> {
        let waits: Vec<Wait> = pollables.iter().map(|&p| self.wait_of(p)).collect();
        self.wait_any(&waits)
    }

    /// `wait_ready` for the conditions themselves (a blocking call has no pollable).
    fn wait_any(&self, waits: &[Wait]) -> Vec<u32> {
        let (buf, changed) = &*self.stdin;
        let mut buf = buf.lock().unwrap();
        loop {
            let now = Instant::now();
            let ready: Vec<u32> = (0..waits.len() as u32)
                .filter(|&i| waits[i as usize].is_ready(now, &buf, self))
                .collect();
            if !ready.is_empty() {
                return ready;
            }
            let mut earliest = waits.iter().filter_map(|w| w.deadline()).min();
            if waits.iter().any(|w| w.polls_socket()) {
                let soon = now + Duration::from_millis(1);
                earliest = Some(earliest.map_or(soon, |d| d.min(soon)));
            }
            buf = match earliest {
                Some(d) => {
                    changed
                        .wait_timeout(buf, d.saturating_duration_since(now))
                        .unwrap()
                        .0
                }
                None => changed.wait(buf).unwrap(),
            };
        }
    }
}

/// What an `error` handle stands for: the message, and the filesystem `error-code` when the
/// failure came from a file stream (what `filesystem-error-code` reports).
struct ErrorEntry {
    message: String,
    fs: Option<Fs>,
    /// The `wasi:http` `error-code` when the failure came from a body.
    http: Option<HttpErrorCode>,
}

/// The default of [`WasiHostBuilder::max_request_body`](WasiHostBuilder::max_request_body).
const DEFAULT_MAX_REQUEST_BODY: usize = 1 << 20;

mod impl_host;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn io_errors_map_to_their_filesystem_code() {
        let code = |kind| io_error_code(Error::from(kind));
        assert_eq!(code(ErrorKind::NotFound), Fs::NoEntry);
        assert_eq!(code(ErrorKind::PermissionDenied), Fs::Access);
        assert_eq!(code(ErrorKind::AlreadyExists), Fs::Exist);
        assert_eq!(code(ErrorKind::DirectoryNotEmpty), Fs::NotEmpty);
        assert_eq!(code(ErrorKind::NotADirectory), Fs::NotDirectory);
        assert_eq!(code(ErrorKind::IsADirectory), Fs::IsDirectory);
        assert_eq!(code(ErrorKind::Unsupported), Fs::Unsupported);
        assert_eq!(code(ErrorKind::BrokenPipe), Fs::Io);
    }

    #[test]
    fn only_a_permission_error_without_an_os_number_is_not_permitted() {
        // what the sandbox reports for a path that leads out of a preopen
        let escaped = Error::new(ErrorKind::PermissionDenied, "outside the sandbox");
        assert_eq!(sandbox_error(escaped), Fs::NotPermitted);
        // a real `EACCES` stays `access`
        // (13 is EACCES on Unix; on Windows it is another error, ERROR_ACCESS_DENIED is 5)
        let eacces = if cfg!(windows) { 5 } else { 13 };
        assert_eq!(sandbox_error(Error::from_raw_os_error(eacces)), Fs::Access);
        // any other error is mapped as ever, with or without an OS number
        assert_eq!(
            sandbox_error(Error::new(ErrorKind::NotFound, "gone")),
            Fs::NoEntry
        );
    }

    #[test]
    fn io_errors_map_to_their_network_code() {
        let code = |kind| net_error(&Error::from(kind));
        assert_eq!(code(ErrorKind::AddrInUse), Net::AddressInUse);
        assert_eq!(code(ErrorKind::AddrNotAvailable), Net::AddressNotBindable);
        assert_eq!(code(ErrorKind::PermissionDenied), Net::AccessDenied);
        assert_eq!(code(ErrorKind::ConnectionRefused), Net::ConnectionRefused);
        assert_eq!(code(ErrorKind::ConnectionReset), Net::ConnectionReset);
        assert_eq!(code(ErrorKind::ConnectionAborted), Net::ConnectionAborted);
        assert_eq!(code(ErrorKind::TimedOut), Net::Timeout);
        assert_eq!(code(ErrorKind::WouldBlock), Net::WouldBlock);
        assert_eq!(code(ErrorKind::InvalidInput), Net::InvalidArgument);
        assert_eq!(code(ErrorKind::NotConnected), Net::InvalidState);
        assert_eq!(code(ErrorKind::BrokenPipe), Net::Unknown);
    }

    #[test]
    fn stream_handles_split_into_disjoint_ranges() {
        let h = |base: usize| base as u32;
        // incoming bodies: the range below the outgoing ones
        assert_eq!(http_slot(h(HTTP_STREAM)), Some(0));
        assert_eq!(
            http_slot(h(HTTP_OUT_STREAM) - 1),
            Some(HTTP_OUT_STREAM - HTTP_STREAM - 1)
        );
        assert_eq!(http_slot(h(HTTP_OUT_STREAM)), None);
        assert_eq!(http_slot(h(HTTP_STREAM) - 1), None);
        // outgoing bodies: the range below the connections
        assert_eq!(http_out_slot(h(HTTP_OUT_STREAM)), Some(0));
        assert_eq!(
            http_out_slot(h(TCP_STREAM) - 1),
            Some(TCP_STREAM - HTTP_OUT_STREAM - 1)
        );
        assert_eq!(http_out_slot(h(TCP_STREAM)), None);
        assert_eq!(http_out_slot(h(HTTP_OUT_STREAM) - 1), None);
    }

    #[test]
    fn only_a_path_that_ends_in_a_name_names_an_entry() {
        for named in ["a", "a/b", "./a", "a/.b"] {
            assert!(names_an_entry(named), "{named}");
        }
        for directory in ["", ".", "a/", "a/b/", "a/.", "a/b/."] {
            assert!(!names_an_entry(directory), "{directory}");
        }
    }

    #[test]
    fn a_deadline_is_the_sum_or_a_distant_one_never_a_panic() {
        let base = Instant::now();
        assert_eq!(
            deadline_after(base, 1_000_000),
            base + Duration::from_millis(1)
        );
        // more than any `Instant` may hold on some platforms: still far away, not `base`
        let far = deadline_after(base, u64::MAX);
        assert!(far >= base + Duration::from_secs(50 * 365 * 24 * 60 * 60));
    }
}
