// A minimal WASI preview2 host for the lowered imports of a `wasm32-wasip2`
// program (`println!`, arguments, environment, exit, stdin, `poll`, clocks, random, files). It is source text appended to
// the generated code (see `examples/wasip2_run.rs`), not a crate: the `Imports` trait
// it implements is generated per module. Build the module with `named_imports`,
// `named_exports` and `reentrant_imports`: the methods are named after the imports,
// and each receives the guest `Instance`, so the host reads and writes the guest's
// linear memory through pointers and calls its `cabi_realloc` to build the lists
// and strings it returns.
//
// The trait has one method per import of the module, so a method for an import the
// module does not have is an error. Blocks between `// optional: <method>` and
// `// end optional` are kept only when the generated trait declares `<method>`
// (see `keep_declared_methods` in `examples/wasip2_run.rs`).
//
// Handles: 0 = stdin, 1 = stdout, 2 = stderr; terminals are placeholders. A pollable
// is an index into `pollables`: always ready (streams, files), ready at a deadline
// (clocks), or ready when stdin has data or is at its end. A thread reads the real
// stdin into a buffer, so `read` never blocks (an empty list means "nothing yet")
// and `poll` and `block` wait on the buffer and the earliest deadline.
// Random numbers come from a small non-cryptographic generator (splitmix64) seeded
// from the OS-seeded `std` hasher: fine for `HashMap` seeds and examples, not for
// keys or tokens.
// Files: a single preopened directory (`with_preopen`, shown to the guest as "/"),
// read-only unless `with_writable_preopen`. A descriptor is an index into `descriptors`
// (the preopen is 0), a file's input stream is `FIRST_FILE_STREAM + index` into `files` and
// its output stream the same into `outputs`. Paths are resolved against the preopen and
// must stay under it: absolute paths and `..` are refused (`not-permitted`), and so are
// symlinks that lead out. Anything that would write is refused with `read-only`; with a
// writable preopen, files can be created, written, appended to, truncated, removed and
// renamed, and directories created and removed (symlinks are never followed for the
// last name).
// `stat`, `get-flags`, `metadata-hash` and directory listings work on the same paths.
// `resource-drop` frees a handle's slot, which the next resource of its kind reuses; a guest
// must not use a handle after dropping it (see docs/component-model-poc.md).
// Not implemented: sockets.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// What a pollable waits for.
#[derive(Clone, Copy)]
enum Pollable {
    /// Always ready.
    Ready,
    /// Ready at the instant.
    Deadline(Instant),
    /// Ready when stdin has data or is at its end. Built only by the optional
    /// `input-stream.subscribe` block.
    #[allow(dead_code)]
    Stdin,
}

impl Pollable {
    fn is_ready(self, now: Instant, stdin: &StdinBuf) -> bool {
        match self {
            Pollable::Ready => true,
            Pollable::Deadline(d) => d <= now,
            Pollable::Stdin => !stdin.bytes.is_empty() || stdin.eof,
        }
    }

    fn deadline(self) -> Option<Instant> {
        match self {
            Pollable::Deadline(d) => Some(d),
            _ => None,
        }
    }
}

/// The bytes the stdin thread has read and not yet handed to the guest.
#[derive(Default)]
struct StdinBuf {
    bytes: VecDeque<u8>,
    eof: bool,
}

type SharedStdin = Arc<(Mutex<StdinBuf>, Condvar)>;

struct P2Host {
    /// Read only by the optional `get-arguments` block.
    #[allow(dead_code)]
    args: Vec<String>,
    env: Vec<(String, String)>,
    pollables: Slab<Pollable>,
    /// Filled by the stdin thread, which `start_stdin` spawns on first use.
    stdin: SharedStdin,
    /// Read only by `start_stdin`, which only the optional stdin blocks call.
    #[allow(dead_code)]
    stdin_started: bool,
    /// The monotonic clock's zero; read only by the optional clock blocks.
    #[allow(dead_code)]
    start: Instant,
    /// The generator state; read only by the optional `wasi:random` blocks.
    #[allow(dead_code)]
    rng: u64,
    /// The preopened directory (canonical); read only by the optional filesystem blocks.
    #[allow(dead_code)]
    root: Option<PathBuf>,
    /// Whether the guest may write under `root`; set by `with_writable_preopen`.
    #[allow(dead_code)]
    writable: bool,
    /// Descriptor handles (the preopen is slot 0, never freed), all under `root`.
    #[allow(dead_code)]
    descriptors: Slab<Descriptor>,
    /// Open files behind input-stream handles `3..`.
    #[allow(dead_code)]
    files: Slab<std::fs::File>,
    /// Open files behind the output-stream handles `3..` (1 and 2 are stdout and stderr).
    outputs: Slab<std::fs::File>,
    /// Directory listings behind `directory-entry-stream` handles: the entries
    /// (type, name) still to be read.
    #[allow(dead_code)]
    listings: Slab<std::vec::IntoIter<(u8, String)>>,
}

type Guest = Instance<P2Host>;

impl P2Host {
    fn new(args: Vec<String>, env: Vec<(String, String)>) -> Self {
        P2Host {
            args,
            env,
            pollables: Slab::new(),
            stdin: SharedStdin::default(),
            stdin_started: false,
            start: Instant::now(),
            rng: os_seed(),
            root: None,
            writable: false,
            descriptors: Slab::new(),
            files: Slab::new(),
            outputs: Slab::new(),
            listings: Slab::new(),
        }
    }

    /// Gives the guest the directory `root` (as its "/"), read-only; see
    /// `with_writable_preopen`.
    #[allow(dead_code)]
    fn with_preopen(mut self, root: PathBuf) -> Self {
        let root = root.canonicalize().expect("preopen: directory does not exist");
        self.descriptors.insert(Descriptor { path: root.clone(), write: false }); // slot 0
        self.root = Some(root);
        self
    }

    /// Like `with_preopen`, but the guest may create, write, append to and truncate
    /// files under `root`.
    #[allow(dead_code)]
    fn with_writable_preopen(mut self, root: PathBuf) -> Self {
        self.writable = true;
        self.with_preopen(root)
    }

