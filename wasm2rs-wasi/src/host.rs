//! A WASI preview2 host written against the typed [`Host`](crate::Host) trait. It has no
//! canonical-ABI code: strings, lists, results and resources arrive as Rust values, and the
//! `Glue` that `wasm2rs --wit-host --glue --external-types=wasm2rs_wasi` generates per component lowers
//! them to the module's flat `Imports`. It covers
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
use cap_std::fs::{Dir, MetadataExt as _};
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
pub use config::{Access, ConfigError, NetworkPolicy, WasiHostBuilder};
pub use http::{IncomingRequestParts, ServedBody, ServedResponse};
#[cfg(feature = "os-random")]
pub use sources::OsRandom;
pub use sources::{
    Clock, HttpClient, HttpClientError, HttpRequest, HttpResponse, InsecureRandom, RandomError,
    RandomSource, SystemClock,
};

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
            (Some(file), _) => file.metadata().map(Stat::from),
            (None, Some(dir)) => dir.dir_metadata().map(Stat::from),
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
        if flags.contains(PathFlags::SYMLINK_FOLLOW) || !names_an_entry(path) {
            dir.metadata(below(path)?)
        } else {
            dir.symlink_metadata(entry_path(path)?)
        }
        .map(Stat::from)
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

/// A preopened directory.
struct Preopen {
    /// The name the guest sees.
    guest_name: String,
    /// Whether the guest may change anything under it.
    writable: bool,
}

/// An open descriptor: a directory (the preopens included) holds the open directory, a file
/// its open `File`. Every path under a directory is resolved below that handle, never from
/// a host path, so what a check saw is what is used.
struct OpenDescriptor {
    /// Where the descriptor was opened from, as a label only: it names the object where the
    /// platform has no device and inode numbers.
    path: PathBuf,
    /// The index in `preopens` of the preopen this descriptor lies under.
    preopen: usize,
    /// Whether a file was opened for `write`.
    write: bool,
    /// Whether a file was opened for reading.
    read: bool,
    file: Option<std::fs::File>,
    dir: Option<Dir>,
}

/// A stream over a duplicate of a descriptor's `File`. Duplicates share one file offset,
/// so each stream keeps its own position and seeks to it before every read or write (the
/// host runs one guest call at a time, so nothing interleaves).
struct FileStream {
    file: std::fs::File,
    pos: u64,
    /// Every write goes to the end of the file.
    append: bool,
}

impl FileStream {
    /// A stream over a duplicate of `file`, from `pos`.
    fn over(file: &std::fs::File, pos: u64, append: bool) -> Result<Self, Fs> {
        Ok(FileStream {
            file: file.try_clone().map_err(io_error_code)?,
            pos,
            append,
        })
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(self.pos))?;
        let n = self.file.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let at = if self.append {
            SeekFrom::End(0)
        } else {
            SeekFrom::Start(self.pos)
        };
        self.pos = self.file.seek(at)?;
        self.file.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
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

/// A table of live handles: a handle is a slot's index, and the slot of a dropped handle
/// is reused by the next `insert`. A guest must not use a handle after dropping it; the
/// host does not detect that, and the stale handle may then name a newer resource.
struct Slab<T> {
    slots: Vec<Option<T>>,
    free: Vec<usize>,
    /// How often each slot was freed: a `SlotRef` taken earlier no longer matches once it grew.
    epochs: Vec<u32>,
}

/// A slot of a `Slab` together with the epoch it had when the reference was made, so a
/// pollable made for a resource cannot be confused with whatever is put in its slot later.
#[derive(Clone, Copy, Debug)]
struct SlotRef {
    slot: usize,
    epoch: u32,
}

impl<T> Slab<T> {
    fn new() -> Self {
        Slab {
            slots: Vec::new(),
            free: Vec::new(),
            epochs: Vec::new(),
        }
    }

    fn insert(&mut self, value: T) -> usize {
        match self.free.pop() {
            Some(slot) => {
                self.slots[slot] = Some(value);
                // a new occupant: a ref made while the slot was free must not match it
                if let Some(epoch) = self.epochs.get_mut(slot) {
                    *epoch = epoch.wrapping_add(1);
                }
                slot
            }
            None => {
                self.slots.push(Some(value));
                self.epochs.push(1);
                self.slots.len() - 1
            }
        }
    }

    fn get(&self, slot: usize) -> Option<&T> {
        self.slots.get(slot)?.as_ref()
    }

    fn get_mut(&mut self, slot: usize) -> Option<&mut T> {
        self.slots.get_mut(slot)?.as_mut()
    }

    /// A reference to what the slot holds now. One made for a free or unknown slot (epoch 0
    /// never occurs) matches nothing, not even what is inserted there later.
    fn slot_ref(&self, slot: usize) -> SlotRef {
        SlotRef {
            slot,
            epoch: self.epochs.get(slot).copied().unwrap_or(0),
        }
    }

    /// What `r` was made for, `None` once it was freed (even if the slot is in use again).
    fn get_ref(&self, r: SlotRef) -> Option<&T> {
        if self.epochs.get(r.slot) != Some(&r.epoch) {
            return None;
        }
        self.get(r.slot)
    }

    /// Frees a slot and hands back its value; `None` for an unknown or already free one.
    fn take(&mut self, slot: usize) -> Option<T> {
        let value = self.slots.get_mut(slot)?.take()?;
        if let Some(epoch) = self.epochs.get_mut(slot) {
            *epoch = epoch.wrapping_add(1);
        }
        self.free.push(slot);
        Some(value)
    }

