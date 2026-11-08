// A WASI preview2 host written against the typed `Host` trait that
// `wasm2rs --wit-host --glue` generates from a component. It has no canonical-ABI
// code: strings, lists, results and resources arrive as Rust values, and the generated
// `Glue` lowers them to the module's flat `Imports`. It is source text appended to the
// generated code (the `Host` trait and its types come from the component), covering
// the interfaces a `println!` program uses: stdout, the environment, the arguments
// (`with_arguments`), exit, output streams and pollables, stderr and the terminal
// queries (not a terminal), which a std program asks for at startup. Handles: 0 = stdin,
// 1 = stdout, 2 = stderr; a pollable is an index into `pollables`: ready at once, at
// the deadline a clock subscription gave it, or when stdin has data or is at its end.
// The clocks: monotonic `now` (nanoseconds since the host was created), `subscribe-duration`,
// `subscribe-instant` (an instant on that same clock) and wall-clock `now`. Stdin is the real
// one: a thread reads it into a buffer, so `read` never blocks (an empty
// list means "nothing yet", `closed` the end of input) and `poll` and `block` wait on the
// buffer's condition variable with a timeout at the earliest deadline. It also serves `wasi:random` and
// `wasi:filesystem`: read-only with `with_preopen`, writable with `with_writable_preopen`.
// Reading: `preopens.get-directories`, `descriptor.open-at`, `read-via-stream`, `stat`,
// `stat-at`, `get-flags`, `read-directory`, `readlink-at`, `metadata-hash` (and `-at`),
// `directory-entry-stream.read-directory-entry` and file `input-stream` `read`, `blocking-read`,
// `skip` and `blocking-skip`. Writing (`read-only` on a read-only
// preopen): `open-at` with create/exclusive/truncate and `write`, `write-via-stream`,
// `append-via-stream`, file `output-stream` `write`/`check-write`/`flush`/`blocking-flush`/
// `blocking-write-and-flush`/`write-zeroes`/`blocking-write-zeroes-and-flush`,
// `create-directory-at`, `remove-directory-at`, `unlink-file-at`, `rename-at`, `symlink-at`,
// `link-at`, `set-size`, `sync`, `sync-data`, `set-times` and `set-times-at`. The semantics:
// one preopened directory shown as "/", descriptors and streams in
// slabs (the preopen is descriptor 0 and never freed; a file's input or output stream is
// `FIRST_FILE_STREAM + slot` in its own slab, clear of the stdio handles), paths resolved
// under the preopen (absolute paths, `..` and symlinks that lead out are `not-permitted`).
// It also serves TCP over loopback (`wasi:sockets` network, instance-network, tcp,
// tcp-create-socket) with these semantics: an address outside
// 127.0.0.0/8 and ::1 is `access-denied` unless `with_external_network` was called, `finish-bind`
// binds and listens, `start-connect` connects at once, `read` never blocks (`blocking-read` and `blocking-skip` wait), `write` never waits (a connection's output stream has a 64 KiB send queue: `check-write` is 0 and its pollable not ready while a peer that does not read keeps it full; a write over `check-write` traps) and a socket's
// pollable is polled every millisecond; a connection's streams are `TCP_STREAM + slot`.
// UDP (`udp`, `udp-create-socket`) works the same way over loopback: `finish-bind` binds a
// non-blocking `std` socket (an address outside loopback is `access-denied`), `stream` (callable on a
// bound socket; a later call replaces the default remote, and the streams of earlier calls then trap) gives the datagram streams as `udp_in` and `udp_out` slots, and
// `receive` and `send` never block (an empty list, or fewer datagrams sent, means "not now").
// A stream made with `Some(remote)` drops datagrams from other senders and sends to `remote` (a
// datagram's own address must then be absent or the same); an unconnected one needs an address in
// each datagram (`invalid-argument` otherwise). A datagram stream's pollable is polled every
// millisecond, an outgoing stream and a socket are always ready, and sending more than `check-send`
// permits (1024) traps. Name lookup (`ip-name-lookup`): `localhost` (both loopback addresses) and
// numeric IPv4 and IPv6 addresses resolve locally; any other name needs `with_external_network`
// (then `std`'s resolver runs at `resolve-addresses`) and is `permanent-resolver-failure` without it;
// a name that is not a host name is `invalid-argument`. The TCP socket options (keep-alive
// enabled, idle time, interval and count, hop limit, receive and send buffer sizes) are only
// recorded, not applied to the OS socket: a socket starts with the defaults, a set value is
// read back, zero is `invalid-argument`, and an accepted socket inherits the listener's values.
// The listen backlog size is only validated (zero is `invalid-argument`, `invalid-state` once a
// connection is started or made, ignored while listening): it is not stored or read back. The UDP ones (hop limit, buffer sizes, `ipv6-only`) answer
// `not-supported`.
//
// A component that imports both filesystem and sockets gets two `error-code` enums, which the
// generator spells `ErrorCode` and `ErrorCode2` in an order that follows the imports. The host
// therefore names them `FsErrorCode` and `NetErrorCode`: type aliases that
// `type_aliases` (`filter.rs`) emits from the generated trait, for the interfaces it declares.
//
// Each method sits between `// optional: <method>` and `// end optional`
// and is kept only when the generated trait declares it (see `keep_declared_methods` in
// `filter.rs`), so one source serves components that import different subsets. A
// component that imports a method this host lacks does not build. The source needs the
// `type_aliases` text before it.

#[allow(unused_imports)]
use std::collections::VecDeque;
#[allow(unused_imports)]
use std::io::Write;
#[allow(unused_imports)]
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, SocketAddrV6, TcpListener,
    TcpStream, ToSocketAddrs, UdpSocket as StdUdpSocket,
};
#[allow(unused_imports)]
use std::path::{Component, Path, PathBuf};
#[allow(unused_imports)]
use std::sync::atomic::{AtomicU64, Ordering};
#[allow(unused_imports)]
use std::sync::{Arc, Condvar, Mutex};
#[allow(unused_imports)]
use std::time::{Duration, Instant};

#[allow(dead_code)]
fn os_seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(0u8)
}

#[allow(dead_code)]
struct TypedHost {
    /// The program's arguments; set by `with_arguments`, read only by the optional
    /// `get-arguments` block.
    args: Vec<String>,
    env: Vec<(String, String)>,
    /// The monotonic clock's zero: when the host was created; read only by the optional
    /// clock blocks.
    start: Instant,
    /// Filled by the stdin thread, which `start_stdin` spawns on first use.
    stdin: SharedStdin,
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
    /// The generator state (splitmix64, seeded from the OS-seeded `std` hasher): fine for
    /// `HashMap` seeds and examples, not for keys or tokens.
    rng: u64,
    /// One entry per subscription: what it waits for. A dropped pollable keeps its slot.
    pollables: Vec<Wait>,
    /// The canonicalized preopened directory (`with_preopen`), shown to the guest as "/".
    root: Option<PathBuf>,
    /// Whether the guest may write under `root`; set by `with_writable_preopen`.
    writable: bool,
    /// Open descriptors; slot 0 is the preopen.
    descriptors: Slab<OpenDescriptor>,
    /// Streams behind the input-stream handles `FIRST_FILE_STREAM..`.
    files: Slab<FileStream>,
    /// Streams behind the output-stream handles `FIRST_FILE_STREAM + slot` (1 and 2 are
    /// stdout and stderr).
    outputs: Slab<FileStream>,
    /// Directory listings behind `directory-entry-stream` handles: the entries (type, name)
    /// still to be read.
    listings: Slab<std::vec::IntoIter<(std::fs::FileType, String)>>,
    // sockets: the fields below are read only by the optional `wasi:sockets` blocks
    /// Whether the guest may use addresses outside loopback; set by `with_external_network`.
    external_network: bool,
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
}

impl Drop for TypedHost {
    fn drop(&mut self) {
        self.flush_all_connections();
    }
}

#[allow(dead_code)]
impl TypedHost {
    fn new(env: Vec<(String, String)>) -> Self {
        TypedHost {
            args: Vec::new(),
            env,
            start: Instant::now(),
            stdin: SharedStdin::default(),
            errors: Slab::new(),
            failed_in: std::collections::HashSet::new(),
            failed_out: std::collections::HashSet::new(),
            stdin_started: false,
            rng: os_seed(),
            pollables: Vec::new(),
            root: None,
            writable: false,
            descriptors: Slab::new(),
            files: Slab::new(),
            outputs: Slab::new(),
            listings: Slab::new(),
            external_network: false,
            sockets: Slab::new(),
            tcp_in: Slab::new(),
            tcp_out: Slab::new(),
            next_tcp_out_id: 0,
            udp_sockets: Slab::new(),
            udp_in: Slab::new(),
            udp_out: Slab::new(),
            resolutions: Slab::new(),
        }
    }