    /// The open file behind an input-stream handle (`None` for stdin, stdout, stderr or
    /// an unknown handle).
    #[allow(dead_code)]
    fn file_stream(&mut self, stream: i32) -> Option<&mut std::fs::File> {
        self.files.get_mut(file_index(stream)?)
    }

    /// The descriptor behind a handle, or `bad-descriptor`.
    #[allow(dead_code)]
    fn open_descriptor(&self, descriptor: i32) -> Result<&Descriptor, u8> {
        self.descriptors.get(descriptor as u32 as usize).ok_or(code::BAD_DESCRIPTOR)
    }

    /// The path behind a descriptor handle, or `bad-descriptor`.
    #[allow(dead_code)]
    fn descriptor(&self, descriptor: i32) -> Result<&Path, u8> {
        self.open_descriptor(descriptor).map(|d| d.path.as_path())
    }

    /// Like `resolve`, for the path given as `(ptr, len)` in guest memory.
    #[allow(dead_code)]
    fn resolve_at(&self, g: &mut Guest, dir: i32, ptr: i32, len: i32) -> Result<PathBuf, u8> {
        self.resolve(dir, &get_str(g, ptr, len).ok_or(code::IO)?)
    }

    /// The path `path` names under the directory behind descriptor `dir`, or the
    /// `error-code` to return: `not-permitted` outside the preopen.
    #[allow(dead_code)]
    fn resolve(&self, dir: i32, path: &str) -> Result<PathBuf, u8> {
        let root = self.root.as_ref().ok_or(code::NOT_PERMITTED)?;
        let base = self.descriptor(dir)?;
        let relative = Path::new(path);
        if relative.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
            return Err(code::NOT_PERMITTED);
        }
        let full = base.join(relative).canonicalize().map_err(io_error_code)?;
        if full.starts_with(root) { Ok(full) } else { Err(code::NOT_PERMITTED) }
    }

    /// The entry `path` names under the directory behind descriptor `dir`, without
    /// following it if it is a symlink: its parent directory must exist and resolve
    /// under the preopen, and the name is joined to it. The entry itself need not
    /// exist. Absolute paths and `..` are `not-permitted`; a name ending in `/` or `/.`
    /// names a directory, which `file_name` would silently drop, so it is `invalid`.
    #[allow(dead_code)]
    fn resolve_entry(&self, dir: i32, path: &str) -> Result<PathBuf, u8> {
        let root = self.root.as_ref().ok_or(code::NOT_PERMITTED)?;
        let base = self.descriptor(dir)?;
        let relative = Path::new(path);
        if relative.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
            return Err(code::NOT_PERMITTED);
        }
        if path.ends_with('/') || path.ends_with("/.") {
            return Err(code::INVALID);
        }
        let name = relative.file_name().ok_or(code::INVALID)?;
        let parent = relative.parent().unwrap_or(Path::new(""));
        let parent = base.join(parent).canonicalize().map_err(io_error_code)?;
        if parent.starts_with(root) { Ok(parent.join(name)) } else { Err(code::NOT_PERMITTED) }
    }

    /// Like `resolve`, for a file that `open-at` may create: if `path` does not exist,
    /// it is named in its parent directory (see `resolve_entry`). A symlink that leads
    /// nowhere (or out of the preopen) is `not-permitted`, not a place to create a file.
    #[allow(dead_code)]
    fn resolve_to_create(&self, dir: i32, path: &str) -> Result<PathBuf, u8> {
        match self.resolve(dir, path) {
            Err(code::NO_ENTRY) => {}
            found => return found,
        }
        let full = self.resolve_entry(dir, path)?;
        if full.symlink_metadata().is_ok() { Err(code::NOT_PERMITTED) } else { Ok(full) }
    }

    /// `Ok` if the preopen is writable, else `read-only`.
    #[allow(dead_code)]
    fn require_writable(&self) -> Result<(), u8> {
        if self.writable { Ok(()) } else { Err(code::READ_ONLY) }
    }

    /// Like `resolve_entry`, for the path given as `(ptr, len)` in guest memory.
    #[allow(dead_code)]
    fn entry_at(&self, g: &mut Guest, dir: i32, ptr: i32, len: i32) -> Result<PathBuf, u8> {
        self.resolve_entry(dir, &get_str(g, ptr, len).ok_or(code::IO)?)
    }

    /// The file behind a descriptor opened for `write`, opened for writing (`append`:
    /// at its end), or the `error-code`: `read-only` unless the preopen is writable.
    #[allow(dead_code)]
    fn writable_file(&self, descriptor: i32, append: bool) -> Result<std::fs::File, u8> {
        self.require_writable()?;
        let descriptor = self.open_descriptor(descriptor)?;
        if descriptor.path.is_dir() {
            return Err(code::IS_DIRECTORY);
        }
        if !descriptor.write {
            return Err(code::BAD_DESCRIPTOR);
        }
        let opened = std::fs::OpenOptions::new().write(true).append(append).open(&descriptor.path);
        opened.map_err(io_error_code)
    }

    /// Flushes the file or directory behind a descriptor to disk (`data_only`: without
    /// its metadata).
    #[allow(dead_code)]
    fn sync_descriptor(&self, descriptor: i32, data_only: bool) -> Result<(), u8> {
        let file = std::fs::File::open(self.descriptor(descriptor)?).map_err(io_error_code)?;
        let synced = if data_only { file.sync_data() } else { file.sync_all() };
        synced.map_err(io_error_code)
    }

    /// Sets the times of the file or directory behind a descriptor, each a
    /// `(tag, seconds, nanoseconds)` `new-timestamp`: `no-change` 0, `now` 1, `timestamp` 2.
    /// A preopen that is not writable is `read-only`; nanoseconds of a second or more are
    /// `invalid`.
    #[allow(dead_code)]
    fn touch(
        &self,
        descriptor: i32,
        access: (i32, i64, i32),
        modification: (i32, i64, i32),
    ) -> Result<(), u8> {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        self.require_writable()?;
        let path = self.descriptor(descriptor)?;
        let time = |(tag, seconds, nanos): (i32, i64, i32)| match tag {
            0 => Ok(None),
            1 => Ok(Some(SystemTime::now())),
            _ if !(0..1_000_000_000).contains(&nanos) => Err(code::INVALID),
            _ => {
                let since = Duration::new(seconds as u64, nanos as u32);
                UNIX_EPOCH.checked_add(since).map(Some).ok_or(code::INVALID)
            }
        };
        let (access, modification) = (time(access)?, time(modification)?);
        let mut times = std::fs::FileTimes::new();
        if let Some(t) = access {
            times = times.set_accessed(t);
        }
        if let Some(t) = modification {
            times = times.set_modified(t);
        }
        let file = std::fs::File::open(path).map_err(io_error_code)?;
        file.set_times(times).map_err(io_error_code)
    }

    /// `result<output-stream, error-code>` at `ret`: a new stream over `opened`, or the error.
    #[allow(dead_code)]
    fn put_output_stream(&mut self, g: &mut Guest, ret: i32, opened: Result<std::fs::File, u8>) {
        match opened {
            Ok(file) => {
                let slot = self.outputs.insert(file);
                fs_ok_handle(g, ret, FIRST_FILE_STREAM + slot);
            }
            Err(c) => fs_error(g, ret, 4, c),
        }
    }

    /// The next 64 random bits (splitmix64).
    #[allow(dead_code)]
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A `list<u8>` of `len` random bytes at `ret`: `(ptr, len)`.
    #[allow(dead_code)]
    fn random_bytes(&mut self, g: &mut Guest, len: i64, ret: i32) {
        let len = u32::try_from(len as u64).expect("random bytes: length over 4 GiB") as usize;
        let bytes: Vec<u8> = std::iter::repeat_with(|| self.next_u64().to_le_bytes())
            .flatten()
            .take(len)
            .collect();
        let ptr = guest_list(g, ret, 1, len as i32);
        put(g, ptr, &bytes);
    }

    /// The pollable behind a handle.
    fn pollable(&self, handle: u32) -> &Pollable {
        self.pollables.get(handle as usize).expect("unknown or dropped pollable")
    }

    /// A new pollable.
    fn subscribe(&mut self, pollable: Pollable) -> i32 {
        self.pollables.insert(pollable) as i32
    }

    /// Starts the thread that reads the real stdin into the buffer (once).
    #[allow(dead_code)]
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

    /// The positions in `handles` of the pollables that are ready, waiting until at
    /// least one is: on the stdin buffer's condition variable, with a timeout at the
    /// earliest deadline.
    fn wait_ready(&self, handles: &[u32]) -> Vec<i32> {
        let (buf, changed) = &*self.stdin;
        let mut buf = buf.lock().unwrap();
        loop {
            let now = Instant::now();
            let ready: Vec<i32> = (0..handles.len() as i32)
                .filter(|&i| self.pollable(handles[i as usize]).is_ready(now, &buf))
                .collect();
            if !ready.is_empty() {
                return ready;
            }
            let earliest = handles.iter().filter_map(|&h| self.pollable(h).deadline()).min();
            buf = match earliest {
                Some(d) => changed.wait_timeout(buf, d.saturating_duration_since(now)).unwrap().0,
                None => changed.wait(buf).unwrap(),
            };
        }
    }
}

