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
// `directory-entry-stream.read-directory-entry` and file `input-stream.read`
// (`blocking-read` and `skip` are not implemented). Writing (`read-only` on a read-only
// preopen): `open-at` with create/exclusive/truncate and `write`, `write-via-stream`,
// `append-via-stream`, file `output-stream` `write`/`check-write`/`blocking-flush`,
// `create-directory-at`, `remove-directory-at`, `unlink-file-at`, `rename-at`, `symlink-at`,
// `link-at`, `set-size`, `sync`, `sync-data`, `set-times` and `set-times-at`. The semantics:
// one preopened directory shown as "/", descriptors and streams in
// slabs (the preopen is descriptor 0 and never freed; a file's input or output stream is
// `FIRST_FILE_STREAM + slot` in its own slab, clear of the stdio handles), paths resolved
// under the preopen (absolute paths, `..` and symlinks that lead out are `not-permitted`).
// It also serves TCP over loopback (`wasi:sockets` network, instance-network, tcp,
// tcp-create-socket) with these semantics: an address outside
// 127.0.0.0/8 and ::1 is `access-denied` unless `with_external_network` was called, `finish-bind`
// binds and listens, `start-connect` connects at once, reads never block and a socket's
// pollable is polled every millisecond; a connection's streams are `TCP_STREAM + slot`.
// UDP, ip-name-lookup and the socket options are not implemented.
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
    Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, SocketAddrV6, TcpListener, TcpStream,
};
#[allow(unused_imports)]
use std::path::{Component, Path, PathBuf};
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
    /// Output streams of connections, behind the output-stream handles `TCP_STREAM..`.
    tcp_out: Slab<TcpStream>,
}