    /// Gives the guest these command-line arguments (none by default).
    fn with_arguments(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Gives the guest the directory `root` (as its "/"), read-only; see
    /// `with_writable_preopen`.
    fn with_preopen(mut self, root: PathBuf) -> Self {
        let root = root.canonicalize().expect("preopen: directory does not exist");
        self.descriptors.insert(OpenDescriptor { path: root.clone(), write: false, read: true, file: None }); // slot 0
        self.root = Some(root);
        self
    }

    /// Like `with_preopen`, but the guest may create, write, append to and truncate
    /// files under `root`.
    fn with_writable_preopen(mut self, root: PathBuf) -> Self {
        self.writable = true;
        self.with_preopen(root)
    }

    /// Like `resolve`, for a file that `open-at` may create: if `path` does not exist, it
    /// is named in its parent directory (see `resolve_entry`). A symlink that leads nowhere
    /// (or out of the preopen) is `not-permitted`, not a place to create a file.
    fn resolve_to_create(&self, dir: u32, path: &str) -> Result<PathBuf, Fs> {
        match self.resolve(dir, path) {
            Err(Fs::NoEntry) => {}
            found => return found,
        }
        let full = self.resolve_entry(dir, path)?;
        if full.symlink_metadata().is_ok() { Err(Fs::NotPermitted) } else { Ok(full) }
    }

    /// The open file behind a descriptor opened for `write`: `read-only` unless the preopen
    /// is writable, `is-directory` for a directory, `bad-descriptor` if it was not opened
    /// for `write`.
    fn writable_descriptor_file(&self, descriptor: u32) -> Result<&std::fs::File, Fs> {
        self.require_writable()?;
        let d = self.open_descriptor(descriptor)?;
        let file = d.file.as_ref().ok_or(Fs::IsDirectory)?;
        if d.write { Ok(file) } else { Err(Fs::BadDescriptor) }
    }

    /// A new output stream over `writable_descriptor_file`, from `pos` (`append`: always
    /// at its end): the handle.
    fn open_output_stream(&mut self, descriptor: u32, pos: u64, append: bool) -> Result<u32, Fs> {
        let stream = FileStream::over(self.writable_descriptor_file(descriptor)?, pos, append)?;
        Ok(FIRST_FILE_STREAM + self.outputs.insert(stream) as u32)
    }

    /// Runs `op` on the open file behind a descriptor, or on a directory's (opened from its
    /// path, resolved again).
    fn with_descriptor_file<T>(
        &self,
        descriptor: u32,
        op: impl FnOnce(&std::fs::File) -> std::io::Result<T>,
    ) -> Result<T, Fs> {
        let done = match &self.open_descriptor(descriptor)?.file {
            Some(file) => op(file),
            None => op(&std::fs::File::open(self.directory(descriptor)?).map_err(io_error_code)?),
        };
        done.map_err(io_error_code)
    }

    /// Flushes the file or directory behind a descriptor to disk (`data_only`: without its
    /// metadata).
    fn sync_descriptor(&self, descriptor: u32, data_only: bool) -> Result<(), Fs> {
        self.with_descriptor_file(descriptor, |file| {
            if data_only { file.sync_data() } else { file.sync_all() }
        })
    }

    /// `Ok` if the preopen is writable, else `read-only`.
    fn require_writable(&self) -> Result<(), Fs> {
        if self.writable { Ok(()) } else { Err(Fs::ReadOnly) }
    }

    /// The descriptor behind a handle, or `bad-descriptor`.
    fn open_descriptor(&self, descriptor: u32) -> Result<&OpenDescriptor, Fs> {
        self.descriptors.get(descriptor as usize).ok_or(Fs::BadDescriptor)
    }

    /// The path behind a directory descriptor, or `bad-descriptor` (`not-directory` for a
    /// file: a file descriptor stays on its file, not on its path). The directories above
    /// it can be moved since it was opened, so the path is resolved again and must still be
    /// under the preopen: `not-permitted` if not.
    fn directory(&self, descriptor: u32) -> Result<PathBuf, Fs> {
        let OpenDescriptor { path, file, .. } = self.open_descriptor(descriptor)?;
        if file.is_some() {
            return Err(Fs::NotDirectory);
        }
        self.confined(path.canonicalize().map_err(io_error_code)?)
    }

    /// `real` if it lies under the preopen, `not-permitted` otherwise.
    fn confined(&self, real: PathBuf) -> Result<PathBuf, Fs> {
        let root = self.root.as_ref().ok_or(Fs::NotPermitted)?;
        if real.starts_with(root) { Ok(real) } else { Err(Fs::NotPermitted) }
    }

    /// The path `path` names under the directory behind descriptor `dir`: `not-permitted`
    /// outside the preopen.
    fn resolve(&self, dir: u32, path: &str) -> Result<PathBuf, Fs> {
        let base = self.directory(dir)?;
        let relative = Path::new(path);
        if !stays_below(relative) {
            return Err(Fs::NotPermitted);
        }
        self.confined(base.join(relative).canonicalize().map_err(io_error_code)?)
    }

    /// The entry `path` names under the directory behind `dir`, without following it if it
    /// is a symlink: its parent directory must resolve under the preopen. A name ending in
    /// `/` or `/.` names a directory, which `file_name` would silently drop: `invalid`.
    fn resolve_entry(&self, dir: u32, path: &str) -> Result<PathBuf, Fs> {
        let base = self.directory(dir)?;
        let relative = Path::new(path);
        if !stays_below(relative) {
            return Err(Fs::NotPermitted);
        }
        if path.ends_with('/') || path.ends_with("/.") {
            return Err(Fs::Invalid);
        }
        let name = relative.file_name().ok_or(Fs::Invalid)?;
        let parent = relative.parent().unwrap_or(Path::new(""));
        let parent = base.join(parent).canonicalize().map_err(io_error_code)?;
        Ok(self.confined(parent)?.join(name))
    }

    /// The target of the symlink at `path`, if it is relative and stays under the preopen.
    fn read_link_target(&self, dir: u32, path: &str) -> Result<String, Fs> {
        let entry = self.resolve_entry(dir, path)?;
        if !entry.symlink_metadata().map_err(io_error_code)?.is_symlink() {
            return Err(Fs::Invalid);
        }
        let target = std::fs::read_link(entry).map_err(io_error_code)?;
        if !stays_below(&target) {
            return Err(Fs::NotPermitted);
        }
        target.into_os_string().into_string().map_err(|_| Fs::Io)
    }

    /// The metadata of a descriptor: of its open file, or of its directory, resolved again.
    fn descriptor_metadata(&self, descriptor: u32) -> Result<std::fs::Metadata, Fs> {
        match &self.open_descriptor(descriptor)?.file {
            Some(file) => file.metadata().map_err(io_error_code),
            None => std::fs::metadata(self.directory(descriptor)?).map_err(io_error_code),
        }
    }

    /// The listing of a directory descriptor: a snapshot of the entries (without `.` and
    /// `..`), in the OS's order.
    fn list_directory(&self, descriptor: u32) -> Result<Vec<(std::fs::FileType, String)>, Fs> {
        let path = self.directory(descriptor)?;
        if !path.is_dir() {
            return Err(Fs::NotDirectory);
        }
        std::fs::read_dir(path)
            .and_then(|dir| {
                dir.map(|entry| {
                    let entry = entry?;
                    Ok((entry.file_type()?, entry.file_name().to_string_lossy().into_owned()))
                })
                .collect()
            })
            .map_err(io_error_code)
    }

    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `len` random bytes. A guest-chosen length above 64 MiB traps (as in wasmtime), so a
    /// guest cannot make the host allocate without bound.
    fn random_bytes(&mut self, len: u64) -> Vec<u8> {
        assert!(len <= 64 << 20, "random bytes: length over 64 MiB");
        std::iter::repeat_with(|| self.next_u64().to_le_bytes())
            .flatten()
            .take(len as usize)
            .collect()
    }

    // optional: wasi_io_streams_method_input_stream_read | wasi_io_streams_method_input_stream_blocking_read | wasi_io_streams_method_input_stream_skip | wasi_io_streams_method_input_stream_blocking_skip | wasi_io_streams_method_output_stream_splice | wasi_io_streams_method_output_stream_blocking_splice | wasi_io_streams_method_input_stream_subscribe
    /// Starts the thread that reads the real stdin into the buffer (once).
    fn start_stdin(&mut self) {
        use std::io::Read;
        if self.stdin_started {
            return;
        }
        self.stdin_started = true;
        let shared = Arc::clone(&self.stdin);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                let (n, error) = match std::io::stdin().read(&mut chunk) {
                    Ok(n) => (n, None),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // the failure ends the input: `read_stream` reports it after the bytes
                    Err(e) => (0, Some(e.to_string())),
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
    // end optional

    // optional: wasi_io_poll_resource_drop_pollable | wasi_io_streams_method_input_stream_blocking_read | wasi_io_streams_method_input_stream_blocking_skip | wasi_io_streams_method_output_stream_blocking_splice | wasi_io_streams_method_output_stream_subscribe
    fn subscribe(&mut self, deadline: Option<Instant>) -> Pollable {
        self.subscribe_wait(deadline.map_or(Wait::Ready, Wait::Deadline))
    }

    fn subscribe_wait(&mut self, wait: Wait) -> Pollable {
        self.pollables.push(wait);
        Pollable((self.pollables.len() - 1) as u32)
    }

    /// What a pollable waits for; an unknown one is ready.
    fn wait_of(&self, pollable: Pollable) -> Wait {
        self.pollables.get(pollable.0 as usize).copied().unwrap_or(Wait::Ready)
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
                Some(d) => changed.wait_timeout(buf, d.saturating_duration_since(now)).unwrap().0,
                None => changed.wait(buf).unwrap(),
            };
        }
    }
    // end optional
}

/// An open descriptor: the preopen and other directories hold their path (resolved again
/// on each use), a file its open `File`.
#[allow(dead_code)]
struct OpenDescriptor {
    path: PathBuf,
    /// Whether a file was opened for `write`.
    write: bool,
    /// Whether a file was opened for reading.
    read: bool,
    file: Option<std::fs::File>,
}

/// A stream over a duplicate of a descriptor's `File`. Duplicates share one file offset,
/// so each stream keeps its own position and seeks to it before every read or write (the
/// host runs one guest call at a time, so nothing interleaves).
#[allow(dead_code)]
struct FileStream {
    file: std::fs::File,
    pos: u64,
    /// Every write goes to the end of the file.
    append: bool,
}

#[allow(dead_code)]
impl FileStream {
    /// A stream over a duplicate of `file`, from `pos`.
    fn over(file: &std::fs::File, pos: u64, append: bool) -> Result<Self, Fs> {
        Ok(FileStream { file: file.try_clone().map_err(io_error_code)?, pos, append })
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
        let at = if self.append { SeekFrom::End(0) } else { SeekFrom::Start(self.pos) };
        self.pos = self.file.seek(at)?;
        self.file.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }
}

/// What an `error` handle stands for: the message, and the filesystem `error-code` when the
/// failure came from a file stream (what `filesystem-error-code` reports).
#[allow(dead_code)]
struct ErrorEntry {
    message: String,
    fs: Option<Fs>,
}

/// A table of live handles: a handle is a slot's index, and the slot of a dropped handle
/// is reused by the next `insert`. A guest must not use a handle after dropping it; the
/// host does not detect that, and the stale handle may then name a newer resource.
#[allow(dead_code)]
struct Slab<T> {
    slots: Vec<Option<T>>,
    free: Vec<usize>,
}

#[allow(dead_code)]
impl<T> Slab<T> {
    fn new() -> Self {
        Slab { slots: Vec::new(), free: Vec::new() }
    }