/// What a descriptor handle names: a path under the preopen, and whether the guest
/// opened it for writing.
#[allow(dead_code)]
struct Descriptor {
    path: PathBuf,
    write: bool,
}

/// A table of live handles: a handle is a slot's index, and the slot of a dropped
/// handle is reused by the next `insert`. A guest must not use a handle after
/// dropping it (the Component Model forbids it); the host does not detect that, and
/// the stale handle may then name a newer resource.
struct Slab<T> {
    slots: Vec<Option<T>>,
    free: Vec<usize>,
}

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

    #[allow(dead_code)]
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

/// The handle of the first file's stream, input or output (0 to 2 are stdin, stdout,
/// stderr).
#[allow(dead_code)]
const FIRST_FILE_STREAM: usize = 3;

/// The slot in `files` (an input stream) or `outputs` (an output stream) of a stream
/// handle (`None` for stdin, stdout, stderr).
#[allow(dead_code)]
fn file_index(stream: i32) -> Option<usize> {
    (stream as u32 as usize).checked_sub(FIRST_FILE_STREAM)
}

/// `wasi:filesystem` error codes; which ones a program's host uses depends on the
/// optional blocks kept.
#[allow(dead_code)]
mod code {
    pub const ACCESS: u8 = 0;
    pub const BAD_DESCRIPTOR: u8 = 3;
    pub const EXIST: u8 = 7;
    pub const INVALID: u8 = 12;
    pub const IO: u8 = 13;
    pub const IS_DIRECTORY: u8 = 14;
    pub const NO_ENTRY: u8 = 20;
    pub const NOT_DIRECTORY: u8 = 24;
    pub const NOT_EMPTY: u8 = 25;
    pub const NOT_PERMITTED: u8 = 31;
    pub const READ_ONLY: u8 = 33;
}

#[allow(dead_code)]
fn io_error_code(e: std::io::Error) -> u8 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => code::NO_ENTRY,
        PermissionDenied => code::ACCESS,
        AlreadyExists => code::EXIST,
        DirectoryNotEmpty => code::NOT_EMPTY,
        NotADirectory => code::NOT_DIRECTORY,
        IsADirectory => code::IS_DIRECTORY,
        _ => code::IO,
    }
}

/// A seed from the OS: `RandomState` is keyed from the system's randomness.
fn os_seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(0u8)
}

fn put(g: &mut Guest, at: i32, bytes: &[u8]) {
    let at = at as u32 as usize;
    g.memory()[at..at + bytes.len()].copy_from_slice(bytes);
}

fn put_u32(g: &mut Guest, at: i32, value: i32) {
    put(g, at, &value.to_le_bytes());
}

fn get_u32(g: &mut Guest, at: i32) -> u32 {
    let at = at as u32 as usize;
    u32::from_le_bytes(g.memory()[at..at + 4].try_into().unwrap())
}