    /// Frees a slot; an unknown or already free one is ignored.
    fn remove(&mut self, slot: usize) {
        drop(self.take(slot));
    }
}

/// The handle of the first file's stream, input or output (0 to 2 are stdin, stdout, stderr).
const FIRST_FILE_STREAM: u32 = 3;

/// The slot in `files` (input streams) or `outputs` (output streams) of a stream handle
/// (`None` for stdin, stdout, stderr).
fn file_index(stream: u32) -> Option<usize> {
    stream.checked_sub(FIRST_FILE_STREAM).map(|i| i as usize)
}

/// The filesystem `error-code`s this host returns; `From<Fs> for FsErrorCode` below.
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
enum Fs {
    Access,
    BadDescriptor,
    Exist,
    Invalid,
    Io,
    IsDirectory,
    Loop,
    NoEntry,
    NotDirectory,
    NotEmpty,
    Unsupported,
    NotPermitted,
    ReadOnly,
}

fn io_error_code(e: std::io::Error) -> Fs {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => Fs::NoEntry,
        PermissionDenied => Fs::Access,
        AlreadyExists => Fs::Exist,
        DirectoryNotEmpty => Fs::NotEmpty,
        NotADirectory => Fs::NotDirectory,
        IsADirectory => Fs::IsDirectory,
        Unsupported => Fs::Unsupported,
        _ => Fs::Io,
    }
}