#[allow(dead_code)]
impl TypedHost {
    fn new(env: Vec<(String, String)>) -> Self {
        TypedHost {
            args: Vec::new(),
            env,
            start: Instant::now(),
            stdin: SharedStdin::default(),
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
        self.descriptors.insert(OpenDescriptor { path: root.clone(), write: false, file: None }); // slot 0
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

    // optional: wasi_io_poll_resource_drop_pollable
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
                // a read error counts as the end of input
                let n = std::io::stdin().read(&mut chunk).unwrap_or(0);
                let (buf, changed) = &*shared;
                let mut buf = buf.lock().unwrap();
                buf.bytes.extend(&chunk[..n]);
                buf.eof = n == 0;
                changed.notify_all();
                if n == 0 {
                    return;
                }
            }
        });
    }

    /// The positions in `pollables` of the ones that are ready, waiting until at least one
    /// is: on the stdin buffer's condition variable, with a timeout at the earliest deadline
    /// (and every millisecond while a socket is among them, which nothing wakes it for).
    fn wait_ready(&self, pollables: &[Pollable]) -> Vec<u32> {
        let (buf, changed) = &*self.stdin;
        let mut buf = buf.lock().unwrap();
        loop {
            let now = Instant::now();
            let ready: Vec<u32> = (0..pollables.len() as u32)
                .filter(|&i| self.wait_of(pollables[i as usize]).is_ready(now, &buf, self))
                .collect();
            if !ready.is_empty() {
                return ready;
            }
            let mut earliest =
                pollables.iter().filter_map(|&p| self.wait_of(p).deadline()).min();
            if pollables.iter().any(|&p| self.wait_of(p).polls_socket()) {
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
        }
    }

    /// Whether readiness can only be found by asking the socket again.
    fn polls_socket(self) -> bool {
        matches!(self, Wait::TcpAccept(_) | Wait::TcpReadable(_))
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
    Connected(TcpStream),
    /// A connection attempt failed.
    Closed,
}

#[allow(dead_code)]
struct NetSocket {
    ipv6: bool,
    state: NetState,
}

/// Writes all of `bytes`, waiting (a millisecond at a time) while the send buffer is full.
#[allow(dead_code)]
fn write_all_waiting(stream: &mut TcpStream, mut bytes: &[u8]) -> std::io::Result<()> {
    use std::io::ErrorKind;
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[allow(dead_code)]
impl TypedHost {
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

    /// The socket behind a handle.
    fn socket(&mut self, handle: u32) -> &mut NetSocket {
        self.sockets.get_mut(handle as usize).expect("unknown or dropped tcp-socket")
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

    /// The slots in `tcp_in` and `tcp_out` of new streams over duplicates of `stream`.
    fn open_tcp_streams(&mut self, stream: &TcpStream) -> Result<(usize, usize), Net> {
        let dup = |s: &TcpStream| s.try_clone().map_err(|e| net_error(&e));
        let (input, output) = (dup(stream)?, dup(stream)?);
        Ok((TCP_STREAM + self.tcp_in.insert(input), TCP_STREAM + self.tcp_out.insert(output)))
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
        self.check_address(ipv6, &addr)?;
        if addr.port() == 0 || addr.ip().is_unspecified() {
            return Err(Net::InvalidArgument);
        }
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
        let streams = self.open_tcp_streams(&stream)?;
        self.socket(handle).state = NetState::Connected(stream);
        Ok(streams)
    }

    /// The new socket's slot and the stream handles (see `TCP_STREAM`).
    fn tcp_accept(&mut self, handle: u32) -> Result<(usize, usize, usize), Net> {
        let socket = self.socket(handle);
        let ipv6 = socket.ipv6;
        let NetState::Listening { listener, pending } = &socket.state else {
            return Err(Net::InvalidState);
        };
        let queued = pending.borrow_mut().take();
        let stream = match queued {
            Some(stream) => stream,
            None => listener.accept().map(|(stream, _)| stream).map_err(|e| net_error(&e))?,
        };
        stream.set_nonblocking(true).map_err(|e| net_error(&e))?;
        let (input, output) = self.open_tcp_streams(&stream)?;
        let slot = self.sockets.insert(NetSocket { ipv6, state: NetState::Connected(stream) });
        Ok((slot, input, output))
    }

    /// The local address of a bound socket, the remote one (`remote`) of a connected one.
    fn tcp_address(&mut self, handle: u32, remote: bool) -> Result<SocketAddr, Net> {
        match (&self.socket(handle).state, remote) {
            (NetState::Connected(stream), true) => stream.peer_addr(),
            (NetState::Connected(stream), false) => stream.local_addr(),
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
        match &self.socket(handle).state {
            NetState::Connected(stream) => stream.shutdown(how).map_err(|e| net_error(&e)),
            _ => Err(Net::InvalidState),
        }
    }
}

// The next blocks use generated types, so each is kept only if the component imports one of
// the methods that need it.

// optional: wasi_filesystem_types_method_descriptor_open_at | wasi_filesystem_types_method_descriptor_read_via_stream | wasi_filesystem_types_method_descriptor_get_flags | wasi_filesystem_types_method_descriptor_read_directory | wasi_filesystem_types_method_descriptor_stat | wasi_filesystem_types_method_descriptor_stat_at | wasi_filesystem_types_method_descriptor_readlink_at | wasi_filesystem_types_method_descriptor_metadata_hash | wasi_filesystem_types_method_descriptor_metadata_hash_at | wasi_filesystem_types_method_directory_entry_stream_read_directory_entry | wasi_filesystem_types_method_descriptor_create_directory_at | wasi_filesystem_types_method_descriptor_remove_directory_at | wasi_filesystem_types_method_descriptor_unlink_file_at | wasi_filesystem_types_method_descriptor_write_via_stream | wasi_filesystem_types_method_descriptor_append_via_stream | wasi_filesystem_types_method_descriptor_set_size | wasi_filesystem_types_method_descriptor_sync | wasi_filesystem_types_method_descriptor_sync_data | wasi_filesystem_types_method_descriptor_rename_at | wasi_filesystem_types_method_descriptor_symlink_at | wasi_filesystem_types_method_descriptor_link_at | wasi_filesystem_types_method_descriptor_set_times | wasi_filesystem_types_method_descriptor_set_times_at
impl From<Fs> for FsErrorCode {
    fn from(e: Fs) -> Self {
        match e {
            Fs::Access => FsErrorCode::Access,
            Fs::BadDescriptor => FsErrorCode::BadDescriptor,
            Fs::Exist => FsErrorCode::Exist,
            Fs::Invalid => FsErrorCode::Invalid,
            Fs::Io => FsErrorCode::Io,
            Fs::IsDirectory => FsErrorCode::IsDirectory,
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

// optional: wasi_filesystem_types_method_descriptor_stat | wasi_filesystem_types_method_descriptor_stat_at | wasi_filesystem_types_method_directory_entry_stream_read_directory_entry
/// Directory 3, regular file 6, symlink 5 (only directory entries are not followed),
/// otherwise unknown.
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

// optional: wasi_filesystem_types_method_descriptor_metadata_hash | wasi_filesystem_types_method_descriptor_metadata_hash_at
/// A hash of the file's identity (device and inode; its path where there are none), so
/// the same file gives the same value within one run and two files never do.
fn metadata_hash(meta: &std::fs::Metadata, path: &Path) -> MetadataHashValue {
    use std::hash::{DefaultHasher, Hash, Hasher};
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        let _ = path;
        (meta.dev(), meta.ino(), String::new())
    };
    #[cfg(not(unix))]
    let identity = {
        let _ = meta;
        (0u64, 0u64, path.to_string_lossy().into_owned())
    };
    let hash = |salt: u8| {
        let mut hasher = DefaultHasher::new();
        (salt, &identity).hash(&mut hasher);
        hasher.finish()
    };
    MetadataHashValue { lower: hash(0), upper: hash(1) }
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

// optional: wasi_sockets_tcp_create_socket_create_tcp_socket | wasi_sockets_tcp_method_tcp_socket_start_bind | wasi_sockets_tcp_method_tcp_socket_finish_bind | wasi_sockets_tcp_method_tcp_socket_start_listen | wasi_sockets_tcp_method_tcp_socket_finish_listen | wasi_sockets_tcp_method_tcp_socket_start_connect | wasi_sockets_tcp_method_tcp_socket_finish_connect | wasi_sockets_tcp_method_tcp_socket_accept | wasi_sockets_tcp_method_tcp_socket_local_address | wasi_sockets_tcp_method_tcp_socket_remote_address | wasi_sockets_tcp_method_tcp_socket_shutdown
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
        }
    }
}
// end optional

// optional: wasi_sockets_tcp_method_tcp_socket_start_bind | wasi_sockets_tcp_method_tcp_socket_start_connect
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

// optional: wasi_sockets_tcp_method_tcp_socket_local_address | wasi_sockets_tcp_method_tcp_socket_remote_address
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
    /// `exit(status: result)`: `ok` is 0, `error` is 1.
    fn wasi_cli_exit_exit(&mut self, status: Result<(), ()>) {
        std::io::stdout().flush().ok();
        std::process::exit(i32::from(status.is_err()));
    }
    // end optional

    // optional: wasi_io_error_resource_drop_error
    fn wasi_io_error_resource_drop_error(&mut self, _handle: Error) {}
    // end optional

    // optional: wasi_io_poll_resource_drop_pollable
    fn wasi_io_poll_resource_drop_pollable(&mut self, _handle: Pollable) {}
    // end optional

    // optional: wasi_io_streams_resource_drop_input_stream
    fn wasi_io_streams_resource_drop_input_stream(&mut self, handle: InputStream) {
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
        self.subscribe(Some(Instant::now() + Duration::from_nanos(when)))
    }
    // end optional

    // optional: wasi_clocks_monotonic_clock_subscribe_instant
    /// An instant is nanoseconds since the host was created (see `monotonic-clock.now`); one
    /// that has passed is ready at once.
    fn wasi_clocks_monotonic_clock_subscribe_instant(&mut self, when: u64) -> Pollable {
        self.subscribe(Some(self.start + Duration::from_nanos(when)))
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
        if let Some(slot) = tcp_slot(handle.0) {
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
    fn wasi_io_streams_method_output_stream_check_write(
        &mut self,
        _self_: OutputStream,
    ) -> Result<u64, StreamError> {
        Ok(4096)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_write
    fn wasi_io_streams_method_output_stream_write(
        &mut self,
        self_: OutputStream,
        contents: Vec<u8>,
    ) -> Result<(), StreamError> {
        let written = if let Some(slot) = tcp_slot(self_.0) {
            match self.tcp_out.get_mut(slot) {
                Some(stream) => write_all_waiting(stream, &contents),
                None => Err(std::io::ErrorKind::BrokenPipe.into()),
            }
        } else {
            match (self_.0, file_index(self_.0)) {
                (_, Some(index)) => match self.outputs.get_mut(index) {
                    Some(file) => file.write_all(&contents),
                    None => Err(std::io::ErrorKind::NotFound.into()),
                },
                (2, None) => std::io::stderr().write_all(&contents),
                (_, None) => std::io::stdout().write_all(&contents),
            }
        };
        // a failed write ends the stream
        written.map_err(|_| StreamError::Closed)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_flush
    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        self_: OutputStream,
    ) -> Result<(), StreamError> {
        if tcp_slot(self_.0).is_some() {
            return Ok(()); // nothing is buffered on a connection
        }
        // stderr is unbuffered in `std`, so only stdout has anything to flush
        std::io::stdout().flush().map_err(|_| StreamError::Closed)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_subscribe
    fn wasi_io_streams_method_output_stream_subscribe(&mut self, _self_: OutputStream) -> Pollable {
        self.subscribe(None)
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
    /// Path flags are ignored (links are always followed, within the preopen). Creating,
    /// truncating and writing are `read-only` unless the preopen is writable; then `create`,
    /// `exclusive` and `truncate` act at once, and the descriptor remembers whether it was
    /// opened for `write`.
    fn wasi_filesystem_types_method_descriptor_open_at(
        &mut self,
        self_: Descriptor,
        _path_flags: PathFlags,
        path: String,
        open_flags: OpenFlags,
        flags: DescriptorFlags,
    ) -> Result<Descriptor, FsErrorCode> {
        let create = open_flags.contains(OpenFlags::CREATE);
        let exclusive = open_flags.contains(OpenFlags::EXCLUSIVE);
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
        if create && exclusive && full.exists() {
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
        let file = if is_dir {
            None
        } else {
            // a file that only writes need not be readable (and one with no flags reads)
            let reads = flags.contains(DescriptorFlags::READ) || !writes;
            let opened = std::fs::OpenOptions::new().read(reads).write(writes).open(&full);
            Some(opened.map_err(io_error_code)?)
        };
        let slot = self.descriptors.insert(OpenDescriptor { path: full, write: writes, file });
        Ok(Descriptor(slot as u32))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_via_stream
    /// A new stream over the file, starting at `offset`.
    fn wasi_filesystem_types_method_descriptor_read_via_stream(
        &mut self,
        self_: Descriptor,
        offset: u64,
    ) -> Result<InputStream, FsErrorCode> {
        let file = self.open_descriptor(self_.0)?.file.as_ref().ok_or(Fs::IsDirectory)?;
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

    // optional: wasi_filesystem_types_method_descriptor_get_flags
    /// Every descriptor is `read`, plus `write` if it was opened for writing.
    fn wasi_filesystem_types_method_descriptor_get_flags(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorFlags, FsErrorCode> {
        let write = self.open_descriptor(self_.0)?.write;
        Ok(if write { DescriptorFlags::READ.union(DescriptorFlags::WRITE) } else { DescriptorFlags::READ })
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
    /// Like `stat`, for a path under the descriptor (links are followed).
    fn wasi_filesystem_types_method_descriptor_stat_at(
        &mut self,
        self_: Descriptor,
        _path_flags: PathFlags,
        path: String,
    ) -> Result<DescriptorStat, FsErrorCode> {
        let full = self.resolve(self_.0, &path)?;
        Ok(descriptor_stat(std::fs::metadata(full).map_err(io_error_code)?))
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
    fn wasi_filesystem_types_method_descriptor_metadata_hash_at(
        &mut self,
        self_: Descriptor,
        _path_flags: PathFlags,
        path: String,
    ) -> Result<MetadataHashValue, FsErrorCode> {
        let full = self.resolve(self_.0, &path)?;
        let meta = std::fs::metadata(&full).map_err(io_error_code)?;
        Ok(metadata_hash(&meta, &full))
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
        let mut buf = vec![0u8; len.min(4096) as usize];
        if self_.0 == 0 {
            if buf.is_empty() {
                return Ok(buf);
            }
            self.start_stdin();
            let mut shared = self.stdin.0.lock().unwrap();
            let n = std::io::Read::read(&mut shared.bytes, &mut buf).unwrap_or(0); // a `VecDeque` never fails
            if n == 0 && shared.eof {
                return Err(StreamError::Closed);
            }
            buf.truncate(n);
            return Ok(buf);
        }
        if let Some(slot) = tcp_slot(self_.0) {
            // never blocks: an empty list means "nothing yet"
            let stream = self.tcp_in.get_mut(slot).expect("read from an unknown or dropped stream");
            return match std::io::Read::read(stream, &mut buf) {
                Ok(0) if !buf.is_empty() => Err(StreamError::Closed),
                Ok(n) => {
                    buf.truncate(n);
                    Ok(buf)
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(Vec::new()),
                Err(_) => Err(StreamError::Closed),
            };
        }
        let Some(index) = file_index(self_.0) else {
            return Err(StreamError::Closed);
        };
        let file = self.files.get_mut(index).expect("read from an unknown or dropped stream");
        if buf.is_empty() {
            return Ok(buf);
        }
        match file.read(&mut buf) {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Ok(buf)
            }
            _ => Err(StreamError::Closed),
        }
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
        let slot = self.sockets.insert(NetSocket { ipv6, state: NetState::Unbound });
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
}