/// Memory the guest owns, from its own allocator (`cabi_realloc(0, 0, align, size)`).
fn alloc(g: &mut Guest, align: i32, size: i32) -> i32 {
    g.cabi_realloc(0, 0, align, size)
}

/// A string copied into guest memory, with its `(ptr, len)` written at `at`.
fn put_string(g: &mut Guest, at: i32, s: &str) {
    let ptr = alloc(g, 1, s.len() as i32);
    put(g, ptr, s.as_bytes());
    put_u32(g, at, ptr);
    put_u32(g, at + 4, s.len() as i32);
}

/// A `list<_>` of `count` elements of `size` bytes at `ret`: `(ptr, len)`. Returns
/// the elements' address for the caller to fill.
fn guest_list(g: &mut Guest, ret: i32, size: i32, count: i32) -> i32 {
    let ptr = if count == 0 { 4 } else { alloc(g, 4, size * count) };
    put_u32(g, ret, ptr);
    put_u32(g, ret + 4, count);
    ptr
}

/// A zero tag byte at `ptr`: `result::ok` or `option::none`.
fn zero_tag(g: &mut Guest, ptr: i32) {
    put(g, ptr, &[0]);
}

/// `result<_, error-code>::err` at `ret`: the error's tag byte sits at `ret + payload`,
/// the offset of the result's payload (its alignment).
#[allow(dead_code)]
fn fs_error(g: &mut Guest, ret: i32, payload: i32, code: u8) {
    put(g, ret, &[1]);
    put(g, ret + payload, &[code]);
}

/// `result<_, error-code>` at `ret`: the error's tag byte is at `ret + 1`.
#[allow(dead_code)]
fn put_status(g: &mut Guest, ret: i32, status: Result<(), u8>) {
    match status {
        Ok(()) => zero_tag(g, ret),
        Err(c) => fs_error(g, ret, 1, c),
    }
}

/// `result<handle, error-code>::ok` at `ret`.
#[allow(dead_code)]
fn fs_ok_handle(g: &mut Guest, ret: i32, handle: usize) {
    zero_tag(g, ret);
    put_u32(g, ret + 4, handle as i32);
}

/// `option<datetime>` at `at`: the tag, seconds (`u64`) at `at + 8`, nanoseconds at
/// `at + 16`; `none` if the time is unknown.
#[allow(dead_code)]
fn put_datetime_option(g: &mut Guest, at: i32, time: std::io::Result<std::time::SystemTime>) {
    let since = time.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
    match since {
        Some(d) => {
            put(g, at, &[1]);
            put(g, at + 8, &d.as_secs().to_le_bytes());
            put_u32(g, at + 16, d.subsec_nanos() as i32);
        }
        None => zero_tag(g, at),
    }
}

/// The `descriptor-type` of a file type: directory 3, regular file 6, symlink 5 (only
/// directory entries are not followed), otherwise 0 (unknown).
#[allow(dead_code)]
fn descriptor_type(t: std::fs::FileType) -> u8 {
    if t.is_dir() {
        3
    } else if t.is_file() {
        6
    } else if t.is_symlink() {
        5
    } else {
        0
    }
}

/// `result<descriptor-stat, error-code>` at `ret` for `path`: the record starts at
/// `ret + 8` (type, link count, size, then access, modification and status-change
/// times as `option<datetime>` of 24 bytes; the status-change time is `none`).
#[allow(dead_code)]
fn put_stat(g: &mut Guest, ret: i32, path: &Path) {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) => return fs_error(g, ret, 8, io_error_code(e)),
    };
    #[cfg(unix)]
    let links = std::os::unix::fs::MetadataExt::nlink(&meta);
    #[cfg(not(unix))]
    let links = 1u64;
    zero_tag(g, ret);
    put(g, ret + 8, &[descriptor_type(meta.file_type())]);
    put(g, ret + 16, &links.to_le_bytes());
    put(g, ret + 24, &meta.len().to_le_bytes());
    put_datetime_option(g, ret + 32, meta.accessed());
    put_datetime_option(g, ret + 56, meta.modified());
    zero_tag(g, ret + 80); // the status-change time: none
}

/// `result<metadata-hash-value, error-code>` at `ret`: two `u64`s at `ret + 8`, a
/// hash of the file's canonical path (the same file gives the same value within one run).
#[allow(dead_code)]
fn put_metadata_hash(g: &mut Guest, ret: i32, path: &Path) {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let hash = |salt: u8| {
        let mut hasher = DefaultHasher::new();
        (salt, path).hash(&mut hasher);
        hasher.finish()
    };
    zero_tag(g, ret);
    put(g, ret + 8, &hash(0).to_le_bytes());
    put(g, ret + 16, &hash(1).to_le_bytes());
}

/// The `len` bytes at `ptr` as text, `None` if they are not UTF-8.
#[allow(dead_code)]
fn get_str(g: &mut Guest, ptr: i32, len: i32) -> Option<String> {
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    String::from_utf8(g.memory()[ptr..ptr + len].to_vec()).ok()
}

fn stream_write(g: &mut Guest, stream: i32, ptr: i32, len: i32) {
    use std::io::Write;
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    let bytes = &g.memory()[ptr..ptr + len];
    match stream {
        2 => std::io::stderr().write_all(bytes).unwrap(),
        _ => std::io::stdout().write_all(bytes).unwrap(),
    }
}

impl Imports for P2Host {
    fn wasi_io_error_resource_drop_error(&mut self, _g: &mut Guest, _handle: i32) {}
    fn wasi_io_poll_resource_drop_pollable(&mut self, _g: &mut Guest, handle: i32) {
        self.pollables.remove(handle as u32 as usize);
    }
    fn wasi_io_streams_resource_drop_input_stream(&mut self, _g: &mut Guest, handle: i32) {
        if let Some(index) = file_index(handle) {
            self.files.remove(index);
        }
    }
    fn wasi_io_streams_resource_drop_output_stream(&mut self, _g: &mut Guest, handle: i32) {
        if let Some(index) = file_index(handle) {
            self.outputs.remove(index);
        }
    }
    fn wasi_cli_terminal_input_resource_drop_terminal_input(
        &mut self,
        _g: &mut Guest,
        _handle: i32,
    ) {
    }
    fn wasi_cli_terminal_output_resource_drop_terminal_output(
        &mut self,
        _g: &mut Guest,
        _handle: i32,
    ) {
    }