    fn insert(&mut self, value: T) -> usize {
        match self.free.pop() {
            Some(slot) => {
                self.slots[slot] = Some(value);
                slot
            }
            None => {
                self.slots.push(Some(value));
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

    /// Frees a slot; an unknown or already free one is ignored.
    fn remove(&mut self, slot: usize) {
        if self.slots.get_mut(slot).and_then(Option::take).is_some() {
            self.free.push(slot);
        }
    }
}

/// The handle of the first file's stream, input or output (0 to 2 are stdin, stdout, stderr).
#[allow(dead_code)]
const FIRST_FILE_STREAM: u32 = 3;

/// The slot in `files` (input streams) or `outputs` (output streams) of a stream handle
/// (`None` for stdin, stdout, stderr).
#[allow(dead_code)]
fn file_index(stream: u32) -> Option<usize> {
    stream.checked_sub(FIRST_FILE_STREAM).map(|i| i as usize)
}

/// The `error-code`s this host returns, without the generated type (which a component
/// that imports no filesystem interface does not have); `From<Fs> for FsErrorCode` below.
#[allow(dead_code)]
#[derive(Clone, Copy)]
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

/// A symlink at `link` holding `target`; hosts without symlinks answer `unsupported`.
#[allow(dead_code)]
fn make_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link);
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

#[allow(dead_code)]
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

/// Whether `path` is relative and has no `..`, so it stays under the directory it is
/// joined to.
#[allow(dead_code)]
fn stays_below(path: &Path) -> bool {
    path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

// Sockets: the types and helpers below use `std` only; the generated types come in the
// optional blocks that follow.

/// The bytes the stdin thread has read and not yet handed to the guest.
#[allow(dead_code)]
#[derive(Default)]
struct StdinBuf {
    bytes: VecDeque<u8>,
    eof: bool,
    /// Why the input ended, when a read failed instead of reaching the end; taken when reported.
    error: Option<String>,
}

#[allow(dead_code)]
type SharedStdin = Arc<(Mutex<StdinBuf>, Condvar)>;

/// What a pollable waits for.
#[allow(dead_code)]
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
    TcpAccept(usize),
    /// Ready when the stream in `tcp_in` at the slot has data, is at its end or failed.
    TcpReadable(usize),
    /// Ready when the stream in `tcp_out` at the slot has room in its send queue (after it
    /// was drained as far as the socket takes) or failed.
    TcpWritable(usize),
    /// Ready when the stream in `udp_in` at the slot has a datagram it would return (or failed).
    UdpReadable(usize),
}

#[allow(dead_code)]
impl Wait {
    fn is_ready(self, now: Instant, stdin: &StdinBuf, host: &TypedHost) -> bool {
        match self {
            Wait::Ready => true,
            Wait::Deadline(d) => d <= now,
            Wait::Stdin => !stdin.bytes.is_empty() || stdin.eof,
            Wait::TcpAccept(slot) => host.accept_ready(slot),
            Wait::TcpReadable(slot) => host.readable(slot),
            Wait::TcpWritable(slot) => host.tcp_writable(slot),
            Wait::UdpReadable(slot) => host.udp_readable(slot),
        }
    }

    /// Whether readiness can only be found by asking the socket again.
    fn polls_socket(self) -> bool {
        matches!(
            self,
            Wait::TcpAccept(_) | Wait::TcpReadable(_) | Wait::TcpWritable(_) | Wait::UdpReadable(_)
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
#[allow(dead_code)]
const TCP_STREAM: usize = 1 << 30;

/// The slot in `tcp_in` or `tcp_out` of a stream handle, `None` for any other stream.
#[allow(dead_code)]
fn tcp_slot(stream: u32) -> Option<usize> {
    (stream as usize).checked_sub(TCP_STREAM)
}

/// The `wasi:sockets` `error-code`s this host returns, without the generated type;
/// `From<Net> for NetErrorCode` below.
#[allow(dead_code)]
#[derive(Clone, Copy)]
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

#[allow(dead_code)]
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
#[allow(dead_code)]
enum NetState {
    Unbound,
    BindStarted(SocketAddr),
    /// Bound: `std` has bound and listened, but the guest has not asked to listen yet.
    Bound(TcpListener),
    ListenStarted(TcpListener),
    /// Listening (non-blocking); `pending` is a connection a `subscribe` poll took off the
    /// queue to find out that one had arrived, for `accept` to return.
    Listening { listener: TcpListener, pending: std::cell::RefCell<Option<TcpStream>> },
    /// `start-connect` has connected (or failed): `finish-connect` reports it.
    ConnectStarted(Result<TcpStream, Net>),
    /// Connected: `out` is the slot in `tcp_out` of the send queue of the output stream
    /// opened with the connection, and that queue's `id` (the slot is reused once the
    /// stream is dropped, by another connection's queue).
    Connected { stream: TcpStream, out: (usize, u64) },
    /// A connection attempt failed.
    Closed,
}

/// The TCP socket options a guest can set and read back. They are only recorded: `std::net`
/// has no API for most of them, so none reaches the OS socket. The defaults are the usual
/// ones of a Linux socket.
#[allow(dead_code)]
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

#[allow(dead_code)]
struct NetSocket {
    ipv6: bool,
    state: NetState,
    options: TcpOptions,
}

/// How many bytes a connection's send queue holds at most.
#[allow(dead_code)]
const SEND_QUEUE_CAPACITY: usize = 64 * 1024;

/// The output side of a connection: the non-blocking socket and the host's send queue in front
/// of it. Bytes a guest writes are appended to the queue and moved to the socket as far as it
/// takes them, so a peer that does not read fills the queue (`check-write` reports 0, the
/// pollable is not ready) instead of making `write` wait. A failure of the socket (a dead
/// peer) marks the stream failed and drops the queue; the first operation that finds out answers
/// `last-operation-failed` (with an `error` handle), then the stream answers `closed`. The
/// flushing methods (`flush_waiting`, `flush_until`) wait for the peer to read, so they do not
/// return while a live peer never does. The queue is behind a `RefCell` because a pollable
/// asks for readiness through `&TypedHost`.
#[allow(dead_code)]
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

#[allow(dead_code)]
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
                Ok(0) => return self.fail(&mut queue, "the connection accepts no more bytes".into()),
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

// optional: wasi_io_streams_method_input_stream_read | wasi_io_streams_method_input_stream_blocking_read | wasi_io_streams_method_input_stream_skip | wasi_io_streams_method_input_stream_blocking_skip | wasi_io_streams_method_output_stream_check_write | wasi_io_streams_method_output_stream_write | wasi_io_streams_method_output_stream_blocking_write_and_flush | wasi_io_streams_method_output_stream_write_zeroes | wasi_io_streams_method_output_stream_blocking_write_zeroes_and_flush | wasi_io_streams_method_output_stream_splice | wasi_io_streams_method_output_stream_blocking_splice | wasi_io_streams_method_output_stream_blocking_flush | wasi_io_streams_method_output_stream_flush
#[allow(dead_code)]
impl TypedHost {
    /// `last-operation-failed` for a failure of the last operation on a stream: a fresh
    /// `error` handle (one per call, live until the guest drops it) that carries `message`.
    /// The caller reports it once: the stream is `closed` from then on.
    fn last_operation_failed(&mut self, message: impl std::fmt::Display) -> StreamError {
        self.last_operation_failed_with(message, None)
    }

    /// `last_operation_failed`, remembering the filesystem `error-code` behind the failure.
    fn last_operation_failed_with(
        &mut self,
        message: impl std::fmt::Display,
        fs: Option<Fs>,
    ) -> StreamError {
        let entry = ErrorEntry { message: message.to_string(), fs };
        StreamError::LastOperationFailed(Error(self.errors.insert(entry) as u32))
    }

    /// `last_operation_failed` for an I/O failure of `stream`: only a file stream's carries a
    /// filesystem `error-code`; a connection's is not a filesystem error.
    fn stream_failed(&mut self, stream: u32, e: &std::io::Error) -> StreamError {
        let is_file = tcp_slot(stream).is_none() && file_index(stream).is_some();
        let fs = is_file.then(|| io_error_code(std::io::Error::from(e.kind())));
        self.last_operation_failed_with(e, fs)
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

    /// Why the output stream in `tcp_out` at `slot` cannot be written: `last-operation-failed`
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
// end optional

#[allow(dead_code)]
impl TypedHost {
    // optional: wasi_io_streams_method_input_stream_read | wasi_io_streams_method_input_stream_blocking_read | wasi_io_streams_method_input_stream_skip | wasi_io_streams_method_input_stream_blocking_skip | wasi_io_streams_method_output_stream_splice | wasi_io_streams_method_output_stream_blocking_splice
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
        if let Some(slot) = tcp_slot(stream) {
            // never blocks: an empty list means "nothing yet"
            let conn = self.tcp_in.get_mut(slot).expect("read from an unknown or dropped stream");
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
        let file = self.files.get_mut(index).expect("read from an unknown or dropped stream");
        if buf.is_empty() {
            // a zero length succeeds only before the end of the file: look, consume nothing
            let at_end = file.file.metadata().map_or(true, |meta| file.pos >= meta.len());
            return if at_end { Err(StreamError::Closed) } else { Ok(buf) };
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
    // end optional

    // optional: wasi_io_streams_method_input_stream_blocking_read | wasi_io_streams_method_input_stream_blocking_skip | wasi_io_streams_method_output_stream_blocking_splice
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
            let wait = match tcp_slot(stream) {
                Some(slot) => Wait::TcpReadable(slot),
                None if stream == 0 => Wait::Stdin,
                None => return Ok(data),
            };
            self.wait_any(&[wait]);
            if len == 0 {
                return self.read_stream(stream, 0);
            }
        }
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_write | wasi_io_streams_method_output_stream_blocking_write_and_flush | wasi_io_streams_method_output_stream_write_zeroes | wasi_io_streams_method_output_stream_blocking_write_zeroes_and_flush | wasi_io_streams_method_output_stream_splice | wasi_io_streams_method_output_stream_blocking_splice
    /// Whether an output stream can still be written (not dropped or unknown): `splice`
    /// asks before it reads, so a dead output does not consume the source's bytes.
    fn output_is_open(&self, stream: u32) -> bool {
        if let Some(slot) = tcp_slot(stream) {
            self.tcp_out.get(slot).is_some()
        } else if let Some(index) = file_index(stream) {
            self.outputs.get(index).is_some()
        } else {
            true // stdout and stderr
        }
    }

    /// How many bytes `splice` may read for `stream`: `len`, for a connection at most its
    /// `check-write` permit. With `wait` the queue is emptied first (waiting for the peer).
    fn splice_budget(&mut self, stream: u32, len: u64, wait: bool) -> Result<u64, StreamError> {
        let Some(slot) = tcp_slot(stream) else { return Ok(len) };
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
            let permit = self.tcp_permit(slot).ok_or_else(|| self.tcp_out_error(slot))?;
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
        } else {
            match (stream, file_index(stream)) {
                (_, Some(index)) => match self.outputs.get_mut(index) {
                    Some(file) => file.write_all(contents),
                    None => Err(std::io::ErrorKind::NotFound.into()),
                },
                (2, None) => std::io::stderr().write_all(contents),
                (_, None) => std::io::stdout().write_all(contents),
            }
        };
        written.map_err(|e| match e.kind() {
            // a dropped file output (see above) is gone, not failed
            std::io::ErrorKind::NotFound => StreamError::Closed,
            _ => self.fail_output(stream, &e),
        })
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_flush | wasi_io_streams_method_output_stream_flush | wasi_io_streams_method_output_stream_blocking_write_and_flush | wasi_io_streams_method_output_stream_blocking_write_zeroes_and_flush
    /// Flushes a stream; only stdout and a connection's send queue buffer anything. With `wait`
    /// (`blocking-flush`, and what a blocking write ends with) the queue is emptied; without
    /// it (`flush`) it is drained as far as the socket takes now and the flush stays pending.
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
        if stream != 1 {
            return Ok(()); // a file and (unbuffered in `std`) stderr hold nothing
        }
        std::io::stdout().flush().map_err(|e| self.fail_output(stream, &e))
    }
    // end optional

    /// Lets the guest use addresses outside loopback (the default is 127.0.0.0/8 and ::1).
    fn with_external_network(mut self) -> Self {
        self.external_network = true;
        self
    }

    /// Whether `accept` on the socket in `slot` would not block (or the socket is not
    /// listening, so `accept` answers at once). A connection found here is kept for `accept`.
    fn accept_ready(&self, slot: usize) -> bool {
        let Some(NetSocket { state: NetState::Listening { listener, pending }, .. }) =
            self.sockets.get(slot)
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

    /// Whether the input stream in `tcp_in` at `slot` has data, is at its end or failed.
    fn readable(&self, slot: usize) -> bool {
        let Some(stream) = self.tcp_in.get(slot) else { return true };
        match stream.peek(&mut [0u8; 1]) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::WouldBlock,
        }
    }

    /// What `check-write` of the output stream in `tcp_out` at `slot` permits (see
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

    /// Whether the output stream in `tcp_out` at `slot` is writable (or failed or dropped).
    fn tcp_writable(&self, slot: usize) -> bool {
        self.tcp_out.get(slot).is_none() || self.tcp_permit(slot) != Some(0)
    }

    /// The socket behind a handle.
    fn socket(&mut self, handle: u32) -> &mut NetSocket {
        self.sockets.get_mut(handle as usize).expect("unknown or dropped tcp-socket")
    }

    /// `check_address` for the address of a peer to connect or send to: also not port 0 nor an
    /// unspecified address (`0.0.0.0`, `::`), which is `invalid-argument` whether or not the
    /// network is allowed (so those checks come before the loopback one, after the family one).
    fn check_peer(&self, ipv6: bool, addr: &SocketAddr) -> Result<(), Net> {
        let mapped = matches!(addr, SocketAddr::V6(a) if a.ip().to_ipv4_mapped().is_some());
        if addr.is_ipv6() != ipv6 || mapped || addr.ip().is_multicast() {
            return Err(Net::InvalidArgument);
        }
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
        if self.external_network || addr.ip().is_loopback() {
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
        let state = NetState::Connected { stream, out: (out, id) };
        Ok((TCP_STREAM + self.tcp_in.insert(input), TCP_STREAM + out, state))
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
                socket.state =
                    NetState::Bound(TcpListener::bind(addr).map_err(|e| net_error(&e))?);
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
                socket.state =
                    NetState::Listening { listener, pending: std::cell::RefCell::new(None) };
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
            None => listener.accept().map(|(stream, _)| stream).map_err(|e| net_error(&e))?,
        };
        stream.set_nonblocking(true).map_err(|e| net_error(&e))?;
        let (input, output, state) = self.open_tcp_streams(stream)?;
        let slot = self.sockets.insert(NetSocket { ipv6, state, options });
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

// The next blocks use generated types, so each is kept only if the component imports one of
// the methods that need it.

// optional: wasi_filesystem_types_method_descriptor_open_at | wasi_filesystem_types_method_descriptor_read_via_stream | wasi_filesystem_types_method_descriptor_get_flags | wasi_filesystem_types_method_descriptor_read_directory | wasi_filesystem_types_method_descriptor_stat | wasi_filesystem_types_method_descriptor_stat_at | wasi_filesystem_types_method_descriptor_readlink_at | wasi_filesystem_types_method_descriptor_metadata_hash | wasi_filesystem_types_method_descriptor_metadata_hash_at | wasi_filesystem_types_method_directory_entry_stream_read_directory_entry | wasi_filesystem_types_method_descriptor_create_directory_at | wasi_filesystem_types_method_descriptor_remove_directory_at | wasi_filesystem_types_method_descriptor_unlink_file_at | wasi_filesystem_types_method_descriptor_write_via_stream | wasi_filesystem_types_method_descriptor_append_via_stream | wasi_filesystem_types_method_descriptor_set_size | wasi_filesystem_types_method_descriptor_sync | wasi_filesystem_types_method_descriptor_sync_data | wasi_filesystem_types_method_descriptor_rename_at | wasi_filesystem_types_method_descriptor_symlink_at | wasi_filesystem_types_method_descriptor_link_at | wasi_filesystem_types_method_descriptor_set_times | wasi_filesystem_types_method_descriptor_set_times_at | wasi_filesystem_types_method_descriptor_get_type | wasi_filesystem_types_method_descriptor_read | wasi_filesystem_types_method_descriptor_write | wasi_filesystem_types_method_descriptor_advise | wasi_filesystem_types_method_descriptor_is_same_object | wasi_filesystem_types_filesystem_error_code
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
// end optional

// optional: wasi_filesystem_types_method_descriptor_open_at | wasi_filesystem_types_method_descriptor_stat_at | wasi_filesystem_types_method_descriptor_metadata_hash_at
/// Whether `path` ends in a real name that `symlink-follow` can apply to: not empty, not
/// `.` and without a trailing `/` or `/.` (those name a directory, which is never a link).
fn names_an_entry(path: &str) -> bool {
    !(path.is_empty() || path == "." || path.ends_with('/') || path.ends_with("/."))
}
// end optional

// optional: wasi_filesystem_types_method_descriptor_stat | wasi_filesystem_types_method_descriptor_stat_at | wasi_filesystem_types_method_directory_entry_stream_read_directory_entry | wasi_filesystem_types_method_descriptor_get_type
/// Directory 3, regular file 6, symlink 5 (seen only for a directory entry or a
/// `symlink_metadata` lookup), otherwise unknown.
fn descriptor_type(t: std::fs::FileType) -> DescriptorType {
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
// end optional

// optional: wasi_filesystem_types_method_descriptor_stat | wasi_filesystem_types_method_descriptor_stat_at
fn datetime(time: std::io::Result<std::time::SystemTime>) -> Option<Datetime> {
    let since = time.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(Datetime { seconds: since.as_secs(), nanoseconds: since.subsec_nanos() })
}

/// The status-change time is not reported.
fn descriptor_stat(meta: std::fs::Metadata) -> DescriptorStat {
    #[cfg(unix)]
    let link_count = std::os::unix::fs::MetadataExt::nlink(&meta);
    #[cfg(not(unix))]
    let link_count = 1u64;
    DescriptorStat {
        type_: descriptor_type(meta.file_type()),
        link_count,
        size: meta.len(),
        data_access_timestamp: datetime(meta.accessed()),
        data_modification_timestamp: datetime(meta.modified()),
        status_change_timestamp: None,
    }
}
// end optional

// optional: wasi_clocks_monotonic_clock_subscribe_duration | wasi_clocks_monotonic_clock_subscribe_instant
/// `base` plus `when` nanoseconds. A guest can ask for more than an `Instant` holds (it
/// depends on the platform), and adding that would panic; such a deadline is replaced by a
/// distant one that is representable (a century, or a year if even that is not), so it is
/// never reached in practice; only if not even a year fits is `base` itself returned.
fn deadline_after(base: Instant, when: u64) -> Instant {
    const YEAR: u64 = 365 * 24 * 60 * 60;
    [Duration::from_nanos(when), Duration::from_secs(100 * YEAR), Duration::from_secs(YEAR)]
        .into_iter()
        .find_map(|d| base.checked_add(d))
        .unwrap_or(base)
}
// end optional

// optional: wasi_filesystem_types_method_descriptor_metadata_hash | wasi_filesystem_types_method_descriptor_metadata_hash_at | wasi_filesystem_types_method_descriptor_is_same_object
/// What names a file within one run: its device and inode (its path where there are none).
fn file_identity(meta: &std::fs::Metadata, path: &Path) -> (u64, u64, String) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        (meta.dev(), meta.ino(), String::new())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (0u64, 0u64, path.to_string_lossy().into_owned())
    }
}
// end optional

// optional: wasi_filesystem_types_method_descriptor_metadata_hash | wasi_filesystem_types_method_descriptor_metadata_hash_at
/// A hash of the file's identity (see `file_identity`), so the same file gives the same
/// value within one run and two files never do.
fn metadata_hash(meta: &std::fs::Metadata, path: &Path) -> MetadataHashValue {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let identity = file_identity(meta, path);
    let hash = |salt: u8| {
        let mut hasher = DefaultHasher::new();
        (salt, &identity).hash(&mut hasher);
        hasher.finish()
    };
    MetadataHashValue { lower: hash(0), upper: hash(1) }
}
// end optional

// optional: wasi_filesystem_types_method_descriptor_is_same_object
#[allow(dead_code)]
impl TypedHost {
    /// Whether two descriptors name the same file or directory; one the host cannot read
    /// (unknown, or a directory that was moved out of the preopen) is not the same as anything.
    fn same_object(&self, a: u32, b: u32) -> bool {
        let identity = |descriptor: u32| {
            let path = &self.open_descriptor(descriptor).ok()?.path;
            Some(file_identity(&self.descriptor_metadata(descriptor).ok()?, path))
        };
        matches!((identity(a), identity(b)), (Some(x), Some(y)) if x == y)
    }
}
// end optional

// optional: wasi_filesystem_types_method_descriptor_set_times | wasi_filesystem_types_method_descriptor_set_times_at
/// The times of a `set-times` call. Nanoseconds of a second or more, or a time too far
/// ahead, are `invalid`.
fn file_times(
    access: NewTimestamp,
    modification: NewTimestamp,
) -> Result<std::fs::FileTimes, Fs> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let time = |t: NewTimestamp| match t {
        NewTimestamp::NoChange => Ok(None),
        NewTimestamp::Now => Ok(Some(SystemTime::now())),
        NewTimestamp::Timestamp(Datetime { seconds, nanoseconds }) if nanoseconds < 1_000_000_000 => {
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
// end optional

/// Where a UDP socket is in its life: unbound, `start-bind` called, or bound (a non-blocking
/// `std` socket, shared with the streams made from it; `remote` is the default peer of the
/// last `stream` call).
#[allow(dead_code)]
enum UdpState {
    Unbound,
    BindStarted(SocketAddr),
    Bound { socket: Arc<StdUdpSocket>, remote: Option<SocketAddr> },
}

#[allow(dead_code)]
struct UdpSock {
    ipv6: bool,
    state: UdpState,
    /// How many times `stream` was called; only the streams of the last call work (see `UdpEnd`).
    generation: Arc<AtomicU64>,
}

/// One datagram stream over a bound socket: the peer it was made for (`stream(Some(..))`).
#[allow(dead_code)]
struct UdpEnd {
    ipv6: bool,
    socket: Arc<StdUdpSocket>,
    remote: Option<SocketAddr>,
    /// The socket's `generation` counter, and its value when this stream was made: a stream is
    /// stale once `stream` was called again.
    current: Arc<AtomicU64>,
    generation: u64,
}

#[allow(dead_code)]
impl UdpEnd {
    fn is_stale(&self) -> bool {
        self.current.load(Ordering::SeqCst) != self.generation
    }

    /// Traps (the spec lets a replaced stream trap) if `stream` was called again since.
    fn require_current(&self) {
        assert!(!self.is_stale(), "stale datagram stream: `stream` was called again");
    }
}

/// Whether two addresses name the same endpoint: the IP and the port. (`SocketAddr`'s `==`
/// also compares an IPv6 address's flow info and scope id, which a sender's address may lack.)
#[allow(dead_code)]
fn same_endpoint(a: SocketAddr, b: SocketAddr) -> bool {
    a.ip() == b.ip() && a.port() == b.port()
}

/// Whether a datagram from `from` is for a stream made for `remote` (any sender if none).
#[allow(dead_code)]
fn from_peer(remote: Option<SocketAddr>, from: SocketAddr) -> bool {
    remote.is_none_or(|peer| same_endpoint(peer, from))
}

/// The most datagrams one `send` takes (what `check-send` permits).
#[allow(dead_code)]
const UDP_SEND_BATCH: u64 = 1024;

/// The most bytes in a UDP datagram over IPv4 and over IPv6.
#[allow(dead_code)]
const UDP_MAX_PAYLOAD: usize = 65507;
#[allow(dead_code)]
const UDP_MAX_PAYLOAD_V6: usize = 65527;

/// Whether `name` (without a trailing dot) is a syntactically valid host name: dot-separated
/// labels of 1 to 63 letters, digits, `-` and `_` that do not start or end with `-`. A name of
/// only digits and dots is not (an address that failed to parse is not a name).
#[allow(dead_code)]
fn is_host_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.chars().all(|c| c.is_ascii_digit() || c == '.')
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

#[allow(dead_code)]
impl TypedHost {
    /// The UDP socket behind a handle.
    fn udp_socket(&mut self, handle: u32) -> &mut UdpSock {
        self.udp_sockets.get_mut(handle as usize).expect("unknown or dropped udp-socket")
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
                socket.state = UdpState::Bound { socket: Arc::new(bound), remote: None };
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
            UdpState::Bound { remote: Some(peer), .. } => Ok(*peer),
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
        let UdpState::Bound { socket, remote: default_peer } = &mut sock.state else {
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

    /// Whether the incoming stream at `slot` has a datagram `receive` would return, or failed.
    /// A datagram from another sender than a connected stream's peer is dropped here. A stale
    /// stream is always ready (so a waiting guest wakes up and traps) and takes nothing.
    fn udp_readable(&self, slot: usize) -> bool {
        let Some(end) = self.udp_in.get(slot) else { return true };
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
        let end = self.udp_in.get(slot as usize).expect("unknown or dropped incoming-datagram-stream");
        end.require_current();
        let mut got = Vec::new();
        let mut buf = vec![0u8; 65536];
        while (got.len() as u64) < max {
            match end.socket.recv_from(&mut buf) {
                Ok((n, from)) if from_peer(end.remote, from) => {
                    got.push((buf[..n].to_vec(), from))
                }
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
        let end = self.udp_out.get(slot as usize).expect("unknown or dropped outgoing-datagram-stream");
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
        let end = self.udp_out.get(slot as usize).expect("unknown or dropped outgoing-datagram-stream");
        end.require_current();
        let max = if end.ipv6 { UDP_MAX_PAYLOAD_V6 } else { UDP_MAX_PAYLOAD };
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
                if data.len() > max { Err(Net::DatagramTooLarge) } else { Ok(to) }
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
            return Ok(VecDeque::from([Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()]));
        }
        if !self.external_network {
            return Err(Net::PermanentResolverFailure);
        }
        let mut found = VecDeque::new();
        for addr in (host, 0).to_socket_addrs().map_err(|_| Net::NameUnresolvable)? {
            if !found.contains(&addr.ip()) {
                found.push_back(addr.ip());
            }
        }
        if found.is_empty() { Err(Net::NameUnresolvable) } else { Ok(found) }
    }
}

// optional: wasi_sockets_tcp_create_socket_create_tcp_socket | wasi_sockets_tcp_method_tcp_socket_start_bind | wasi_sockets_tcp_method_tcp_socket_finish_bind | wasi_sockets_tcp_method_tcp_socket_start_listen | wasi_sockets_tcp_method_tcp_socket_finish_listen | wasi_sockets_tcp_method_tcp_socket_start_connect | wasi_sockets_tcp_method_tcp_socket_finish_connect | wasi_sockets_tcp_method_tcp_socket_accept | wasi_sockets_tcp_method_tcp_socket_local_address | wasi_sockets_tcp_method_tcp_socket_remote_address | wasi_sockets_tcp_method_tcp_socket_shutdown | wasi_sockets_udp_method_udp_socket_start_bind | wasi_sockets_udp_method_udp_socket_finish_bind | wasi_sockets_udp_method_udp_socket_stream | wasi_sockets_udp_method_udp_socket_local_address | wasi_sockets_udp_method_udp_socket_remote_address | wasi_sockets_udp_method_incoming_datagram_stream_receive | wasi_sockets_udp_method_outgoing_datagram_stream_send | wasi_sockets_ip_name_lookup_resolve_addresses
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
// end optional

// optional: wasi_sockets_tcp_method_tcp_socket_start_bind | wasi_sockets_tcp_method_tcp_socket_start_connect | wasi_sockets_udp_method_udp_socket_start_bind | wasi_sockets_udp_method_udp_socket_stream | wasi_sockets_udp_method_outgoing_datagram_stream_send
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
// end optional

// optional: wasi_sockets_tcp_method_tcp_socket_local_address | wasi_sockets_tcp_method_tcp_socket_remote_address | wasi_sockets_udp_method_udp_socket_local_address | wasi_sockets_udp_method_udp_socket_remote_address | wasi_sockets_udp_method_incoming_datagram_stream_receive
fn ip_socket_address(addr: SocketAddr) -> IpSocketAddress {
    match addr {
        SocketAddr::V4(a) => {
            let [a0, a1, a2, a3] = a.ip().octets();
            IpSocketAddress::Ipv4(Ipv4SocketAddress { port: a.port(), address: (a0, a1, a2, a3) })
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
// end optional

// optional: wasi_sockets_ip_name_lookup_method_resolve_address_stream_resolve_next_address
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
// end optional

impl Host for TypedHost {
    // optional: wasi_cli_stdout_get_stdout
    fn wasi_cli_stdout_get_stdout(&mut self) -> OutputStream {
        OutputStream(1)
    }
    // end optional

    // optional: wasi_cli_environment_get_arguments
    fn wasi_cli_environment_get_arguments(&mut self) -> Vec<String> {
        self.args.clone()
    }
    // end optional

    // optional: wasi_cli_environment_get_environment
    fn wasi_cli_environment_get_environment(&mut self) -> Vec<(String, String)> {
        self.env.clone()
    }
    // end optional

    // optional: wasi_cli_exit_exit
    /// `exit(status: result)`: `ok` is 0, `error` is 1. The send queues of the connections are
    /// flushed first (see `flush_all_connections`).
    fn wasi_cli_exit_exit(&mut self, status: Result<(), ()>) {
        std::io::stdout().flush().ok();
        self.flush_all_connections();
        std::process::exit(i32::from(status.is_err()));
    }
    // end optional

    // optional: wasi_io_error_method_error_to_debug_string
    /// The message of the failure behind an `error` handle (an unknown one is empty).
    fn wasi_io_error_method_error_to_debug_string(&mut self, self_: Error) -> String {
        self.errors.get(self_.0 as usize).map(|e| e.message.clone()).unwrap_or_default()
    }
    // end optional

    // optional: wasi_filesystem_types_filesystem_error_code
    /// The filesystem `error-code` behind an `error` handle: only a failure of a file stream
    /// has one (`None` for a connection's or stdout's, and for a handle the host does not hold).
    fn wasi_filesystem_types_filesystem_error_code(&mut self, err: Error) -> Option<FsErrorCode> {
        self.errors.get(err.0 as usize)?.fs.map(FsErrorCode::from)
    }
    // end optional

    // optional: wasi_io_error_resource_drop_error
    fn wasi_io_error_resource_drop_error(&mut self, handle: Error) {
        self.errors.remove(handle.0 as usize);
    }
    // end optional

    // optional: wasi_io_poll_resource_drop_pollable
    fn wasi_io_poll_resource_drop_pollable(&mut self, _handle: Pollable) {}
    // end optional

    // optional: wasi_io_streams_resource_drop_input_stream
    fn wasi_io_streams_resource_drop_input_stream(&mut self, handle: InputStream) {
        self.failed_in.remove(&handle.0);
        if let Some(slot) = tcp_slot(handle.0) {
            self.tcp_in.remove(slot);
        } else if let Some(index) = file_index(handle.0) {
            self.files.remove(index);
        }
    }
    // end optional

    // optional: wasi_cli_terminal_input_resource_drop_terminal_input
    fn wasi_cli_terminal_input_resource_drop_terminal_input(&mut self, _handle: TerminalInput) {}
    // end optional

    // optional: wasi_cli_terminal_output_resource_drop_terminal_output
    fn wasi_cli_terminal_output_resource_drop_terminal_output(&mut self, _handle: TerminalOutput) {}
    // end optional

    // optional: wasi_cli_stdin_get_stdin
    fn wasi_cli_stdin_get_stdin(&mut self) -> InputStream {
        InputStream(0)
    }
    // end optional

    // optional: wasi_cli_stderr_get_stderr
    fn wasi_cli_stderr_get_stderr(&mut self) -> OutputStream {
        OutputStream(2)
    }
    // end optional

    // optional: wasi_cli_terminal_stdin_get_terminal_stdin
    fn wasi_cli_terminal_stdin_get_terminal_stdin(&mut self) -> Option<TerminalInput> {
        None
    }
    // end optional

    // optional: wasi_cli_terminal_stdout_get_terminal_stdout
    fn wasi_cli_terminal_stdout_get_terminal_stdout(&mut self) -> Option<TerminalOutput> {
        None
    }
    // end optional

    // optional: wasi_cli_terminal_stderr_get_terminal_stderr
    fn wasi_cli_terminal_stderr_get_terminal_stderr(&mut self) -> Option<TerminalOutput> {
        None
    }
    // end optional

    // optional: wasi_clocks_monotonic_clock_subscribe_duration
    fn wasi_clocks_monotonic_clock_subscribe_duration(&mut self, when: u64) -> Pollable {
        self.subscribe(Some(deadline_after(Instant::now(), when)))
    }
    // end optional

    // optional: wasi_clocks_monotonic_clock_subscribe_instant
    /// An instant is nanoseconds since the host was created (see `monotonic-clock.now`); one
    /// that has passed is ready at once.
    fn wasi_clocks_monotonic_clock_subscribe_instant(&mut self, when: u64) -> Pollable {
        self.subscribe(Some(deadline_after(self.start, when)))
    }
    // end optional

    // optional: wasi_clocks_monotonic_clock_now
    /// Nanoseconds since the host was created.
    fn wasi_clocks_monotonic_clock_now(&mut self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
    // end optional

    // optional: wasi_clocks_wall_clock_now
    fn wasi_clocks_wall_clock_now(&mut self) -> Datetime {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the system clock is before the Unix epoch");
        Datetime { seconds: since.as_secs(), nanoseconds: since.subsec_nanos() }
    }
    // end optional

    // optional: wasi_io_poll_poll
    /// The indices of the ready pollables, waiting for the earliest deadline, for data
    /// on stdin or for a socket if none is ready yet. An empty list traps, as the interface specifies.
    fn wasi_io_poll_poll(&mut self, in_: Vec<Pollable>) -> Vec<u32> {
        assert!(!in_.is_empty(), "wasi:io/poll.poll: empty list");
        self.wait_ready(&in_)
    }
    // end optional

    // optional: wasi_io_streams_resource_drop_output_stream
    fn wasi_io_streams_resource_drop_output_stream(&mut self, handle: OutputStream) {
        self.failed_out.remove(&handle.0);
        if let Some(slot) = tcp_slot(handle.0) {
            // what the guest wrote still reaches the peer (see `TcpOut`)
            if let Some(conn) = self.tcp_out.get(slot) {
                conn.flush_waiting();
            }
            self.tcp_out.remove(slot);
        } else if let Some(index) = file_index(handle.0) {
            self.outputs.remove(index);
        }
    }
    // end optional

    // optional: wasi_io_poll_method_pollable_block
    fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable) {
        self.wait_ready(&[self_]);
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_check_write
    /// 4096, except for a connection: its send queue is drained first and the permit is what
    /// still fits it (up to 4096), so 0 while the peer does not read.
    fn wasi_io_streams_method_output_stream_check_write(
        &mut self,
        self_: OutputStream,
    ) -> Result<u64, StreamError> {
        match tcp_slot(self_.0) {
            Some(slot) => self.tcp_permit(slot).ok_or_else(|| self.tcp_out_error(slot)),
            None => Ok(4096),
        }
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_write
    /// Writes without flushing. `contents` over 4096 bytes (what `check-write` returns) traps.
    fn wasi_io_streams_method_output_stream_write(
        &mut self,
        self_: OutputStream,
        contents: Vec<u8>,
    ) -> Result<(), StreamError> {
        assert!(contents.len() <= 4096, "write: more than check-write allows");
        self.assert_within_permit(self_.0, contents.len() as u64, "write")?;
        self.write_stream(self_.0, &contents, false)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_flush
    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        self_: OutputStream,
    ) -> Result<(), StreamError> {
        self.flush_stream(self_.0, true)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_flush
    /// Drains a connection's send queue as far as the socket takes now, without waiting
    /// (`blocking-flush` waits until the queue is empty); the other streams hold nothing.
    fn wasi_io_streams_method_output_stream_flush(
        &mut self,
        self_: OutputStream,
    ) -> Result<(), StreamError> {
        self.flush_stream(self_.0, false)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_write_and_flush
    /// Writes then flushes. `contents` over 4096 bytes (what `check-write` returns) traps.
    /// A connection's send queue is emptied first and then the bytes are sent, waiting for the
    /// peer to read, so this does not return while it never does.
    fn wasi_io_streams_method_output_stream_blocking_write_and_flush(
        &mut self,
        self_: OutputStream,
        contents: Vec<u8>,
    ) -> Result<(), StreamError> {
        assert!(contents.len() <= 4096, "blocking-write-and-flush: more than check-write allows");
        self.write_stream(self_.0, &contents, true)?;
        self.flush_stream(self_.0, true)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_write_zeroes
    /// Writes `len` zero bytes; a `len` over 4096 or over what `check-write` permits a
    /// connection (see `write`) traps.
    fn wasi_io_streams_method_output_stream_write_zeroes(
        &mut self,
        self_: OutputStream,
        len: u64,
    ) -> Result<(), StreamError> {
        assert!(len <= 4096, "write-zeroes: more than check-write allows");
        self.assert_within_permit(self_.0, len, "write-zeroes")?;
        self.write_stream(self_.0, &vec![0u8; len as usize], false)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_write_zeroes_and_flush
    /// `write-zeroes` then flush; a `len` over 4096 traps. Waits on a connection like
    /// `blocking-write-and-flush`.
    fn wasi_io_streams_method_output_stream_blocking_write_zeroes_and_flush(
        &mut self,
        self_: OutputStream,
        len: u64,
    ) -> Result<(), StreamError> {
        assert!(len <= 4096, "blocking-write-zeroes-and-flush: more than check-write allows");
        self.write_stream(self_.0, &vec![0u8; len as usize], true)?;
        self.flush_stream(self_.0, true)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_splice
    /// Moves at most `min(len, 4096)` bytes from `src` to this stream and returns how many:
    /// a dropped output is `closed` first (before anything is read), then `read`, then
    /// `write` (a zero `len` is ok while both are open). It does not wait for `src`: `src` at
    /// its end is `closed`, nothing available yet is `ok(0)`. For a connection it reads at
    /// most what `check-write` permits, so with the send queue full (permit 0) it returns
    /// `ok(0)` without touching `src`. A failed write is `last-operation-failed` once, then the
    /// output stream is `closed`. Known
    /// limitation: the bytes already read from `src` are lost then.
    fn wasi_io_streams_method_output_stream_splice(
        &mut self,
        self_: OutputStream,
        src: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        if !self.output_is_open(self_.0) {
            return Err(StreamError::Closed);
        }
        let budget = self.splice_budget(self_.0, len, false)?;
        if budget == 0 && len > 0 {
            return Ok(0);
        }
        let data = self.read_stream(src.0, budget)?;
        if !data.is_empty() {
            self.write_stream(self_.0, &data, false)?;
        }
        Ok(data.len() as u64)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_splice
    /// Like `splice`, but waits while an open `src` (stdin or a connection) has nothing yet,
    /// so at least one byte is moved for a `len` over 0. For a connection it first waits for its
    /// send queue to empty (the peer to read), then reads at most what `check-write` permits.
    fn wasi_io_streams_method_output_stream_blocking_splice(
        &mut self,
        self_: OutputStream,
        src: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        if !self.output_is_open(self_.0) {
            return Err(StreamError::Closed);
        }
        let budget = self.splice_budget(self_.0, len, true)?;
        let data = self.read_stream_waiting(src.0, budget)?;
        if !data.is_empty() {
            self.write_stream(self_.0, &data, false)?;
        }
        Ok(data.len() as u64)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_subscribe
    /// Ready at once, except for a connection: it is ready while its send queue has room.
    fn wasi_io_streams_method_output_stream_subscribe(&mut self, self_: OutputStream) -> Pollable {
        match tcp_slot(self_.0) {
            Some(slot) => self.subscribe_wait(Wait::TcpWritable(slot)),
            None => self.subscribe(None),
        }
    }
    // end optional

    // optional: wasi_random_random_get_random_bytes
    fn wasi_random_random_get_random_bytes(&mut self, len: u64) -> Vec<u8> {
        self.random_bytes(len)
    }
    // end optional

    // optional: wasi_random_random_get_random_u64
    fn wasi_random_random_get_random_u64(&mut self) -> u64 {
        self.next_u64()
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_bytes
    fn wasi_random_insecure_get_insecure_random_bytes(&mut self, len: u64) -> Vec<u8> {
        self.random_bytes(len)
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_u64
    fn wasi_random_insecure_get_insecure_random_u64(&mut self) -> u64 {
        self.next_u64()
    }
    // end optional

    // optional: wasi_random_insecure_seed_insecure_seed
    fn wasi_random_insecure_seed_insecure_seed(&mut self) -> (u64, u64) {
        (self.next_u64(), self.next_u64())
    }
    // end optional

    // optional: wasi_filesystem_types_resource_drop_descriptor
    fn wasi_filesystem_types_resource_drop_descriptor(&mut self, handle: Descriptor) {
        if handle.0 != 0 {
            // the preopen (slot 0) is never freed
            self.descriptors.remove(handle.0 as usize);
        }
    }
    // end optional

    // optional: wasi_filesystem_types_resource_drop_directory_entry_stream
    fn wasi_filesystem_types_resource_drop_directory_entry_stream(
        &mut self,
        handle: DirectoryEntryStream,
    ) {
        self.listings.remove(handle.0 as usize);
    }
    // end optional

    // optional: wasi_filesystem_preopens_get_directories
    /// The preopen, named "/", if there is one.
    fn wasi_filesystem_preopens_get_directories(&mut self) -> Vec<(Descriptor, String)> {
        self.root.iter().map(|_| (Descriptor(0), "/".to_string())).collect()
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_open_at
    /// Without `symlink-follow` a symlink as the last component is `loop` (like `O_NOFOLLOW`),
    /// also when creating; `create` + `exclusive` on a symlink name is `exist` whatever the
    /// flags (it never follows, dangling or not). Links in the rest of the path, and a path
    /// naming a directory itself (`names_an_entry`), are resolved as ever, always within the preopen. Creating,
    /// truncating and writing are `read-only` unless the preopen is writable; then `create`,
    /// `exclusive` and `truncate` act at once, and the descriptor remembers whether it was
    /// opened for `write`.
    fn wasi_filesystem_types_method_descriptor_open_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
        open_flags: OpenFlags,
        flags: DescriptorFlags,
    ) -> Result<Descriptor, FsErrorCode> {
        let create = open_flags.contains(OpenFlags::CREATE);
        let exclusive = open_flags.contains(OpenFlags::EXCLUSIVE);
        let exclusive_create = create && exclusive;
        let truncate = open_flags.contains(OpenFlags::TRUNCATE);
        let writes = flags.contains(DescriptorFlags::WRITE);
        let changes = create || exclusive || truncate;
        if !self.writable && (changes || writes || flags.contains(DescriptorFlags::MUTATE_DIRECTORY)) {
            return Err(Fs::ReadOnly.into());
        }
        let wants_directory = open_flags.contains(OpenFlags::DIRECTORY);
        if wants_directory && (changes || writes) {
            return Err(Fs::Invalid.into());
        }
        let follows = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        if (exclusive_create || !follows) && names_an_entry(&path) {
            let entry = self.resolve_entry(self_.0, &path)?;
            if entry.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) {
                // `create` + `exclusive` never follows: the name exists (before `loop`),
                // dangling or not
                return Err(if exclusive_create { Fs::Exist } else { Fs::Loop }.into());
            }
        }
        let full = if create {
            self.resolve_to_create(self_.0, &path)?
        } else {
            self.resolve(self_.0, &path)?
        };
        let is_dir = full.is_dir();
        if wants_directory && !is_dir {
            return Err(Fs::NotDirectory.into());
        }
        if is_dir && writes {
            return Err(Fs::IsDirectory.into());
        }
        if exclusive_create && full.exists() {
            return Err(Fs::Exist.into());
        }
        if create || truncate {
            std::fs::OpenOptions::new()
                .write(true)
                .create(create)
                .truncate(truncate)
                .open(&full)
                .map_err(io_error_code)?;
        }
        // a file that only writes need not be readable (and one with no flags reads)
        let reads = flags.contains(DescriptorFlags::READ) || !writes;
        let file = if is_dir {
            None
        } else {
            let opened = std::fs::OpenOptions::new().read(reads).write(writes).open(&full);
            Some(opened.map_err(io_error_code)?)
        };
        let slot = self.descriptors.insert(OpenDescriptor { path: full, write: writes, read: reads, file });
        Ok(Descriptor(slot as u32))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_via_stream
    /// A new stream over the file, starting at `offset`: `is-directory` for a directory,
    /// `bad-descriptor` if the descriptor was not opened for reading.
    fn wasi_filesystem_types_method_descriptor_read_via_stream(
        &mut self,
        self_: Descriptor,
        offset: u64,
    ) -> Result<InputStream, FsErrorCode> {
        let d = self.open_descriptor(self_.0)?;
        let file = d.file.as_ref().ok_or(Fs::IsDirectory)?;
        if !d.read {
            return Err(Fs::BadDescriptor.into());
        }
        let stream = FileStream::over(file, offset, false)?;
        let slot = self.files.insert(stream);
        Ok(InputStream(FIRST_FILE_STREAM + slot as u32))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_write_via_stream
    /// A new stream writing into the file from `offset`: `read-only` unless the preopen is
    /// writable; the descriptor must have been opened for `write`.
    fn wasi_filesystem_types_method_descriptor_write_via_stream(
        &mut self,
        self_: Descriptor,
        offset: u64,
    ) -> Result<OutputStream, FsErrorCode> {
        Ok(OutputStream(self.open_output_stream(self_.0, offset, false)?))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_append_via_stream
    /// Like `write-via-stream`, but every write goes to the end of the file.
    fn wasi_filesystem_types_method_descriptor_append_via_stream(
        &mut self,
        self_: Descriptor,
    ) -> Result<OutputStream, FsErrorCode> {
        Ok(OutputStream(self.open_output_stream(self_.0, 0, true)?))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_rename_at
    /// Both names are checked like any other entry; an existing target file is replaced. A
    /// symlink cannot be renamed (`not-permitted`).
    fn wasi_filesystem_types_method_descriptor_rename_at(
        &mut self,
        self_: Descriptor,
        old_path: String,
        new_descriptor: Descriptor,
        new_path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        let old = self.resolve_entry(self_.0, &old_path)?;
        let new = self.resolve_entry(new_descriptor.0, &new_path)?;
        // Not a symlink: moving one over the path of an open descriptor would lead it out of
        // the preopen. (A directory that holds a symlink can still be moved there;
        // `directory` resolves the path again on every use.)
        if old.symlink_metadata().map_err(io_error_code)?.file_type().is_symlink() {
            return Err(Fs::NotPermitted.into());
        }
        Ok(std::fs::rename(old, new).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_symlink_at
    /// A symlink named `new-path` (under this directory) holding `old-path`. The target must
    /// be relative and have no `..` (`not-permitted`; empty is `no-entry`), so every link
    /// made can be read back with `readlink-at`, and a taken name is `exist`. Needs a
    /// writable preopen.
    fn wasi_filesystem_types_method_descriptor_symlink_at(
        &mut self,
        self_: Descriptor,
        old_path: String,
        new_path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        if old_path.is_empty() {
            return Err(Fs::NoEntry.into());
        }
        if !stays_below(Path::new(&old_path)) {
            return Err(Fs::NotPermitted.into());
        }
        let link = self.resolve_entry(self_.0, &new_path)?;
        Ok(make_symlink(&old_path, &link).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_link_at
    /// A hard link. With `symlink-follow` a symlink `old-path` is followed; without it a
    /// symlink is `unsupported` (hosts differ on what to link) and a directory is
    /// `not-permitted`. Both names are checked like any other entry; a taken new name is
    /// `exist`. Needs a writable preopen.
    fn wasi_filesystem_types_method_descriptor_link_at(
        &mut self,
        self_: Descriptor,
        old_path_flags: PathFlags,
        old_path: String,
        new_descriptor: Descriptor,
        new_path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        let old = if old_path_flags.contains(PathFlags::SYMLINK_FOLLOW) {
            self.resolve(self_.0, &old_path)?
        } else {
            let entry = self.resolve_entry(self_.0, &old_path)?;
            if entry.symlink_metadata().map_err(io_error_code)?.is_symlink() {
                return Err(Fs::Unsupported.into());
            }
            entry
        };
        if old.metadata().map_err(io_error_code)?.is_dir() {
            return Err(Fs::NotPermitted.into());
        }
        let new = self.resolve_entry(new_descriptor.0, &new_path)?;
        Ok(std::fs::hard_link(old, new).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_set_times
    /// Needs a writable preopen; a file must have been opened for `write` (a directory needs
    /// nothing more).
    fn wasi_filesystem_types_method_descriptor_set_times(
        &mut self,
        self_: Descriptor,
        data_access_timestamp: NewTimestamp,
        data_modification_timestamp: NewTimestamp,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        let d = self.open_descriptor(self_.0)?;
        if d.file.is_some() && !d.write {
            return Err(Fs::BadDescriptor.into());
        }
        let times = file_times(data_access_timestamp, data_modification_timestamp)?;
        Ok(self.with_descriptor_file(self_.0, |file| file.set_times(times))?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_set_times_at
    /// `set-times` for the entry a path names. With `symlink-follow` a symlink is followed
    /// to its target; without it the entry itself is set, but `std` cannot set the times of
    /// a symlink, so that is `unsupported`. Needs a writable preopen.
    fn wasi_filesystem_types_method_descriptor_set_times_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
        data_access_timestamp: NewTimestamp,
        data_modification_timestamp: NewTimestamp,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        let times = file_times(data_access_timestamp, data_modification_timestamp)?;
        let full = if path_flags.contains(PathFlags::SYMLINK_FOLLOW) {
            self.resolve(self_.0, &path)?
        } else {
            let entry = self.resolve_entry(self_.0, &path)?;
            if entry.symlink_metadata().map_err(io_error_code)?.is_symlink() {
                return Err(Fs::Unsupported.into());
            }
            entry
        };
        let file = std::fs::File::open(full).map_err(io_error_code)?;
        Ok(file.set_times(times).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_set_size
    /// Truncates or zero-extends the file. Like writing, it needs a writable preopen and a
    /// descriptor opened for `write`.
    fn wasi_filesystem_types_method_descriptor_set_size(
        &mut self,
        self_: Descriptor,
        size: u64,
    ) -> Result<(), FsErrorCode> {
        let file = self.writable_descriptor_file(self_.0)?;
        Ok(file.set_len(size).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_sync
    /// Flushes the file or directory to disk. It changes nothing, so a read-only preopen
    /// allows it.
    fn wasi_filesystem_types_method_descriptor_sync(
        &mut self,
        self_: Descriptor,
    ) -> Result<(), FsErrorCode> {
        Ok(self.sync_descriptor(self_.0, false)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_sync_data
    /// Like `sync`, without the metadata.
    fn wasi_filesystem_types_method_descriptor_sync_data(
        &mut self,
        self_: Descriptor,
    ) -> Result<(), FsErrorCode> {
        Ok(self.sync_descriptor(self_.0, true)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_create_directory_at
    /// `exist` if it is there. This and the three below (and the other methods that change
    /// the preopen) are `read-only` unless it is writable, and never follow a symlink to
    /// its target.
    fn wasi_filesystem_types_method_descriptor_create_directory_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        let entry = self.resolve_entry(self_.0, &path)?;
        Ok(std::fs::create_dir(entry).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_unlink_file_at
    /// Removes a file or a symlink (the link, not its target); a directory is
    /// `is-directory`.
    fn wasi_filesystem_types_method_descriptor_unlink_file_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        let entry = self.resolve_entry(self_.0, &path)?;
        if entry.symlink_metadata().map_err(io_error_code)?.is_dir() {
            return Err(Fs::IsDirectory.into());
        }
        Ok(std::fs::remove_file(entry).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_remove_directory_at
    /// An empty directory only (`not-empty`); anything else, a symlink to a directory
    /// included, is `not-directory`.
    fn wasi_filesystem_types_method_descriptor_remove_directory_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable()?;
        let entry = self.resolve_entry(self_.0, &path)?;
        if !entry.symlink_metadata().map_err(io_error_code)?.is_dir() {
            return Err(Fs::NotDirectory.into());
        }
        Ok(std::fs::remove_dir(entry).map_err(io_error_code)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_readlink_at
    /// The target of a symlink, if it is relative and stays under the preopen.
    fn wasi_filesystem_types_method_descriptor_readlink_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<String, FsErrorCode> {
        Ok(self.read_link_target(self_.0, &path)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_get_type
    /// `directory` for a directory and `regular-file` for a file; a descriptor stays on the
    /// file it opened, so never `symbolic-link`.
    fn wasi_filesystem_types_method_descriptor_get_type(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorType, FsErrorCode> {
        Ok(descriptor_type(self.descriptor_metadata(self_.0)?.file_type()))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read
    /// Up to `length` bytes of the file from `offset`, without a stream or a position, and
    /// whether the end of the file was reached. Like `read-via-stream`: `is-directory` for a
    /// directory, `bad-descriptor` if it was not opened for reading. At most 1 MiB is
    /// returned per call (the guest asks again), so a huge `length` allocates nothing extra.
    fn wasi_filesystem_types_method_descriptor_read(
        &mut self,
        self_: Descriptor,
        length: u64,
        offset: u64,
    ) -> Result<(Vec<u8>, bool), FsErrorCode> {
        const LIMIT: u64 = 1 << 20;
        let d = self.open_descriptor(self_.0)?;
        let file = d.file.as_ref().ok_or(Fs::IsDirectory)?;
        if !d.read {
            return Err(Fs::BadDescriptor.into());
        }
        let mut stream = FileStream::over(file, offset, false)?;
        let wanted = length.min(LIMIT) as usize;
        let mut data = Vec::new();
        let mut chunk = [0u8; 8192];
        while data.len() < wanted {
            let room = (wanted - data.len()).min(chunk.len());
            match stream.read(&mut chunk[..room]).map_err(io_error_code)? {
                0 => return Ok((data, true)),
                n => data.extend_from_slice(&chunk[..n]),
            }
        }
        Ok((data, false))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_write
    /// Writes `buffer` at `offset` without a stream or a position; past the end of the file
    /// the gap is zeros. Like `write-via-stream`: `read-only` unless the preopen is writable,
    /// and the descriptor must have been opened for `write`.
    fn wasi_filesystem_types_method_descriptor_write(
        &mut self,
        self_: Descriptor,
        buffer: Vec<u8>,
        offset: u64,
    ) -> Result<u64, FsErrorCode> {
        let file = self.writable_descriptor_file(self_.0)?;
        FileStream::over(file, offset, false)?.write_all(&buffer).map_err(io_error_code)?;
        Ok(buffer.len() as u64)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_advise
    /// Only a hint, so nothing is done with it (the range and the advice are not checked);
    /// like `read`, it needs a file: `is-directory` for a directory.
    fn wasi_filesystem_types_method_descriptor_advise(
        &mut self,
        self_: Descriptor,
        _offset: u64,
        _length: u64,
        _advice: Advice,
    ) -> Result<(), FsErrorCode> {
        self.open_descriptor(self_.0)?.file.as_ref().ok_or(Fs::IsDirectory)?;
        Ok(())
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_is_same_object
    /// Whether both descriptors name the same file (or directory).
    fn wasi_filesystem_types_method_descriptor_is_same_object(
        &mut self,
        self_: Descriptor,
        other: Descriptor,
    ) -> bool {
        self.same_object(self_.0, other.0)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_get_flags
    /// `read` and `write` as the descriptor was opened (the preopen and directories read).
    fn wasi_filesystem_types_method_descriptor_get_flags(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorFlags, FsErrorCode> {
        let d = self.open_descriptor(self_.0)?;
        let read = if d.read { DescriptorFlags::READ.0 } else { 0 };
        let write = if d.write { DescriptorFlags::WRITE.0 } else { 0 };
        Ok(DescriptorFlags(read | write))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_directory
    fn wasi_filesystem_types_method_descriptor_read_directory(
        &mut self,
        self_: Descriptor,
    ) -> Result<DirectoryEntryStream, FsErrorCode> {
        let entries = self.list_directory(self_.0)?;
        Ok(DirectoryEntryStream(self.listings.insert(entries.into_iter()) as u32))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_stat
    fn wasi_filesystem_types_method_descriptor_stat(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorStat, FsErrorCode> {
        Ok(descriptor_stat(self.descriptor_metadata(self_.0)?))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_stat_at
    /// Like `stat`, for a path under the descriptor. With `symlink-follow` a symlink is
    /// followed to its target; without it the link itself is described (type
    /// `symbolic-link`), and a link that leads out of the preopen is not an error.
    fn wasi_filesystem_types_method_descriptor_stat_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
    ) -> Result<DescriptorStat, FsErrorCode> {
        let meta = if path_flags.contains(PathFlags::SYMLINK_FOLLOW) || !names_an_entry(&path) {
            std::fs::metadata(self.resolve(self_.0, &path)?)
        } else {
            std::fs::symlink_metadata(self.resolve_entry(self_.0, &path)?)
        };
        Ok(descriptor_stat(meta.map_err(io_error_code)?))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_metadata_hash
    fn wasi_filesystem_types_method_descriptor_metadata_hash(
        &mut self,
        self_: Descriptor,
    ) -> Result<MetadataHashValue, FsErrorCode> {
        let meta = self.descriptor_metadata(self_.0)?;
        Ok(metadata_hash(&meta, &self.open_descriptor(self_.0)?.path))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_metadata_hash_at
    /// `metadata-hash` for a path under the descriptor. With `symlink-follow` a symlink is
    /// followed to its target (and hashes like it); without it the link's own identity is
    /// hashed. `.`, the empty path and a trailing `/` or `/.` name the directory itself,
    /// which is always resolved.
    fn wasi_filesystem_types_method_descriptor_metadata_hash_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
    ) -> Result<MetadataHashValue, FsErrorCode> {
        let (full, meta) = if path_flags.contains(PathFlags::SYMLINK_FOLLOW) || !names_an_entry(&path) {
            let full = self.resolve(self_.0, &path)?;
            let meta = std::fs::metadata(&full);
            (full, meta)
        } else {
            let full = self.resolve_entry(self_.0, &path)?;
            let meta = std::fs::symlink_metadata(&full);
            (full, meta)
        };
        Ok(metadata_hash(&meta.map_err(io_error_code)?, &full))
    }
    // end optional

    // optional: wasi_filesystem_types_method_directory_entry_stream_read_directory_entry
    /// The next entry, `None` once the listing is exhausted.
    fn wasi_filesystem_types_method_directory_entry_stream_read_directory_entry(
        &mut self,
        self_: DirectoryEntryStream,
    ) -> Result<Option<DirectoryEntry>, FsErrorCode> {
        let listing = self.listings.get_mut(self_.0 as usize).ok_or(Fs::BadDescriptor)?;
        Ok(listing.next().map(|(kind, name)| DirectoryEntry { type_: descriptor_type(kind), name }))
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_read
    /// Reads at most 4096 bytes of a file, connection or stdin stream at once (a
    /// connection's stream or stdin gives an empty list while nothing has arrived);
    /// `closed` at its end. An unknown or dropped stream traps.
    fn wasi_io_streams_method_input_stream_read(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<Vec<u8>, StreamError> {
        self.read_stream(self_.0, len)
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_blocking_read
    /// Like `read`, but waits while stdin or a connection has nothing yet, so the list is
    /// never empty for a `len` over 0; `closed` at the end (for a `len` of 0 too).
    fn wasi_io_streams_method_input_stream_blocking_read(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<Vec<u8>, StreamError> {
        self.read_stream_waiting(self_.0, len)
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_skip
    /// Reads and discards at most 4096 bytes, returning how many (0 while a connection or
    /// stdin has nothing yet); `closed` at the end (for a `len` of 0 too).
    fn wasi_io_streams_method_input_stream_skip(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        Ok(self.read_stream(self_.0, len)?.len() as u64)
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_blocking_skip
    /// Like `skip`, but waits while stdin or a connection has nothing yet.
    fn wasi_io_streams_method_input_stream_blocking_skip(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        Ok(self.read_stream_waiting(self_.0, len)?.len() as u64)
    }
    // end optional

    // optional: wasi_sockets_network_resource_drop_network
    fn wasi_sockets_network_resource_drop_network(&mut self, _handle: Network) {}
    // end optional

    // optional: wasi_sockets_instance_network_instance_network
    /// The one network: handle 0, which no call looks at (the address policy is the host's).
    fn wasi_sockets_instance_network_instance_network(&mut self) -> Network {
        Network(0)
    }
    // end optional

    // optional: wasi_sockets_tcp_create_socket_create_tcp_socket
    fn wasi_sockets_tcp_create_socket_create_tcp_socket(
        &mut self,
        address_family: IpAddressFamily,
    ) -> Result<TcpSocket, NetErrorCode> {
        let ipv6 = address_family == IpAddressFamily::Ipv6;
        let slot = self.sockets.insert(NetSocket { ipv6, state: NetState::Unbound, options: TcpOptions::default() });
        Ok(TcpSocket(slot as u32))
    }
    // end optional

    // optional: wasi_sockets_tcp_resource_drop_tcp_socket
    fn wasi_sockets_tcp_resource_drop_tcp_socket(&mut self, handle: TcpSocket) {
        self.sockets.remove(handle.0 as usize);
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_start_bind
    /// `access-denied` outside loopback unless `with_external_network` was called.
    fn wasi_sockets_tcp_method_tcp_socket_start_bind(
        &mut self,
        self_: TcpSocket,
        _network: Network,
        local_address: IpSocketAddress,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_start_bind(self_.0, socket_addr(local_address))?)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_finish_bind
    fn wasi_sockets_tcp_method_tcp_socket_finish_bind(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_finish_bind(self_.0)?)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_start_listen
    fn wasi_sockets_tcp_method_tcp_socket_start_listen(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_start_listen(self_.0)?)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_finish_listen
    fn wasi_sockets_tcp_method_tcp_socket_finish_listen(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_finish_listen(self_.0)?)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_local_address
    fn wasi_sockets_tcp_method_tcp_socket_local_address(
        &mut self,
        self_: TcpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.tcp_address(self_.0, false)?))
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_start_connect
    /// Connects at once; a refusal shows in `finish-connect`.
    fn wasi_sockets_tcp_method_tcp_socket_start_connect(
        &mut self,
        self_: TcpSocket,
        _network: Network,
        remote_address: IpSocketAddress,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_start_connect(self_.0, socket_addr(remote_address))?)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_finish_connect
    fn wasi_sockets_tcp_method_tcp_socket_finish_connect(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(InputStream, OutputStream), NetErrorCode> {
        let (input, output) = self.tcp_finish_connect(self_.0)?;
        Ok((InputStream(input as u32), OutputStream(output as u32)))
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_accept
    /// `would-block` if no connection is waiting.
    fn wasi_sockets_tcp_method_tcp_socket_accept(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(TcpSocket, InputStream, OutputStream), NetErrorCode> {
        let (socket, input, output) = self.tcp_accept(self_.0)?;
        Ok((TcpSocket(socket as u32), InputStream(input as u32), OutputStream(output as u32)))
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_remote_address
    fn wasi_sockets_tcp_method_tcp_socket_remote_address(
        &mut self,
        self_: TcpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.tcp_address(self_.0, true)?))
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_subscribe
    /// Ready when `accept` would not block (a socket that is not listening is always ready).
    fn wasi_sockets_tcp_method_tcp_socket_subscribe(&mut self, self_: TcpSocket) -> Pollable {
        self.subscribe_wait(Wait::TcpAccept(self_.0 as usize))
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_subscribe
    /// Ready when stdin has data or is at its end, or a connection's stream has data or is
    /// at its end; a file is always ready.
    fn wasi_io_streams_method_input_stream_subscribe(&mut self, self_: InputStream) -> Pollable {
        if let Some(slot) = tcp_slot(self_.0) {
            return self.subscribe_wait(Wait::TcpReadable(slot));
        }
        if self_.0 != 0 {
            return self.subscribe(None);
        }
        self.start_stdin();
        self.subscribe_wait(Wait::Stdin)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_is_listening
    fn wasi_sockets_tcp_method_tcp_socket_is_listening(&mut self, self_: TcpSocket) -> bool {
        matches!(self.socket(self_.0).state, NetState::Listening { .. })
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_listen_backlog_size
    fn wasi_sockets_tcp_method_tcp_socket_set_listen_backlog_size(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        // `std` listens with its own backlog; the size is checked, not applied. While
        // listening it is ignored (as the spec allows), not an error.
        match self.socket(self_.0).state {
            NetState::ConnectStarted(_) | NetState::Connected { .. } | NetState::Closed => {
                Err(Net::InvalidState.into())
            }
            _ => Ok(()),
        }
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_keep_alive_enabled
    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_enabled(
        &mut self,
        self_: TcpSocket,
    ) -> Result<bool, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_enabled)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_keep_alive_enabled
    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_enabled(
        &mut self,
        self_: TcpSocket,
        value: bool,
    ) -> Result<(), NetErrorCode> {
        self.socket(self_.0).options.keep_alive_enabled = value;
        Ok(())
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_keep_alive_idle_time
    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_idle_time(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_idle_time)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_keep_alive_idle_time
    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_idle_time(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.keep_alive_idle_time = value;
        Ok(())
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_keep_alive_interval
    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_interval(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_interval)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_keep_alive_interval
    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_interval(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.keep_alive_interval = value;
        Ok(())
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_keep_alive_count
    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_count(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u32, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_count)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_keep_alive_count
    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_count(
        &mut self,
        self_: TcpSocket,
        value: u32,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.keep_alive_count = value;
        Ok(())
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_hop_limit
    fn wasi_sockets_tcp_method_tcp_socket_hop_limit(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u8, NetErrorCode> {
        Ok(self.socket(self_.0).options.hop_limit)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_hop_limit
    fn wasi_sockets_tcp_method_tcp_socket_set_hop_limit(
        &mut self,
        self_: TcpSocket,
        value: u8,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.hop_limit = value;
        Ok(())
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_receive_buffer_size
    fn wasi_sockets_tcp_method_tcp_socket_receive_buffer_size(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.receive_buffer_size)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_receive_buffer_size
    fn wasi_sockets_tcp_method_tcp_socket_set_receive_buffer_size(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.receive_buffer_size = value;
        Ok(())
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_send_buffer_size
    fn wasi_sockets_tcp_method_tcp_socket_send_buffer_size(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.send_buffer_size)
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_set_send_buffer_size
    fn wasi_sockets_tcp_method_tcp_socket_set_send_buffer_size(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.send_buffer_size = value;
        Ok(())
    }
    // end optional

    // optional: wasi_sockets_tcp_method_tcp_socket_shutdown
    fn wasi_sockets_tcp_method_tcp_socket_shutdown(
        &mut self,
        self_: TcpSocket,
        shutdown_type: ShutdownType,
    ) -> Result<(), NetErrorCode> {
        let how = match shutdown_type {
            ShutdownType::Receive => Shutdown::Read,
            ShutdownType::Send_ => Shutdown::Write,
            ShutdownType::Both => Shutdown::Both,
        };
        Ok(self.tcp_shutdown(self_.0, how)?)
    }
    // end optional

    // optional: wasi_sockets_udp_create_socket_create_udp_socket
    fn wasi_sockets_udp_create_socket_create_udp_socket(
        &mut self,
        address_family: IpAddressFamily,
    ) -> Result<UdpSocket, NetErrorCode> {
        let ipv6 = address_family == IpAddressFamily::Ipv6;
        let slot = self.udp_sockets.insert(UdpSock { ipv6, state: UdpState::Unbound, generation: Arc::new(AtomicU64::new(0)) });
        Ok(UdpSocket(slot as u32))
    }
    // end optional

    // optional: wasi_sockets_udp_resource_drop_udp_socket
    fn wasi_sockets_udp_resource_drop_udp_socket(&mut self, handle: UdpSocket) {
        self.udp_sockets.remove(handle.0 as usize);
    }
    // end optional

    // optional: wasi_sockets_udp_resource_drop_incoming_datagram_stream
    fn wasi_sockets_udp_resource_drop_incoming_datagram_stream(
        &mut self,
        handle: IncomingDatagramStream,
    ) {
        self.udp_in.remove(handle.0 as usize);
    }
    // end optional

    // optional: wasi_sockets_udp_resource_drop_outgoing_datagram_stream
    fn wasi_sockets_udp_resource_drop_outgoing_datagram_stream(
        &mut self,
        handle: OutgoingDatagramStream,
    ) {
        self.udp_out.remove(handle.0 as usize);
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_start_bind
    /// `access-denied` outside loopback unless `with_external_network` was called.
    fn wasi_sockets_udp_method_udp_socket_start_bind(
        &mut self,
        self_: UdpSocket,
        _network: Network,
        local_address: IpSocketAddress,
    ) -> Result<(), NetErrorCode> {
        Ok(self.udp_start_bind(self_.0, socket_addr(local_address))?)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_finish_bind
    fn wasi_sockets_udp_method_udp_socket_finish_bind(
        &mut self,
        self_: UdpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.udp_finish_bind(self_.0)?)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_stream
    /// `invalid-state` unless the socket is bound; `Some(remote)` connects the streams to it.
    fn wasi_sockets_udp_method_udp_socket_stream(
        &mut self,
        self_: UdpSocket,
        remote_address: Option<IpSocketAddress>,
    ) -> Result<(IncomingDatagramStream, OutgoingDatagramStream), NetErrorCode> {
        let (input, output) = self.udp_stream(self_.0, remote_address.map(socket_addr))?;
        Ok((IncomingDatagramStream(input as u32), OutgoingDatagramStream(output as u32)))
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_local_address
    fn wasi_sockets_udp_method_udp_socket_local_address(
        &mut self,
        self_: UdpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.udp_address(self_.0, false)?))
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_remote_address
    /// `invalid-state` unless `stream` was called with a remote address.
    fn wasi_sockets_udp_method_udp_socket_remote_address(
        &mut self,
        self_: UdpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.udp_address(self_.0, true)?))
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_address_family
    fn wasi_sockets_udp_method_udp_socket_address_family(&mut self, self_: UdpSocket) -> IpAddressFamily {
        if self.udp_socket(self_.0).ipv6 { IpAddressFamily::Ipv6 } else { IpAddressFamily::Ipv4 }
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_ipv6_only
    fn wasi_sockets_udp_method_udp_socket_ipv6_only(
        &mut self,
        _self_: UdpSocket,
    ) -> Result<bool, NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_set_ipv6_only
    fn wasi_sockets_udp_method_udp_socket_set_ipv6_only(
        &mut self,
        _self_: UdpSocket,
        _value: bool,
    ) -> Result<(), NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_unicast_hop_limit
    fn wasi_sockets_udp_method_udp_socket_unicast_hop_limit(
        &mut self,
        _self_: UdpSocket,
    ) -> Result<u8, NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_set_unicast_hop_limit
    fn wasi_sockets_udp_method_udp_socket_set_unicast_hop_limit(
        &mut self,
        _self_: UdpSocket,
        _value: u8,
    ) -> Result<(), NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_receive_buffer_size
    fn wasi_sockets_udp_method_udp_socket_receive_buffer_size(
        &mut self,
        _self_: UdpSocket,
    ) -> Result<u64, NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_set_receive_buffer_size
    fn wasi_sockets_udp_method_udp_socket_set_receive_buffer_size(
        &mut self,
        _self_: UdpSocket,
        _value: u64,
    ) -> Result<(), NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_send_buffer_size
    fn wasi_sockets_udp_method_udp_socket_send_buffer_size(
        &mut self,
        _self_: UdpSocket,
    ) -> Result<u64, NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_set_send_buffer_size
    fn wasi_sockets_udp_method_udp_socket_set_send_buffer_size(
        &mut self,
        _self_: UdpSocket,
        _value: u64,
    ) -> Result<(), NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }
    // end optional

    // optional: wasi_sockets_udp_method_udp_socket_subscribe
    /// Always ready: binding completes in `finish-bind`.
    fn wasi_sockets_udp_method_udp_socket_subscribe(&mut self, _self_: UdpSocket) -> Pollable {
        self.subscribe(None)
    }
    // end optional

    // optional: wasi_sockets_udp_method_incoming_datagram_stream_receive
    /// An empty list if nothing is queued (or `max_results` is 0).
    fn wasi_sockets_udp_method_incoming_datagram_stream_receive(
        &mut self,
        self_: IncomingDatagramStream,
        max_results: u64,
    ) -> Result<Vec<IncomingDatagram>, NetErrorCode> {
        let got = self.udp_receive(self_.0, max_results)?;
        Ok(got
            .into_iter()
            .map(|(data, from)| IncomingDatagram { data, remote_address: ip_socket_address(from) })
            .collect())
    }
    // end optional

    // optional: wasi_sockets_udp_method_incoming_datagram_stream_subscribe
    /// Ready when `receive` would return a datagram.
    fn wasi_sockets_udp_method_incoming_datagram_stream_subscribe(
        &mut self,
        self_: IncomingDatagramStream,
    ) -> Pollable {
        self.subscribe_wait(Wait::UdpReadable(self_.0 as usize))
    }
    // end optional

    // optional: wasi_sockets_udp_method_outgoing_datagram_stream_check_send
    fn wasi_sockets_udp_method_outgoing_datagram_stream_check_send(
        &mut self,
        self_: OutgoingDatagramStream,
    ) -> Result<u64, NetErrorCode> {
        self.udp_check_send(self_.0);
        Ok(UDP_SEND_BATCH)
    }
    // end optional

    // optional: wasi_sockets_udp_method_outgoing_datagram_stream_send
    /// Returns how many datagrams were sent (see `udp_send`).
    fn wasi_sockets_udp_method_outgoing_datagram_stream_send(
        &mut self,
        self_: OutgoingDatagramStream,
        datagrams: Vec<OutgoingDatagram>,
    ) -> Result<u64, NetErrorCode> {
        let datagrams = datagrams
            .into_iter()
            .map(|d| (d.data, d.remote_address.map(socket_addr)))
            .collect();
        Ok(self.udp_send(self_.0, datagrams)?)
    }
    // end optional

    // optional: wasi_sockets_udp_method_outgoing_datagram_stream_subscribe
    /// Always ready: `send` never waits.
    fn wasi_sockets_udp_method_outgoing_datagram_stream_subscribe(
        &mut self,
        _self_: OutgoingDatagramStream,
    ) -> Pollable {
        self.subscribe(None)
    }
    // end optional

    // optional: wasi_sockets_ip_name_lookup_resolve_addresses
    /// Resolves at once (see `resolve_name`); the stream holds the result.
    fn wasi_sockets_ip_name_lookup_resolve_addresses(
        &mut self,
        _network: Network,
        name: String,
    ) -> Result<ResolveAddressStream, NetErrorCode> {
        let found = self.resolve_name(&name)?;
        Ok(ResolveAddressStream(self.resolutions.insert(found) as u32))
    }
    // end optional

    // optional: wasi_sockets_ip_name_lookup_method_resolve_address_stream_resolve_next_address
    /// The next address, `None` once they are all read; never `would-block`.
    fn wasi_sockets_ip_name_lookup_method_resolve_address_stream_resolve_next_address(
        &mut self,
        self_: ResolveAddressStream,
    ) -> Result<Option<IpAddress>, NetErrorCode> {
        let stream = self.resolutions.get_mut(self_.0 as usize).expect("unknown or dropped resolve-address-stream");
        Ok(stream.pop_front().map(ip_address))
    }
    // end optional

    // optional: wasi_sockets_ip_name_lookup_method_resolve_address_stream_subscribe
    /// Always ready: the addresses are known when `resolve-addresses` returns.
    fn wasi_sockets_ip_name_lookup_method_resolve_address_stream_subscribe(
        &mut self,
        _self_: ResolveAddressStream,
    ) -> Pollable {
        self.subscribe(None)
    }
    // end optional

    // optional: wasi_sockets_ip_name_lookup_resource_drop_resolve_address_stream
    fn wasi_sockets_ip_name_lookup_resource_drop_resolve_address_stream(
        &mut self,
        handle: ResolveAddressStream,
    ) {
        self.resolutions.remove(handle.0 as usize);
    }
    // end optional
}