/// A symlink named `link` below `dir` that holds `target`; hosts without symlinks answer
/// `unsupported`.
fn make_symlink(dir: &Dir, target: &str, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return dir.symlink(target, link);
    #[cfg(not(unix))]
    {
        let _ = (dir, target, link);
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// An error of an operation below a directory handle. The sandbox reports a path that leads
/// out (a symlink or `..` above the handle) as a permission error without an OS error number;
/// that is `not-permitted`, whereas a real `EACCES` stays `access`.
fn sandbox_error(e: std::io::Error) -> Fs {
    if e.kind() == std::io::ErrorKind::PermissionDenied && e.raw_os_error().is_none() {
        Fs::NotPermitted
    } else {
        io_error_code(e)
    }
}

/// `path` as a path below a directory handle: `not-permitted` if it is absolute or has `..`
/// (such a path could not stay below the descriptor), and the empty path is the directory.
fn below(path: &str) -> Result<&Path, Fs> {
    let relative = Path::new(if path.is_empty() { "." } else { path });
    if stays_below(relative) {
        Ok(relative)
    } else {
        Err(Fs::NotPermitted)
    }
}

/// The entry `path` names, which is not followed if it is a symlink: its parent is resolved
/// below the directory handle. A name ending in `/` or `/.` names a directory, which
/// `file_name` would silently drop: `invalid`.
fn entry_path(path: &str) -> Result<&Path, Fs> {
    let relative = Path::new(path);
    if !stays_below(relative) {
        return Err(Fs::NotPermitted);
    }
    if path.ends_with('/') || path.ends_with("/.") {
        return Err(Fs::Invalid);
    }
    relative.file_name().ok_or(Fs::Invalid)?;
    Ok(relative)
}

/// Whether the entry `name` below `dir` is a symlink (not if it cannot be looked up).
fn is_symlink(dir: &Dir, name: &Path) -> bool {
    dir.symlink_metadata(name)
        .is_ok_and(|meta| meta.is_symlink())
}

/// Whether `path` is relative and has no `..`, so it stays under the directory it is
/// joined to.
fn stays_below(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

// Sockets: the types and helpers below use `std` only.

/// The bytes the stdin thread has read and not yet handed to the guest.
#[derive(Default)]
struct StdinBuf {
    bytes: VecDeque<u8>,
    eof: bool,
    /// Why the input ended, when a read failed instead of reaching the end; taken when reported.
    error: Option<String>,
}

type SharedStdin = Arc<(Mutex<StdinBuf>, Condvar)>;

/// What a pollable waits for.
#[derive(Clone, Copy)]
enum Wait {
    /// Always ready.
    Ready,
    /// Ready at the instant.
    Deadline(Instant),
    /// Ready when stdin has data or is at its end.
    Stdin,
    /// Ready when the listening socket in the slot has a connection to accept (or is not
    /// listening).
    TcpAccept(SlotRef),
    /// Ready when the stream in `tcp_in` at the slot has data, is at its end or failed.
    TcpReadable(SlotRef),
    /// Ready when the stream in `tcp_out` at the slot has room in its send queue (after it
    /// was drained as far as the socket takes) or failed.
    TcpWritable(SlotRef),
    /// Ready when the stream in `udp_in` at the slot has a datagram it would return (or failed).
    UdpReadable(SlotRef),
    /// Ready when the future in `http_futures` at the slot has its result.
    HttpResponse(SlotRef),
    /// Ready when the body in `http_bodies` at the slot has bytes or its end to read.
    HttpReadable(SlotRef),
}

impl Wait {
    fn is_ready(self, now: Instant, stdin: &StdinBuf, host: &WasiHost) -> bool {
        match self {
            Wait::Ready => true,
            Wait::Deadline(d) => d <= now,
            Wait::Stdin => !stdin.bytes.is_empty() || stdin.eof,
            Wait::TcpAccept(r) => host.accept_ready(r),
            Wait::TcpReadable(r) => host.readable(r),
            Wait::TcpWritable(r) => host.tcp_writable(r),
            Wait::UdpReadable(r) => host.udp_readable(r),
            Wait::HttpResponse(r) => host.http_future_ready(r),
            Wait::HttpReadable(r) => host.http_body_readable(r),
        }
    }

    /// Whether readiness can only be found by asking the socket again.
    fn polls_socket(self) -> bool {
        matches!(
            self,
            Wait::TcpAccept(_)
                | Wait::TcpReadable(_)
                | Wait::TcpWritable(_)
                | Wait::UdpReadable(_)
                | Wait::HttpResponse(_)
                | Wait::HttpReadable(_)
        )
    }

    fn deadline(self) -> Option<Instant> {
        match self {
            Wait::Deadline(d) => Some(d),
            _ => None,
        }
    }
}

/// The first handle of the streams of TCP connections: the slot in `tcp_in` or `tcp_out`
/// is added to it (file stream handles stay far below).
const TCP_STREAM: usize = 1 << 30;

/// The first handle of the streams of incoming bodies (`wasi:http`): the slot in `http_bodies`
/// is added to it. The range lies between the file streams and `TCP_STREAM`.
const HTTP_STREAM: usize = 1 << 29;

/// The first handle of the streams of outgoing bodies: the slot in `http_out_bodies` is added to
/// it. Input streams take the range below it.
const HTTP_OUT_STREAM: usize = HTTP_STREAM + (1 << 28);

/// The default of [`WasiHostBuilder::max_request_body`](WasiHostBuilder::max_request_body).
const DEFAULT_MAX_REQUEST_BODY: usize = 1 << 20;

/// The slot in `http_bodies` of a stream handle, `None` for any other stream.
fn http_slot(stream: u32) -> Option<usize> {
    let slot = (stream as usize).checked_sub(HTTP_STREAM)?;
    (slot < HTTP_OUT_STREAM - HTTP_STREAM).then_some(slot)
}

/// The slot in `http_out_bodies` of a stream handle, `None` for any other stream.
fn http_out_slot(stream: u32) -> Option<usize> {
    let slot = (stream as usize).checked_sub(HTTP_OUT_STREAM)?;
    (slot < TCP_STREAM - HTTP_OUT_STREAM).then_some(slot)
}

/// The slot in `tcp_in` or `tcp_out` of a stream handle, `None` for any other stream.
fn tcp_slot(stream: u32) -> Option<usize> {
    (stream as usize).checked_sub(TCP_STREAM)
}

/// The `wasi:sockets` `error-code`s this host returns; `From<Net> for NetErrorCode` below.
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
enum Net {
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

fn net_error(e: &std::io::Error) -> Net {
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
enum NetState {
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
struct TcpOptions {
    keep_alive_enabled: bool,
    /// Nanoseconds.
    keep_alive_idle_time: u64,
    /// Nanoseconds.
    keep_alive_interval: u64,
    keep_alive_count: u32,
    hop_limit: u8,
    receive_buffer_size: u64,
    send_buffer_size: u64,
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

struct NetSocket {
    ipv6: bool,
    state: NetState,
    options: TcpOptions,
}

/// How many bytes a connection's send queue holds at most.
const SEND_QUEUE_CAPACITY: usize = 64 * 1024;

/// The output side of a connection: the non-blocking socket and the host's send queue in front
/// of it. Bytes a guest writes are appended to the queue and moved to the socket as far as it
/// takes them, so a peer that does not read fills the queue (`check-write` reports 0, the
/// pollable is not ready) instead of making `write` wait. A failure of the socket (a dead
/// peer) marks the stream failed and drops the queue; the first operation that finds out answers
/// `last-operation-failed` (with an `error` handle), then the stream answers `closed`. The
/// flushing methods (`flush_waiting`, `flush_until`) wait for the peer to read, so they do not
/// return while a live peer never does. The queue is behind a `RefCell` because a pollable
/// asks for readiness through `&WasiHost`.
struct TcpOut {
    /// Unique among the host's connections (see `NetState::Connected`).
    id: u64,
    stream: TcpStream,
    queue: std::cell::RefCell<VecDeque<u8>>,
    /// Why the socket failed: the stream is marked failed (this is `Some`) and the queue was
    /// dropped. The first operation that finds out takes it and answers
    /// `last-operation-failed`.
    failure: std::cell::RefCell<Option<String>>,
    /// The failure was reported: from then on the stream answers `closed`.
    reported: std::cell::Cell<bool>,
    /// A `flush` found bytes still queued: until they are all sent, `check-write` is 0.
    flush_pending: std::cell::Cell<bool>,
}

impl TcpOut {
    fn new(id: u64, stream: TcpStream) -> Self {
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
    fn drain(&self) {
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

    fn fail(&self, queue: &mut VecDeque<u8>, why: String) {
        queue.clear();
        *self.failure.borrow_mut() = Some(why);
    }

    /// Why the socket failed, once: the first call after the failure returns the reason (the
    /// caller answers `last-operation-failed`), then the stream is `closed`.
    fn take_failure(&self) -> Option<String> {
        let why = self.failure.borrow_mut().take();
        if why.is_some() {
            self.reported.set(true);
        }
        why
    }

    /// Whether the socket failed (reported or not).
    fn failed(&self) -> bool {
        self.failure.borrow().is_some() || self.reported.get()
    }

    /// What `check-write` permits after a drain: 4096 at most, less with bytes queued, 0 while
    /// the queue is full or a flush is pending; `None` once the stream failed.
    fn permit(&self) -> Option<u64> {
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
    fn push(&self, bytes: &[u8]) {
        if !self.failed() {
            self.queue.borrow_mut().extend(bytes);
            self.drain();
        }
    }

    /// `flush`: drains as far as the socket takes; if bytes remain the flush is pending.
    fn flush(&self) {
        self.drain();
        if !self.failed() && !self.queue.borrow().is_empty() {
            self.flush_pending.set(true);
        }
    }

    /// Drains the whole queue, waiting while the socket is full, until `deadline` if there is
    /// one (what is left stays queued).
    fn flush_until(&self, deadline: Option<Instant>) {
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

    fn flush_waiting(&self) {
        self.flush_until(None);
    }
}

impl WasiHost {
    /// `last-operation-failed` for a failure of the last operation on a stream: a fresh
    /// `error` handle (one per call, live until the guest drops it) that carries `message`.
    /// The caller reports it once: the stream is `closed` from then on.
    fn last_operation_failed(&mut self, message: impl std::fmt::Display) -> StreamError {
        self.last_operation_failed_with(message, None, None)
    }

    /// `last_operation_failed`, remembering the filesystem or HTTP `error-code` behind the failure.
    fn last_operation_failed_with(
        &mut self,
        message: impl std::fmt::Display,
        fs: Option<Fs>,
        http: Option<HttpErrorCode>,
    ) -> StreamError {
        let entry = ErrorEntry {
            message: message.to_string(),
            fs,
            http,
        };
        StreamError::LastOperationFailed(Error(self.errors.insert(entry) as u32))
    }

    /// `last_operation_failed` for an I/O failure of `stream`: only a file stream's carries a
    /// filesystem `error-code`; a connection's is not a filesystem error. (The streams of http
    /// bodies report their failures elsewhere and never get here.)
    fn stream_failed(&mut self, stream: u32, e: &std::io::Error) -> StreamError {
        let is_file = tcp_slot(stream).is_none() && file_index(stream).is_some();
        let fs = is_file.then(|| io_error_code(std::io::Error::from(e.kind())));
        self.last_operation_failed_with(e, fs, None)
    }

    /// The first read failure of the input stream `stream` is `last-operation-failed`; the
    /// stream is `closed` after it (see `read_stream`).
    fn fail_input(&mut self, stream: u32, e: &std::io::Error) -> StreamError {
        self.failed_in.insert(stream);
        self.stream_failed(stream, e)
    }

    /// `fail_input` for the output stream `stream` that is not a connection.
    fn fail_output(&mut self, stream: u32, e: &std::io::Error) -> StreamError {
        self.failed_out.insert(stream);
        self.stream_failed(stream, e)
    }

    /// Why the output stream in `tcp_out` that `r` refers to cannot be written: `last-operation-failed`
    /// the first time after its socket failed, `closed` after that and when it was dropped.
    fn tcp_out_error(&mut self, slot: usize) -> StreamError {
        match self.tcp_out.get(slot).and_then(TcpOut::take_failure) {
            Some(why) => self.last_operation_failed(why),
            None => StreamError::Closed,
        }
    }

    /// `Ok` while the connection in `tcp_out` at `slot` has not failed, else its error (see
    /// `tcp_out_error`).
    fn tcp_status(&mut self, slot: usize) -> Result<(), StreamError> {
        match self.tcp_out.get(slot) {
            Some(conn) if !conn.failed() => Ok(()),
            _ => Err(self.tcp_out_error(slot)),
        }
    }
}

impl WasiHost {
    /// Reads at most 4096 bytes of a stream without waiting: the one read path of `read`,
    /// `blocking-read`, `skip`, `blocking-skip`, `splice` and `blocking-splice`.
    fn read_stream(&mut self, stream: u32, len: u64) -> Result<Vec<u8>, StreamError> {
        let mut buf = vec![0u8; len.min(4096) as usize];
        if stream == 0 {
            self.start_stdin();
            let stdin = Arc::clone(&self.stdin);
            let mut shared = stdin.0.lock().unwrap();
            // a zero length consumes nothing: ok while the input is open, like a read with data left
            let n = std::io::Read::read(&mut shared.bytes, &mut buf).unwrap_or(0); // a `VecDeque` never fails
            if n == 0 && shared.bytes.is_empty() && shared.eof {
                // the end of the input: `closed`, unless a read failed: that is reported once,
                // after the bytes before it, and the input is `closed` from then on
                return Err(match shared.error.take() {
                    Some(why) => self.last_operation_failed(why),
                    None => StreamError::Closed,
                });
            }
            buf.truncate(n);
            return Ok(buf);
        }
        if self.failed_in.contains(&stream) {
            // a failure was reported: the stream is closed, a zero-length read included
            return Err(StreamError::Closed);
        }
        if let Some(slot) = http_slot(stream) {
            let body = self
                .http_bodies
                .get(slot)
                .expect("read from an unknown or dropped stream")
                .shared();
            return match body.read(buf.len()) {
                http::BodyRead::Data(data) => Ok(data),
                http::BodyRead::Closed => Err(StreamError::Closed),
                http::BodyRead::Failed(kind, why) => {
                    self.failed_in.insert(stream);
                    let code = http::body_error_code(kind, &why);
                    Err(self.last_operation_failed_with(why, None, Some(code)))
                }
            };
        }
        if let Some(slot) = tcp_slot(stream) {
            // never blocks: an empty list means "nothing yet"
            let conn = self
                .tcp_in
                .get_mut(slot)
                .expect("read from an unknown or dropped stream");
            if buf.is_empty() {
                // a zero length succeeds only while the connection is open: peek, consume nothing
                return match conn.peek(&mut [0u8; 1]) {
                    Ok(0) => Err(StreamError::Closed),
                    Ok(_) => Ok(buf),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(buf),
                    Err(e) => Err(self.fail_input(stream, &e)),
                };
            }
            return match std::io::Read::read(conn, &mut buf) {
                Ok(0) => Err(StreamError::Closed),
                Ok(n) => {
                    buf.truncate(n);
                    Ok(buf)
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(Vec::new()),
                Err(e) => Err(self.fail_input(stream, &e)),
            };
        }
        let Some(index) = file_index(stream) else {
            return Err(StreamError::Closed);
        };
        let file = self
            .files
            .get_mut(index)
            .expect("read from an unknown or dropped stream");
        if buf.is_empty() {
            // a zero length succeeds only before the end of the file: look, consume nothing
            let at_end = file
                .file
                .metadata()
                .map_or(true, |meta| file.pos >= meta.len());
            return if at_end {
                Err(StreamError::Closed)
            } else {
                Ok(buf)
            };
        }
        match file.read(&mut buf) {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Ok(buf)
            }
            Ok(_) => Err(StreamError::Closed), // the end of the file
            Err(e) => Err(self.fail_input(stream, &e)),
        }
    }

    /// `read_stream`, but waits (as the stream's `subscribe` would) while an open stream has
    /// nothing yet: stdin and connections; a file never waits. A zero length waits, too, until
    /// the stream is readable (data or its end), then answers like `read_stream(.., 0)`: ok
    /// and empty while open, `closed` at the end.
    fn read_stream_waiting(&mut self, stream: u32, len: u64) -> Result<Vec<u8>, StreamError> {
        loop {
            let data = self.read_stream(stream, len)?;
            if !data.is_empty() {
                return Ok(data);
            }
            let wait = match (tcp_slot(stream), http_slot(stream)) {
                (Some(slot), _) => Wait::TcpReadable(self.tcp_in.slot_ref(slot)),
                (None, Some(slot)) => Wait::HttpReadable(self.http_bodies.slot_ref(slot)),
                (None, None) if stream == 0 => Wait::Stdin,
                (None, None) => return Ok(data),
            };
            self.wait_any(&[wait]);
            if len == 0 {
                return self.read_stream(stream, 0);
            }
        }
    }

    /// Whether an output stream can still be written (not dropped or unknown): `splice`
    /// asks before it reads, so a dead output does not consume the source's bytes.
    fn output_is_open(&self, stream: u32) -> bool {
        if let Some(slot) = tcp_slot(stream) {
            self.tcp_out.get(slot).is_some()
        } else if let Some(slot) = http_out_slot(stream) {
            self.http_out_bodies.get(slot).is_some()
        } else if let Some(index) = file_index(stream) {
            self.outputs.get(index).is_some()
        } else {
            true // stdout and stderr
        }
    }

    /// How many bytes `splice` may read for `stream`: `len`, for a connection at most its
    /// `check-write` permit. With `wait` the queue is emptied first (waiting for the peer).
    fn splice_budget(&mut self, stream: u32, len: u64, wait: bool) -> Result<u64, StreamError> {
        let Some(slot) = tcp_slot(stream) else {
            return Ok(len);
        };
        let conn = self.tcp_out.get(slot).ok_or(StreamError::Closed)?;
        if wait {
            conn.flush_waiting();
        }
        match conn.permit() {
            Some(permit) => Ok(len.min(permit)),
            None => Err(self.tcp_out_error(slot)),
        }
    }

    /// Traps if a non-blocking write of `len` bytes to a connection is more than `check-write`
    /// permits now (its send queue's room, up to 4096); for a connection that failed or is gone it
    /// answers like `check-write` (`last-operation-failed` once, then `closed`).
    /// The other streams are not checked here.
    fn assert_within_permit(&mut self, stream: u32, len: u64, op: &str) -> Result<(), StreamError> {
        if let Some(slot) = tcp_slot(stream) {
            let permit = self
                .tcp_permit(slot)
                .ok_or_else(|| self.tcp_out_error(slot))?;
            assert!(len <= permit, "{op}: more than check-write allows");
        }
        Ok(())
    }

    /// Writes all of `contents` to a stream: the one write path of `write`,
    /// `blocking-write-and-flush`, `write-zeroes`, `blocking-write-zeroes-and-flush`,
    /// `splice` and `blocking-splice`. A failed write is `last-operation-failed` once (the first
    /// time the failure shows), then the stream is `closed`. A connection's bytes go
    /// through its send queue (see `TcpOut`), so their order is kept: with `wait` false
    /// (`write`, and `splice` / `blocking-splice`, which read within the permit) they are queued
    /// and sent as far as the socket takes them; with `wait` the queue is then emptied, waiting
    /// for the peer to read.
    fn write_stream(
        &mut self,
        stream: u32,
        contents: &[u8],
        wait: bool,
    ) -> Result<(), StreamError> {
        let written = if let Some(slot) = tcp_slot(stream) {
            let conn = self.tcp_out.get(slot).ok_or(StreamError::Closed)?;
            conn.push(contents);
            if wait {
                conn.flush_waiting();
            }
            return self.tcp_status(slot);
        } else if self.failed_out.contains(&stream) {
            return Err(StreamError::Closed); // a failure was reported: the stream is closed
        } else if let Some(slot) = http_out_slot(stream) {
            return self.http_out_write(stream, slot, contents);
        } else {
            match (stream, file_index(stream)) {
                (_, Some(index)) => match self.outputs.get_mut(index) {
                    Some(file) => file.write_all(contents),
                    None => Err(std::io::ErrorKind::NotFound.into()),
                },
                (_, None) => self.with_stdio(stream, |w| w.write_all(contents)),
            }
        };
        written.map_err(|e| match e.kind() {
            // a dropped file output (see above) is gone, not failed
            std::io::ErrorKind::NotFound => StreamError::Closed,
            _ => self.fail_output(stream, &e),
        })
    }

    /// Appends to the body behind an output stream; a body over the limit fails the stream
    /// (`last-operation-failed` once, then `closed`).
    fn http_out_write(
        &mut self,
        stream: u32,
        slot: usize,
        contents: &[u8],
    ) -> Result<(), StreamError> {
        self.http_out_handle(slot)?
            .push(contents)
            .map_err(|code| self.fail_http_out(stream, code))
    }

    /// `check-write` of an output stream that is a request body.
    fn http_out_permit(&mut self, stream: u32, slot: usize) -> Result<u64, StreamError> {
        if self.failed_out.contains(&stream) {
            return Err(StreamError::Closed);
        }
        self.http_out_handle(slot)?
            .permit()
            .map_err(|code| self.fail_http_out(stream, code))
    }

    /// The buffer behind an output stream; a body already finished or dropped is `closed`.
    fn http_out_handle(&self, slot: usize) -> Result<http::OutBodyHandle, StreamError> {
        self.http_out_bodies
            .get(slot)
            .map(http::HttpOutgoingBody::handle)
            .ok_or(StreamError::Closed)
    }

    fn fail_http_out(&mut self, stream: u32, code: HttpErrorCode) -> StreamError {
        self.failed_out.insert(stream);
        self.last_operation_failed_with(format!("{code:?}"), None, Some(code))
    }

    /// Runs `f` on stderr for stream 2 and on stdout otherwise: the given writer, else the
    /// process's.
    fn with_stdio<T>(&mut self, stream: u32, f: impl FnOnce(&mut dyn Write) -> T) -> T {
        let given = if stream == 2 {
            &mut self.stderr
        } else {
            &mut self.stdout
        };
        match given {
            Some(w) => f(w.as_mut()),
            None if stream == 2 => f(&mut std::io::stderr()),
            None => f(&mut std::io::stdout()),
        }
    }

    /// Flushes stdout and stderr (an exit and a drop end with it); a failure has nobody left to
    /// tell, so it is dropped.
    fn flush_stdio(&mut self) {
        self.with_stdio(1, |w| w.flush()).ok();
        self.with_stdio(2, |w| w.flush()).ok();
    }

    /// Flushes a stream; only stdout, stderr (when given) and a connection's send queue
    /// buffer anything. With `wait` (`blocking-flush`, and what a blocking write ends with)
    /// the queue is emptied; without it (`flush`) it is drained as far as the socket takes now
    /// and the flush stays pending.
    fn flush_stream(&mut self, stream: u32, wait: bool) -> Result<(), StreamError> {
        if let Some(slot) = tcp_slot(stream) {
            let conn = self.tcp_out.get(slot).ok_or(StreamError::Closed)?;
            if wait {
                conn.flush_waiting();
            } else {
                conn.flush();
            }
            return self.tcp_status(slot);
        }
        if self.failed_out.contains(&stream) {
            return Err(StreamError::Closed); // a failure was reported: the stream is closed
        }
        if file_index(stream).is_some() {
            return Ok(()); // a file holds nothing
        }
        let flushed = match stream {
            1 | 2 => self.with_stdio(stream, |w| w.flush()),
            _ => Ok(()),
        };
        flushed.map_err(|e| self.fail_output(stream, &e))
    }

    /// Lets the guest use addresses outside loopback (the default is 127.0.0.0/8 and ::1).
    pub fn with_external_network(mut self) -> Self {
        self.network = NetworkPolicy::External;
        self
    }

    /// Whether `accept` on the socket `r` refers to would not block (or the socket is not
    /// listening, so `accept` answers at once). A connection found here is kept for `accept`.
    fn accept_ready(&self, r: SlotRef) -> bool {
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
    fn readable(&self, r: SlotRef) -> bool {
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
    fn tcp_permit(&self, slot: usize) -> Option<u64> {
        self.tcp_out.get(slot)?.permit()
    }

    /// Best effort, at exit and when the host is dropped: sends what the connections' send
    /// queues still hold, waiting for the peers to read for up to 5 seconds in all.
    fn flush_all_connections(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        for conn in self.tcp_out.slots.iter().flatten() {
            conn.flush_until(Some(deadline));
        }
    }

    /// Whether the output stream in `tcp_out` that `r` refers to is writable (or failed or dropped).
    fn tcp_writable(&self, r: SlotRef) -> bool {
        self.tcp_out.get_ref(r).is_none() || self.tcp_permit(r.slot) != Some(0)
    }

    /// The socket behind a handle.
    fn socket(&mut self, handle: u32) -> &mut NetSocket {
        self.sockets
            .get_mut(handle as usize)
            .expect("unknown or dropped tcp-socket")
    }

    /// `check_address` for the address of a peer to connect or send to: also not port 0 nor an
    /// unspecified address (`0.0.0.0`, `::`), which is `invalid-argument` whether or not the
    /// network is allowed. Every refusal of `check_address` that is not about the policy is
    /// `invalid-argument` too, so the order between the two sets of checks cannot be seen.
    fn check_peer(&self, ipv6: bool, addr: &SocketAddr) -> Result<(), Net> {
        if addr.port() == 0 || addr.ip().is_unspecified() {
            return Err(Net::InvalidArgument);
        }
        self.check_address(ipv6, addr)
    }

    /// `Ok` if the guest may use `addr` on a socket of the family `ipv6`: it must be of
    /// that family, a unicast address that is not an IPv4-mapped IPv6 one, and loopback
    /// unless the host allows the network.
    fn check_address(&self, ipv6: bool, addr: &SocketAddr) -> Result<(), Net> {
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
    fn open_tcp_streams(&mut self, stream: TcpStream) -> Result<(usize, usize, NetState), Net> {
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

    fn tcp_start_bind(&mut self, handle: u32, addr: SocketAddr) -> Result<(), Net> {
        let socket = self.socket(handle);
        if !matches!(socket.state, NetState::Unbound) {
            return Err(Net::InvalidState);
        }
        let ipv6 = socket.ipv6;
        self.check_address(ipv6, &addr)?;
        self.socket(handle).state = NetState::BindStarted(addr);
        Ok(())
    }

    fn tcp_finish_bind(&mut self, handle: u32) -> Result<(), Net> {
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

    fn tcp_start_listen(&mut self, handle: u32) -> Result<(), Net> {
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

    fn tcp_finish_listen(&mut self, handle: u32) -> Result<(), Net> {
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
    fn tcp_start_connect(&mut self, handle: u32, addr: SocketAddr) -> Result<(), Net> {
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
    fn tcp_finish_connect(&mut self, handle: u32) -> Result<(usize, usize), Net> {
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
    fn tcp_accept(&mut self, handle: u32) -> Result<(usize, usize, usize), Net> {
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
    fn tcp_address(&mut self, handle: u32, remote: bool) -> Result<SocketAddr, Net> {
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

    fn tcp_shutdown(&mut self, handle: u32, how: Shutdown) -> Result<(), Net> {
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

// The next blocks use the library's generated types.

impl From<Fs> for FsErrorCode {
    fn from(e: Fs) -> Self {
        match e {
            Fs::Access => FsErrorCode::Access,
            Fs::BadDescriptor => FsErrorCode::BadDescriptor,
            Fs::Exist => FsErrorCode::Exist,
            Fs::Invalid => FsErrorCode::Invalid,
            Fs::Io => FsErrorCode::Io,
            Fs::IsDirectory => FsErrorCode::IsDirectory,
            Fs::Loop => FsErrorCode::Loop,
            Fs::NoEntry => FsErrorCode::NoEntry,
            Fs::NotDirectory => FsErrorCode::NotDirectory,
            Fs::NotEmpty => FsErrorCode::NotEmpty,
            Fs::Unsupported => FsErrorCode::Unsupported,
            Fs::NotPermitted => FsErrorCode::NotPermitted,
            Fs::ReadOnly => FsErrorCode::ReadOnly,
        }
    }
}

/// Whether `path` ends in a real name that `symlink-follow` can apply to: not empty, not
/// `.` and without a trailing `/` or `/.` (those name a directory, which is never a link).
fn names_an_entry(path: &str) -> bool {
    !(path.is_empty() || path == "." || path.ends_with('/') || path.ends_with("/."))
}

/// Directory 3, regular file 6, symlink 5 (seen only for a directory entry or a
/// `symlink_metadata` lookup), otherwise unknown.
fn descriptor_type(t: &impl FileKind) -> DescriptorType {
    if t.is_dir() {
        DescriptorType::Directory
    } else if t.is_file() {
        DescriptorType::RegularFile
    } else if t.is_symlink() {
        DescriptorType::SymbolicLink
    } else {
        DescriptorType::Unknown
    }
}

/// `std`'s and the sandbox's file types, which have the same three questions.
trait FileKind {
    fn is_dir(&self) -> bool;
    fn is_file(&self) -> bool;
    fn is_symlink(&self) -> bool;
}

impl FileKind for std::fs::FileType {
    fn is_dir(&self) -> bool {
        std::fs::FileType::is_dir(self)
    }
    fn is_file(&self) -> bool {
        std::fs::FileType::is_file(self)
    }
    fn is_symlink(&self) -> bool {
        std::fs::FileType::is_symlink(self)
    }
}

impl FileKind for cap_std::fs::FileType {
    fn is_dir(&self) -> bool {
        cap_std::fs::FileType::is_dir(self)
    }
    fn is_file(&self) -> bool {
        cap_std::fs::FileType::is_file(self)
    }
    fn is_symlink(&self) -> bool {
        cap_std::fs::FileType::is_symlink(self)
    }
}

/// What the host says about a file: from `std` for an open file, from the sandbox for what a
/// path names.
struct Stat {
    kind: DescriptorType,
    size: u64,
    link_count: u64,
    accessed: Option<Datetime>,
    modified: Option<Datetime>,
    /// Device and inode, which name the file within one run (zero where there are none).
    id: (u64, u64),
}

impl From<std::fs::Metadata> for Stat {
    fn from(meta: std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let (link_count, id) = {
            use std::os::unix::fs::MetadataExt;
            (meta.nlink(), (meta.dev(), meta.ino()))
        };
        #[cfg(not(unix))]
        let (link_count, id) = (1u64, (0u64, 0u64));
        Stat {
            kind: descriptor_type(&meta.file_type()),
            size: meta.len(),
            link_count,
            accessed: datetime(meta.accessed()),
            modified: datetime(meta.modified()),
            id,
        }
    }
}

impl From<cap_std::fs::Metadata> for Stat {
    fn from(meta: cap_std::fs::Metadata) -> Self {
        Stat {
            kind: descriptor_type(&meta.file_type()),
            size: meta.len(),
            link_count: meta.nlink(),
            accessed: datetime(meta.accessed().map(cap_std::time::SystemTime::into_std)),
            modified: datetime(meta.modified().map(cap_std::time::SystemTime::into_std)),
            id: (meta.dev(), meta.ino()),
        }
    }
}

fn datetime(time: std::io::Result<std::time::SystemTime>) -> Option<Datetime> {
    let since = time.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(Datetime {
        seconds: since.as_secs(),
        nanoseconds: since.subsec_nanos(),
    })
}

/// The status-change time is not reported.
fn descriptor_stat(meta: Stat) -> DescriptorStat {
    DescriptorStat {
        type_: meta.kind,
        link_count: meta.link_count,
        size: meta.size,
        data_access_timestamp: meta.accessed,
        data_modification_timestamp: meta.modified,
        status_change_timestamp: None,
    }
}

/// `base` plus `when` nanoseconds. A guest can ask for more than an `Instant` holds (it
/// depends on the platform), and adding that would panic; such a deadline is replaced by a
/// distant one that is representable (a century, or a year if even that is not), so it is
/// never reached in practice; only if not even a year fits is `base` itself returned.
fn deadline_after(base: Instant, when: u64) -> Instant {
    const YEAR: u64 = 365 * 24 * 60 * 60;
    [
        Duration::from_nanos(when),
        Duration::from_secs(100 * YEAR),
        Duration::from_secs(YEAR),
    ]
    .into_iter()
    .find_map(|d| base.checked_add(d))
    .unwrap_or(base)
}

/// What names a file within one run: its device and inode (its path where there are none).
fn file_identity(meta: &Stat, path: &Path) -> (u64, u64, String) {
    #[cfg(unix)]
    {
        let _ = path;
        (meta.id.0, meta.id.1, String::new())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (0u64, 0u64, path.to_string_lossy().into_owned())
    }
}

/// A hash of the file's identity (see `file_identity`), so the same file gives the same
/// value within one run and two files never do.
fn metadata_hash(meta: &Stat, path: &Path) -> MetadataHashValue {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let identity = file_identity(meta, path);
    let hash = |salt: u8| {
        let mut hasher = DefaultHasher::new();
        (salt, &identity).hash(&mut hasher);
        hasher.finish()
    };
    MetadataHashValue {
        lower: hash(0),
        upper: hash(1),
    }
}

impl WasiHost {
    /// Whether two descriptors name the same file or directory; one the host cannot read
    /// (unknown, or a directory that was moved out of the preopen) is not the same as anything.
    fn same_object(&self, a: u32, b: u32) -> bool {
        let identity = |descriptor: u32| {
            let path = &self.open_descriptor(descriptor).ok()?.path;
            Some(file_identity(
                &self.descriptor_metadata(descriptor).ok()?,
                path,
            ))
        };
        matches!((identity(a), identity(b)), (Some(x), Some(y)) if x == y)
    }
}

/// The times of a `set-times` call. Nanoseconds of a second or more, or a time too far
/// ahead, are `invalid`.
fn file_times(access: NewTimestamp, modification: NewTimestamp) -> Result<std::fs::FileTimes, Fs> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let time = |t: NewTimestamp| match t {
        NewTimestamp::NoChange => Ok(None),
        NewTimestamp::Now => Ok(Some(SystemTime::now())),
        NewTimestamp::Timestamp(Datetime {
            seconds,
            nanoseconds,
        }) if nanoseconds < 1_000_000_000 => {
            let since = Duration::new(seconds, nanoseconds);
            UNIX_EPOCH.checked_add(since).map(Some).ok_or(Fs::Invalid)
        }
        NewTimestamp::Timestamp(_) => Err(Fs::Invalid),
    };
    let mut times = std::fs::FileTimes::new();
    if let Some(t) = time(access)? {
        times = times.set_accessed(t);
    }
    if let Some(t) = time(modification)? {
        times = times.set_modified(t);
    }
    Ok(times)
}

/// Where a UDP socket is in its life: unbound, `start-bind` called, or bound (a non-blocking
/// `std` socket, shared with the streams made from it; `remote` is the default peer of the
/// last `stream` call).
enum UdpState {
    Unbound,
    BindStarted(SocketAddr),
    Bound {
        socket: Arc<StdUdpSocket>,
        remote: Option<SocketAddr>,
    },
}

struct UdpSock {
    ipv6: bool,
    state: UdpState,
    /// How many times `stream` was called; only the streams of the last call work (see `UdpEnd`).
    generation: Arc<AtomicU64>,
}

/// One datagram stream over a bound socket: the peer it was made for (`stream(Some(..))`).
struct UdpEnd {
    ipv6: bool,
    socket: Arc<StdUdpSocket>,
    remote: Option<SocketAddr>,
    /// The socket's `generation` counter, and its value when this stream was made: a stream is
    /// stale once `stream` was called again.
    current: Arc<AtomicU64>,
    generation: u64,
}

impl UdpEnd {
    fn is_stale(&self) -> bool {
        self.current.load(Ordering::SeqCst) != self.generation
    }

    /// Traps (the spec lets a replaced stream trap) if `stream` was called again since.
    fn require_current(&self) {
        assert!(
            !self.is_stale(),
            "stale datagram stream: `stream` was called again"
        );
    }
}

/// Whether two addresses name the same endpoint: the IP and the port. (`SocketAddr`'s `==`
/// also compares an IPv6 address's flow info and scope id, which a sender's address may lack.)
fn same_endpoint(a: SocketAddr, b: SocketAddr) -> bool {
    a.ip() == b.ip() && a.port() == b.port()
}

/// Whether a datagram from `from` is for a stream made for `remote` (any sender if none).
fn from_peer(remote: Option<SocketAddr>, from: SocketAddr) -> bool {
    remote.is_none_or(|peer| same_endpoint(peer, from))
}

/// The most datagrams one `send` takes (what `check-send` permits).
const UDP_SEND_BATCH: u64 = 1024;

/// The most bytes in a UDP datagram over IPv4 and over IPv6.
const UDP_MAX_PAYLOAD: usize = 65507;
const UDP_MAX_PAYLOAD_V6: usize = 65527;

/// Whether `name` (without a trailing dot) is a syntactically valid host name: dot-separated
/// labels of 1 to 63 letters, digits, `-` and `_` that do not start or end with `-`. A name of
/// only digits and dots is not (an address that failed to parse is not a name).
fn is_host_name(name: &str) -> bool {
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
    fn udp_socket(&mut self, handle: u32) -> &mut UdpSock {
        self.udp_sockets
            .get_mut(handle as usize)
            .expect("unknown or dropped udp-socket")
    }

    fn udp_start_bind(&mut self, handle: u32, addr: SocketAddr) -> Result<(), Net> {
        let socket = self.udp_socket(handle);
        if !matches!(socket.state, UdpState::Unbound) {
            return Err(Net::InvalidState);
        }
        let ipv6 = socket.ipv6;
        self.check_address(ipv6, &addr)?;
        self.udp_socket(handle).state = UdpState::BindStarted(addr);
        Ok(())
    }

    fn udp_finish_bind(&mut self, handle: u32) -> Result<(), Net> {
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
    fn udp_address(&mut self, handle: u32, remote: bool) -> Result<SocketAddr, Net> {
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
    fn udp_stream(
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
    fn udp_readable(&self, r: SlotRef) -> bool {
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
    fn udp_receive(&mut self, slot: u32, max: u64) -> Result<Vec<(Vec<u8>, SocketAddr)>, Net> {
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
    fn udp_check_send(&self, slot: u32) {
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
    fn udp_send(
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
    fn resolve_name(&self, name: &str) -> Result<VecDeque<IpAddr>, Net> {
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

fn socket_addr(addr: IpSocketAddress) -> SocketAddr {
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

fn ip_socket_address(addr: SocketAddr) -> IpSocketAddress {
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

fn ip_address(addr: IpAddr) -> IpAddress {
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
        assert_eq!(sandbox_error(Error::from_raw_os_error(13)), Fs::Access);
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
        let h = |base: usize, offset: usize| (base + offset) as u32;
        // incoming bodies: the range below the outgoing ones
        assert_eq!(http_slot(h(HTTP_STREAM, 0)), Some(0));
        assert_eq!(
            http_slot(h(HTTP_OUT_STREAM, 0) - 1),
            Some(HTTP_OUT_STREAM - HTTP_STREAM - 1)
        );
        assert_eq!(http_slot(h(HTTP_OUT_STREAM, 0)), None);
        assert_eq!(http_slot(h(HTTP_STREAM, 0) - 1), None);
        // outgoing bodies: the range below the connections
        assert_eq!(http_out_slot(h(HTTP_OUT_STREAM, 0)), Some(0));
        assert_eq!(
            http_out_slot(h(TCP_STREAM, 0) - 1),
            Some(TCP_STREAM - HTTP_OUT_STREAM - 1)
        );
        assert_eq!(http_out_slot(h(TCP_STREAM, 0)), None);
        assert_eq!(http_out_slot(h(HTTP_OUT_STREAM, 0) - 1), None);
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