    // optional: wasi_cli_environment_get_arguments
    /// `list<string>` at `ret`: 8-byte `(ptr, len)` elements.
    fn wasi_cli_environment_get_arguments(&mut self, g: &mut Guest, ret: i32) {
        let items = guest_list(g, ret, 8, self.args.len() as i32);
        for (i, arg) in self.args.iter().enumerate() {
            put_string(g, items + 8 * i as i32, arg);
        }
    }
    // end optional

    /// `list<tuple<string, string>>` at `ret`: 16-byte `(key, value)` elements.
    fn wasi_cli_environment_get_environment(&mut self, g: &mut Guest, ret: i32) {
        let items = guest_list(g, ret, 16, self.env.len() as i32);
        for (i, (key, value)) in self.env.iter().enumerate() {
            let at = items + 16 * i as i32;
            put_string(g, at, key);
            put_string(g, at + 8, value);
        }
    }

    /// `exit(status: result)`: 0 is `ok`, 1 is `error`.
    fn wasi_cli_exit_exit(&mut self, _g: &mut Guest, status: i32) {
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::process::exit(status);
    }

    fn wasi_io_poll_method_pollable_block(&mut self, _g: &mut Guest, pollable: i32) {
        self.wait_ready(&[pollable as u32]);
    }

    /// `poll(in: list<borrow<pollable>>) -> list<u32>`: the indices of the ready
    /// pollables, waiting until one is ready. An empty list traps, as the interface
    /// specifies.
    fn wasi_io_poll_poll(&mut self, g: &mut Guest, list: i32, len: i32, ret: i32) {
        assert!(len > 0, "wasi:io/poll.poll: empty list");
        let handles: Vec<u32> = (0..len).map(|i| get_u32(g, list + 4 * i)).collect();
        let ready = self.wait_ready(&handles);
        let items = guest_list(g, ret, 4, ready.len() as i32);
        for (i, index) in ready.iter().enumerate() {
            put_u32(g, items + 4 * i as i32, *index);
        }
    }

    /// `result<u64, stream-error>` at `ret`: `ok` with a fixed write budget.
    fn wasi_io_streams_method_output_stream_check_write(
        &mut self,
        g: &mut Guest,
        _stream: i32,
        ret: i32,
    ) {
        zero_tag(g, ret);
        put(g, ret + 8, &4096u64.to_le_bytes());
    }

    fn wasi_io_streams_method_output_stream_write(
        &mut self,
        g: &mut Guest,
        stream: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        use std::io::Write;
        let Some(index) = file_index(stream) else {
            stream_write(g, stream, ptr, len);
            return zero_tag(g, ret);
        };
        let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
        let bytes = &g.memory()[ptr..ptr + len];
        let written = self.outputs.get_mut(index).map(|file| file.write_all(bytes));
        if let Some(Ok(())) = written {
            zero_tag(g, ret);
        } else {
            // a failed write ends the stream: `result::err(stream-error::closed)`
            put(g, ret, &[1]);
            put(g, ret + 4, &[1]);
        }
    }

    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        g: &mut Guest,
        _stream: i32,
        ret: i32,
    ) {
        use std::io::Write;
        std::io::stdout().flush().ok();
        zero_tag(g, ret);
    }

    fn wasi_io_streams_method_output_stream_subscribe(
        &mut self,
        _g: &mut Guest,
        _stream: i32,
    ) -> i32 {
        self.subscribe(Pollable::Ready)
    }

    // optional: wasi_io_streams_method_input_stream_read
    /// `result<list<u8>, stream-error>` at `ret`: the tag, then the list or, for the
    /// `closed` error at end of input (or a read error), the error's tag byte, both
    /// at `ret + 4`. Stdin never blocks: with nothing buffered yet the list is empty.
    /// A file is read at once.
    fn wasi_io_streams_method_input_stream_read(
        &mut self,
        g: &mut Guest,
        stream: i32,
        len: i64,
        ret: i32,
    ) {
        use std::io::Read;
        // `len` is a `u64`: a negative `i64` is a huge length, and 0 is an empty list.
        let want = (len as u64).min(4096) as usize;
        let mut buf = vec![0u8; want];
        // `None` is the end of input: `stream-error::closed`.
        let read = if want == 0 {
            Some(0)
        } else if stream == 0 {
            self.start_stdin();
            let mut shared = self.stdin.0.lock().unwrap();
            let n = shared.bytes.read(&mut buf).unwrap(); // a `VecDeque` never fails
            (n > 0 || !shared.eof).then_some(n)
        } else {
            let file = self.file_stream(stream).expect("read from an unknown or dropped stream");
            file.read(&mut buf).ok().filter(|&n| n > 0)
        };
        let Some(n) = read else {
            put(g, ret, &[1]); // result::err
            put(g, ret + 4, &[1]); // stream-error::closed
            return;
        };
        zero_tag(g, ret);
        let ptr = if n == 0 { 4 } else { alloc(g, 1, n as i32) };
        put(g, ptr, &buf[..n]);
        put_u32(g, ret + 4, ptr);
        put_u32(g, ret + 8, n as i32);
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_subscribe
    /// Ready when stdin has data or is at its end; a file is always ready.
    fn wasi_io_streams_method_input_stream_subscribe(&mut self, _g: &mut Guest, stream: i32) -> i32 {
        if stream != 0 {
            return self.subscribe(Pollable::Ready);
        }
        self.start_stdin();
        self.subscribe(Pollable::Stdin)
    }
    // end optional

    fn wasi_cli_stdin_get_stdin(&mut self, _g: &mut Guest) -> i32 {
        0
    }
    fn wasi_cli_stdout_get_stdout(&mut self, _g: &mut Guest) -> i32 {
        1
    }
    fn wasi_cli_stderr_get_stderr(&mut self, _g: &mut Guest) -> i32 {
        2
    }

    /// `option<terminal>` at `ret`: `none`, the program is not attached to one.
    fn wasi_cli_terminal_stdin_get_terminal_stdin(&mut self, g: &mut Guest, ret: i32) {
        zero_tag(g, ret);
    }
    fn wasi_cli_terminal_stdout_get_terminal_stdout(&mut self, g: &mut Guest, ret: i32) {
        zero_tag(g, ret);
    }
    fn wasi_cli_terminal_stderr_get_terminal_stderr(&mut self, g: &mut Guest, ret: i32) {
        zero_tag(g, ret);
    }

    fn wasi_clocks_monotonic_clock_subscribe_duration(
        &mut self,
        _g: &mut Guest,
        when: i64,
    ) -> i32 {
        let deadline = Instant::now() + Duration::from_nanos(when as u64);
        self.subscribe(Pollable::Deadline(deadline))
    }

    // optional: wasi_clocks_monotonic_clock_subscribe_instant
    fn wasi_clocks_monotonic_clock_subscribe_instant(&mut self, _g: &mut Guest, when: i64) -> i32 {
        let deadline = self.start + Duration::from_nanos(when as u64);
        self.subscribe(Pollable::Deadline(deadline))
    }
    // end optional

    // optional: wasi_clocks_monotonic_clock_now
    /// Nanoseconds since the host was created.
    fn wasi_clocks_monotonic_clock_now(&mut self, _g: &mut Guest) -> i64 {
        self.start.elapsed().as_nanos() as i64
    }
    // end optional

    // optional: wasi_clocks_wall_clock_now
    /// `datetime` at `ret`: seconds (`u64`), then nanoseconds (`u32`), since the epoch.
    fn wasi_clocks_wall_clock_now(&mut self, g: &mut Guest, ret: i32) {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        put(g, ret, &since.as_secs().to_le_bytes());
        put_u32(g, ret + 8, since.subsec_nanos() as i32);
    }
    // end optional

    // optional: wasi_filesystem_types_resource_drop_descriptor
    fn wasi_filesystem_types_resource_drop_descriptor(&mut self, _g: &mut Guest, handle: i32) {
        if handle != 0 {
            // the preopen (slot 0) is never freed
            self.descriptors.remove(handle as u32 as usize);
        }
    }
    // end optional

    // optional: wasi_filesystem_types_resource_drop_directory_entry_stream
    fn wasi_filesystem_types_resource_drop_directory_entry_stream(
        &mut self,
        _g: &mut Guest,
        handle: i32,
    ) {
        self.listings.remove(handle as u32 as usize);
    }
    // end optional

    // optional: wasi_filesystem_preopens_get_directories
    /// `list<tuple<descriptor, string>>` at `ret`: 12-byte `(handle, ptr, len)`
    /// elements; the preopen, named "/", if there is one.
    fn wasi_filesystem_preopens_get_directories(&mut self, g: &mut Guest, ret: i32) {
        let count = i32::from(self.root.is_some());
        let items = guest_list(g, ret, 12, count);
        if self.root.is_some() {
            put_u32(g, items, 0);
            put_string(g, items + 4, "/");
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_open_at
    /// `open-at(path-flags, path, open-flags, descriptor-flags)` -> `result<descriptor,
    /// error-code>`. Path flags are ignored (links are always followed, within the
    /// preopen). Creating, truncating and writing are `read-only` unless the preopen is
    /// writable; then `create`, `exclusive` and `truncate` act at once, and the
    /// descriptor remembers whether it was opened for `write`.
    fn wasi_filesystem_types_method_descriptor_open_at(
        &mut self,
        g: &mut Guest,
        dir: i32,
        _path_flags: i32,
        path_ptr: i32,
        path_len: i32,
        open_flags: i32,
        descriptor_flags: i32,
        ret: i32,
    ) {
        const OPEN_CREATE: i32 = 1;
        const OPEN_DIRECTORY: i32 = 2;
        const OPEN_EXCLUSIVE: i32 = 4;
        const OPEN_TRUNCATE: i32 = 8;
        const DESCRIPTOR_WRITE: i32 = 2;
        const DESCRIPTOR_MUTATE_DIRECTORY: i32 = 32;
        let (create, exclusive, truncate) =
            (open_flags & OPEN_CREATE != 0, open_flags & OPEN_EXCLUSIVE != 0, open_flags & OPEN_TRUNCATE != 0);
        let writes = descriptor_flags & DESCRIPTOR_WRITE != 0;
        let changes = create || exclusive || truncate;
        if !self.writable && (changes || writes || descriptor_flags & DESCRIPTOR_MUTATE_DIRECTORY != 0) {
            return fs_error(g, ret, 4, code::READ_ONLY);
        }
        if open_flags & OPEN_DIRECTORY != 0 && (changes || writes) {
            return fs_error(g, ret, 4, code::INVALID);
        }
        let Some(path) = get_str(g, path_ptr, path_len) else {
            return fs_error(g, ret, 4, code::IO);
        };
        let full = if create { self.resolve_to_create(dir, &path) } else { self.resolve(dir, &path) };
        let full = match full {
            Ok(full) if open_flags & OPEN_DIRECTORY != 0 && !full.is_dir() => {
                return fs_error(g, ret, 4, code::NOT_DIRECTORY);
            }
            Ok(full) if full.is_dir() && writes => return fs_error(g, ret, 4, code::IS_DIRECTORY),
            Ok(full) if create && exclusive && full.exists() => {
                return fs_error(g, ret, 4, code::EXIST);
            }
            Ok(full) => full,
            Err(code) => return fs_error(g, ret, 4, code),
        };
        if create || truncate {
            let created = std::fs::OpenOptions::new().write(true).create(create).truncate(truncate).open(&full);
            if let Err(e) = created {
                return fs_error(g, ret, 4, io_error_code(e));
            }
        }
        let handle = self.descriptors.insert(Descriptor { path: full, write: writes });
        fs_ok_handle(g, ret, handle);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_via_stream
    /// `read-via-stream(offset)` -> `result<input-stream, error-code>`: a new stream
    /// over the file, starting at `offset`.
    fn wasi_filesystem_types_method_descriptor_read_via_stream(
        &mut self,
        g: &mut Guest,
        descriptor: i32,
        offset: i64,
        ret: i32,
    ) {
        use std::io::{Seek, SeekFrom};
        let path = match self.descriptor(descriptor) {
            Ok(path) => path,
            Err(c) => return fs_error(g, ret, 4, c),
        };
        if path.is_dir() {
            return fs_error(g, ret, 4, code::IS_DIRECTORY);
        }
        let opened = std::fs::File::open(path).and_then(|mut file| {
            file.seek(SeekFrom::Start(offset as u64))?;
            Ok(file)
        });
        match opened {
            Ok(file) => {
                let slot = self.files.insert(file);
                fs_ok_handle(g, ret, FIRST_FILE_STREAM + slot);
            }
            Err(e) => fs_error(g, ret, 4, io_error_code(e)),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_write_via_stream
    /// `write-via-stream(offset)` -> `result<output-stream, error-code>`: a new stream
    /// writing into the file from `offset`. `read-only` unless the preopen is writable;
    /// the descriptor must have been opened for `write`.
    fn wasi_filesystem_types_method_descriptor_write_via_stream(
        &mut self,
        g: &mut Guest,
        descriptor: i32,
        offset: i64,
        ret: i32,
    ) {
        use std::io::{Seek, SeekFrom};
        let opened = self.writable_file(descriptor, false).and_then(|mut file| {
            file.seek(SeekFrom::Start(offset as u64)).map_err(io_error_code)?;
            Ok(file)
        });
        self.put_output_stream(g, ret, opened);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_append_via_stream
    /// `append-via-stream` -> `result<output-stream, error-code>`: every write goes to
    /// the end of the file.
    fn wasi_filesystem_types_method_descriptor_append_via_stream(
        &mut self,
        g: &mut Guest,
        descriptor: i32,
        ret: i32,
    ) {
        let opened = self.writable_file(descriptor, true);
        self.put_output_stream(g, ret, opened);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_create_directory_at
    /// `create-directory-at(path)` -> `result<_, error-code>`: `exist` if it is there.
    /// This and the three below change the preopen: `read-only` unless it is writable,
    /// and a symlink is never followed to its target.
    fn wasi_filesystem_types_method_descriptor_create_directory_at(
        &mut self,
        g: &mut Guest,
        dir: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        let done = self
            .require_writable()
            .and_then(|()| self.entry_at(g, dir, ptr, len))
            .and_then(|entry| std::fs::create_dir(entry).map_err(io_error_code));
        put_status(g, ret, done);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_unlink_file_at
    /// `unlink-file-at(path)`: removes a file or a symlink (the link, not its target);
    /// a directory is `is-directory`.
    fn wasi_filesystem_types_method_descriptor_unlink_file_at(
        &mut self,
        g: &mut Guest,
        dir: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        let done = self.require_writable().and_then(|()| self.entry_at(g, dir, ptr, len)).and_then(|entry| {
            if entry.symlink_metadata().map_err(io_error_code)?.is_dir() {
                return Err(code::IS_DIRECTORY);
            }
            std::fs::remove_file(entry).map_err(io_error_code)
        });
        put_status(g, ret, done);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_remove_directory_at
    /// `remove-directory-at(path)`: an empty directory only (`not-empty`); anything else,
    /// a symlink to a directory included, is `not-directory`.
    fn wasi_filesystem_types_method_descriptor_remove_directory_at(
        &mut self,
        g: &mut Guest,
        dir: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        let done = self.require_writable().and_then(|()| self.entry_at(g, dir, ptr, len)).and_then(|entry| {
            if !entry.symlink_metadata().map_err(io_error_code)?.is_dir() {
                return Err(code::NOT_DIRECTORY);
            }
            std::fs::remove_dir(entry).map_err(io_error_code)
        });
        put_status(g, ret, done);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_rename_at
    /// `rename-at(old-path, new-descriptor, new-path)`: both names are checked like any
    /// other entry; an existing target file is replaced. A symlink cannot be renamed
    /// (`not-permitted`).
    fn wasi_filesystem_types_method_descriptor_rename_at(
        &mut self,
        g: &mut Guest,
        dir: i32,
        old_ptr: i32,
        old_len: i32,
        new_dir: i32,
        new_ptr: i32,
        new_len: i32,
        ret: i32,
    ) {
        let done = self.require_writable().and_then(|()| {
            let old = self.entry_at(g, dir, old_ptr, old_len)?;
            let new = self.entry_at(g, new_dir, new_ptr, new_len)?;
            // Not a symlink: a descriptor's path is checked once, when it is opened, and
            // stays valid because nothing a guest can do puts a symlink under a path it
            // holds. Moving a symlink there would lead the descriptor out of the preopen.
            if old.symlink_metadata().map_err(io_error_code)?.file_type().is_symlink() {
                return Err(code::NOT_PERMITTED);
            }
            std::fs::rename(old, new).map_err(io_error_code)
        });
        put_status(g, ret, done);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_set_size
    /// `set-size(size)` -> `result<_, error-code>`: truncates or zero-extends the file.
    /// Like writing, it needs a writable preopen and a descriptor opened for `write`.
    fn wasi_filesystem_types_method_descriptor_set_size(
        &mut self,
        g: &mut Guest,
        descriptor: i32,
        size: i64,
        ret: i32,
    ) {
        let resized = self
            .writable_file(descriptor, false)
            .and_then(|file| file.set_len(size as u64).map_err(io_error_code));
        put_status(g, ret, resized);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_sync
    /// `sync` -> `result<_, error-code>`: flushes the file or directory to disk. It
    /// changes nothing, so a read-only preopen allows it.
    fn wasi_filesystem_types_method_descriptor_sync(&mut self, g: &mut Guest, d: i32, ret: i32) {
        let synced = self.sync_descriptor(d, false);
        put_status(g, ret, synced);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_sync_data
    /// `sync-data` -> `result<_, error-code>`: like `sync`, without the metadata.
    fn wasi_filesystem_types_method_descriptor_sync_data(&mut self, g: &mut Guest, d: i32, ret: i32) {
        let synced = self.sync_descriptor(d, true);
        put_status(g, ret, synced);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_set_times
    /// `set-times(access, modification)` -> `result<_, error-code>`. Each time is a
    /// `new-timestamp`, flattened to a tag (`no-change` 0, `now` 1, `timestamp` 2), the
    /// seconds and the nanoseconds. Needs a writable preopen; works on files and directories.
    fn wasi_filesystem_types_method_descriptor_set_times(
        &mut self,
        g: &mut Guest,
        descriptor: i32,
        access_tag: i32,
        access_seconds: i64,
        access_nanos: i32,
        modification_tag: i32,
        modification_seconds: i64,
        modification_nanos: i32,
        ret: i32,
    ) {
        let access = (access_tag, access_seconds, access_nanos);
        let modification = (modification_tag, modification_seconds, modification_nanos);
        let touched = self.touch(descriptor, access, modification);
        put_status(g, ret, touched);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_get_flags
    /// `result<descriptor-flags, error-code>`: the flags are one byte, at `ret + 1`;
    /// every descriptor is `read` (1), plus `write` (2) if it was opened for writing.
    fn wasi_filesystem_types_method_descriptor_get_flags(&mut self, g: &mut Guest, d: i32, ret: i32) {
        match self.open_descriptor(d) {
            Ok(descriptor) => {
                zero_tag(g, ret);
                put(g, ret + 1, &[1 | if descriptor.write { 2 } else { 0 }]);
            }
            Err(c) => fs_error(g, ret, 1, c),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_directory
    /// `read-directory` -> `result<directory-entry-stream, error-code>`: a snapshot of
    /// the entries (without `.` and `..`), in the OS's order.
    fn wasi_filesystem_types_method_descriptor_read_directory(&mut self, g: &mut Guest, d: i32, ret: i32) {
        let path = match self.descriptor(d) {
            Ok(path) => path,
            Err(c) => return fs_error(g, ret, 4, c),
        };
        if !path.is_dir() {
            return fs_error(g, ret, 4, code::NOT_DIRECTORY);
        }
        let entries = std::fs::read_dir(path).and_then(|dir| {
            dir.map(|entry| {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                Ok((descriptor_type(entry.file_type()?), name))
            })
            .collect::<std::io::Result<Vec<_>>>()
        });
        match entries {
            Ok(entries) => {
                let handle = self.listings.insert(entries.into_iter());
                fs_ok_handle(g, ret, handle);
            }
            Err(e) => fs_error(g, ret, 4, io_error_code(e)),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_stat
    fn wasi_filesystem_types_method_descriptor_stat(&mut self, g: &mut Guest, d: i32, ret: i32) {
        match self.descriptor(d) {
            Ok(path) => put_stat(g, ret, path),
            Err(c) => fs_error(g, ret, 8, c),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_stat_at
    /// Like `stat`, for a path under the descriptor (links are followed).
    fn wasi_filesystem_types_method_descriptor_stat_at(
        &mut self,
        g: &mut Guest,
        d: i32,
        _path_flags: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        match self.resolve_at(g, d, ptr, len) {
            Ok(full) => put_stat(g, ret, &full),
            Err(c) => fs_error(g, ret, 8, c),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_metadata_hash
    fn wasi_filesystem_types_method_descriptor_metadata_hash(&mut self, g: &mut Guest, d: i32, ret: i32) {
        match self.descriptor(d) {
            Ok(path) => put_metadata_hash(g, ret, path),
            Err(c) => fs_error(g, ret, 8, c),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_metadata_hash_at
    fn wasi_filesystem_types_method_descriptor_metadata_hash_at(
        &mut self,
        g: &mut Guest,
        d: i32,
        _path_flags: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        match self.resolve_at(g, d, ptr, len) {
            Ok(full) => put_metadata_hash(g, ret, &full),
            Err(c) => fs_error(g, ret, 8, c),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_directory_entry_stream_read_directory_entry
    /// `read-directory-entry` -> `result<option<directory-entry>, error-code>`: the
    /// option's tag at `ret + 4`, then the entry: its type byte at `ret + 8` and its
    /// name `(ptr, len)` at `ret + 12`. `none` once the listing is exhausted.
    fn wasi_filesystem_types_method_directory_entry_stream_read_directory_entry(
        &mut self,
        g: &mut Guest,
        stream: i32,
        ret: i32,
    ) {
        let Some(listing) = self.listings.get_mut(stream as u32 as usize) else {
            return fs_error(g, ret, 4, code::BAD_DESCRIPTOR);
        };
        zero_tag(g, ret);
        match listing.next() {
            Some((kind, name)) => {
                put(g, ret + 4, &[1]);
                put(g, ret + 8, &[kind]);
                put_string(g, ret + 12, &name);
            }
            None => zero_tag(g, ret + 4),
        }
    }
    // end optional

    // optional: wasi_random_random_get_random_bytes
    fn wasi_random_random_get_random_bytes(&mut self, g: &mut Guest, len: i64, ret: i32) {
        self.random_bytes(g, len, ret);
    }
    // end optional

    // optional: wasi_random_random_get_random_u64
    fn wasi_random_random_get_random_u64(&mut self, _g: &mut Guest) -> i64 {
        self.next_u64() as i64
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_bytes
    fn wasi_random_insecure_get_insecure_random_bytes(
        &mut self,
        g: &mut Guest,
        len: i64,
        ret: i32,
    ) {
        self.random_bytes(g, len, ret);
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_u64
    fn wasi_random_insecure_get_insecure_random_u64(&mut self, _g: &mut Guest) -> i64 {
        self.next_u64() as i64
    }
    // end optional

    // optional: wasi_random_insecure_seed_insecure_seed
    /// `tuple<u64, u64>` at `ret`.
    fn wasi_random_insecure_seed_insecure_seed(&mut self, g: &mut Guest, ret: i32) {
        let (a, b) = (self.next_u64(), self.next_u64());
        put(g, ret, &a.to_le_bytes());
        put(g, ret + 8, &b.to_le_bytes());
    }
    // end optional
}
