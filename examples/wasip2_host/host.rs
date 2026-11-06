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
// is an index into `pollables`: `None` is always ready (streams), `Some(deadline)`
// becomes ready at the deadline (clocks); `poll` and `block` sleep until then.
// `read` blocks on the real stdin, so an input stream is always ready too.
// Random numbers come from a small non-cryptographic generator (splitmix64) seeded
// from the OS-seeded `std` hasher: fine for `HashMap` seeds and examples, not for
// keys or tokens.
// Files: a single preopened directory (`with_preopen`, shown to the guest as "/"),
// read-only. A descriptor is an index into `descriptors` (the preopen is 0), a file's
// input stream is `FIRST_FILE_STREAM + index` into `files`. Paths are resolved against the preopen and
// must stay under it: absolute paths and `..` are refused (`not-permitted`), and so are
// symlinks that lead out. Anything that would write is refused with `read-only`.
// `stat`, `get-flags`, `metadata-hash` and directory listings work on the same paths.
// Not implemented: sockets.

use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

struct P2Host {
    /// Read only by the optional `get-arguments` block.
    #[allow(dead_code)]
    args: Vec<String>,
    env: Vec<(String, String)>,
    pollables: Vec<Option<Instant>>,
    /// The monotonic clock's zero; read only by the optional clock blocks.
    #[allow(dead_code)]
    start: Instant,
    /// The generator state; read only by the optional `wasi:random` blocks.
    #[allow(dead_code)]
    rng: u64,
    /// The preopened directory (canonical); read only by the optional filesystem blocks.
    #[allow(dead_code)]
    root: Option<PathBuf>,
    /// Paths behind descriptor handles (the preopen first), all under `root`; never freed.
    #[allow(dead_code)]
    descriptors: Vec<PathBuf>,
    /// Open files behind input-stream handles `3..`; `None` once dropped.
    #[allow(dead_code)]
    files: Vec<Option<std::fs::File>>,
    /// Directory listings behind `directory-entry-stream` handles: the entries
    /// (type, name) still to be read; `None` once dropped.
    #[allow(dead_code)]
    listings: Vec<Option<std::vec::IntoIter<(u8, String)>>>,
}

type Guest = Instance<P2Host>;

impl P2Host {
    fn new(args: Vec<String>, env: Vec<(String, String)>) -> Self {
        P2Host { args, env, pollables: Vec::new(), start: Instant::now(),
            rng: os_seed(),
            root: None,
            descriptors: Vec::new(),
            files: Vec::new(),
            listings: Vec::new(),
        }
    }

    /// Gives the guest the directory `root` (as its "/"), read-only.
    #[allow(dead_code)]
    fn with_preopen(mut self, root: PathBuf) -> Self {
        let root = root.canonicalize().expect("preopen: directory does not exist");
        self.descriptors = vec![root.clone()];
        self.root = Some(root);
        self
    }

    /// The slot of a file's input stream (`None` for stdin, stdout, stderr or an
    /// unknown handle).
    #[allow(dead_code)]
    fn file_stream(&mut self, stream: i32) -> Option<&mut Option<std::fs::File>> {
        let index = (stream as u32 as usize).checked_sub(FIRST_FILE_STREAM)?;
        self.files.get_mut(index)
    }

    /// The path behind a descriptor handle.
    #[allow(dead_code)]
    fn descriptor(&self, descriptor: i32) -> Option<&Path> {
        self.descriptors.get(descriptor as u32 as usize).map(PathBuf::as_path)
    }

    /// The path `path` names under the directory behind descriptor `dir`, or the
    /// `error-code` to return: `not-permitted` outside the preopen.
    #[allow(dead_code)]
    fn resolve(&self, dir: i32, path: &str) -> Result<PathBuf, u8> {
        let root = self.root.as_ref().ok_or(code::NOT_PERMITTED)?;
        let base = self.descriptors.get(dir as u32 as usize).ok_or(code::BAD_DESCRIPTOR)?;
        let relative = Path::new(path);
        if relative.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) {
            return Err(code::NOT_PERMITTED);
        }
        let full = base.join(relative).canonicalize().map_err(io_error_code)?;
        if full.starts_with(root) { Ok(full) } else { Err(code::NOT_PERMITTED) }
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

    /// A new pollable that is ready at `deadline` (always, for `None`).
    fn subscribe(&mut self, deadline: Option<Instant>) -> i32 {
        self.pollables.push(deadline);
        self.pollables.len() as i32 - 1
    }

    /// The pollable's deadline: `None` if it is always ready.
    fn deadline(&self, pollable: u32) -> Option<Instant> {
        self.pollables[pollable as usize]
    }

    /// Sleeps until the pollable is ready.
    fn block_on(&self, pollable: i32) {
        if let Some(deadline) = self.deadline(pollable as u32) {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

/// The handle of the first file's input stream (0 to 2 are stdin, stdout, stderr).
#[allow(dead_code)]
const FIRST_FILE_STREAM: usize = 3;

/// `wasi:filesystem` error codes; which ones a program's host uses depends on the
/// optional blocks kept.
#[allow(dead_code)]
mod code {
    pub const ACCESS: u8 = 0;
    pub const BAD_DESCRIPTOR: u8 = 3;
    pub const EXIST: u8 = 7;
    pub const IO: u8 = 13;
    pub const IS_DIRECTORY: u8 = 14;
    pub const NO_ENTRY: u8 = 20;
    pub const NOT_DIRECTORY: u8 = 24;
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
        None => put(g, at, &[0]),
    }
}

/// The `descriptor-type` of a file type: directory 3, regular file 6, symlink 5.
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
    let links = 1;
    zero_tag(g, ret);
    put(g, ret + 8, &[descriptor_type(meta.file_type())]);
    put(g, ret + 16, &links.to_le_bytes());
    put(g, ret + 24, &meta.len().to_le_bytes());
    put_datetime_option(g, ret + 32, meta.accessed());
    put_datetime_option(g, ret + 56, meta.modified());
    put(g, ret + 80, &[0]);
}

/// `result<metadata-hash-value, error-code>` at `ret`: two `u64`s at `ret + 8`, a
/// hash of the file's canonical path (the same file always gives the same value).
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
    fn wasi_io_poll_resource_drop_pollable(&mut self, _g: &mut Guest, _handle: i32) {}
    fn wasi_io_streams_resource_drop_input_stream(&mut self, _g: &mut Guest, handle: i32) {
        if let Some(file) = self.file_stream(handle) {
            *file = None;
        }
    }
    fn wasi_io_streams_resource_drop_output_stream(&mut self, _g: &mut Guest, _handle: i32) {}
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
        self.block_on(pollable);
    }

    /// `poll(in: list<borrow<pollable>>) -> list<u32>`: the indices of the ready
    /// pollables, sleeping until the earliest deadline if none is ready yet. An empty
    /// list traps, as the interface specifies.
    fn wasi_io_poll_poll(&mut self, g: &mut Guest, list: i32, len: i32, ret: i32) {
        assert!(len > 0, "wasi:io/poll.poll: empty list");
        let handles: Vec<u32> = (0..len).map(|i| get_u32(g, list + 4 * i)).collect();
        let ready = loop {
            let now = Instant::now();
            let ready: Vec<i32> = (0..len)
                .filter(|&i| self.deadline(handles[i as usize]).is_none_or(|d| d <= now))
                .collect();
            if !ready.is_empty() {
                break ready;
            }
            let earliest = handles.iter().filter_map(|&h| self.deadline(h)).min().unwrap();
            std::thread::sleep(earliest.saturating_duration_since(now));
        };
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
        stream_write(g, stream, ptr, len);
        zero_tag(g, ret);
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
        self.subscribe(None)
    }

    // optional: wasi_io_streams_method_input_stream_read
    /// `result<list<u8>, stream-error>` at `ret`: the tag, then the list or, for the
    /// `closed` error at end of input (or a read error), the error's tag byte, both
    /// at `ret + 4`. It blocks until at least one byte (or end of input) is available.
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
        let n = if want == 0 {
            0
        } else {
            let read = match stream {
                0 => std::io::stdin().read(&mut buf),
                _ => self
                    .file_stream(stream)
                    .and_then(Option::as_mut)
                    .expect("read from an unknown or dropped stream")
                    .read(&mut buf),
            };
            match read {
                Ok(0) | Err(_) => {
                    put(g, ret, &[1]); // result::err
                    put(g, ret + 4, &[1]); // stream-error::closed
                    return;
                }
                Ok(n) => n,
            }
        };
        zero_tag(g, ret);
        let ptr = if n == 0 { 4 } else { alloc(g, 1, n as i32) };
        put(g, ptr, &buf[..n]);
        put_u32(g, ret + 4, ptr);
        put_u32(g, ret + 8, n as i32);
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_subscribe
    fn wasi_io_streams_method_input_stream_subscribe(&mut self, _g: &mut Guest, _stream: i32) -> i32 {
        self.subscribe(None)
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
        self.subscribe(Some(deadline))
    }

    // optional: wasi_clocks_monotonic_clock_subscribe_instant
    fn wasi_clocks_monotonic_clock_subscribe_instant(&mut self, _g: &mut Guest, when: i64) -> i32 {
        let deadline = self.start + Duration::from_nanos(when as u64);
        self.subscribe(Some(deadline))
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
    fn wasi_filesystem_types_resource_drop_descriptor(&mut self, _g: &mut Guest, _handle: i32) {}
    // end optional

    // optional: wasi_filesystem_types_resource_drop_directory_entry_stream
    fn wasi_filesystem_types_resource_drop_directory_entry_stream(
        &mut self,
        _g: &mut Guest,
        handle: i32,
    ) {
        if let Some(listing) = self.listings.get_mut(handle as u32 as usize) {
            *listing = None;
        }
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
    /// preopen); creating, truncating and writing are `read-only`.
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
        const OPEN_CREATE_EXCLUSIVE_TRUNCATE: i32 = 1 | 4 | 8;
        const DESCRIPTOR_WRITE_MUTATE_DIRECTORY: i32 = 2 | 32;
        const OPEN_DIRECTORY: i32 = 2;
        if open_flags & OPEN_CREATE_EXCLUSIVE_TRUNCATE != 0 || descriptor_flags & DESCRIPTOR_WRITE_MUTATE_DIRECTORY != 0 {
            return fs_error(g, ret, 4, code::READ_ONLY);
        }
        let Some(path) = get_str(g, path_ptr, path_len) else {
            return fs_error(g, ret, 4, code::IO);
        };
        match self.resolve(dir, &path) {
            Ok(full) if open_flags & OPEN_DIRECTORY != 0 && !full.is_dir() => {
                fs_error(g, ret, 4, code::NOT_DIRECTORY)
            }
            Ok(full) => {
                self.descriptors.push(full);
                fs_ok_handle(g, ret, self.descriptors.len() - 1);
            }
            Err(code) => fs_error(g, ret, 4, code),
        }
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
        let Some(path) = self.descriptors.get(descriptor as u32 as usize) else {
            return fs_error(g, ret, 4, code::BAD_DESCRIPTOR);
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
                self.files.push(Some(file));
                fs_ok_handle(g, ret, FIRST_FILE_STREAM + self.files.len() - 1);
            }
            Err(e) => fs_error(g, ret, 4, io_error_code(e)),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_write_via_stream
    fn wasi_filesystem_types_method_descriptor_write_via_stream(
        &mut self,
        g: &mut Guest,
        _descriptor: i32,
        _offset: i64,
        ret: i32,
    ) {
        fs_error(g, ret, 4, code::READ_ONLY);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_append_via_stream
    fn wasi_filesystem_types_method_descriptor_append_via_stream(
        &mut self,
        g: &mut Guest,
        _descriptor: i32,
        ret: i32,
    ) {
        fs_error(g, ret, 4, code::READ_ONLY);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_get_flags
    /// `result<descriptor-flags, error-code>`: the flags are one byte, at `ret + 1`;
    /// every descriptor is `read` (1).
    fn wasi_filesystem_types_method_descriptor_get_flags(&mut self, g: &mut Guest, _d: i32, ret: i32) {
        zero_tag(g, ret);
        put(g, ret + 1, &[1]);
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_directory
    /// `read-directory` -> `result<directory-entry-stream, error-code>`: a snapshot of
    /// the entries (without `.` and `..`), in the OS's order.
    fn wasi_filesystem_types_method_descriptor_read_directory(&mut self, g: &mut Guest, d: i32, ret: i32) {
        let Some(path) = self.descriptor(d) else {
            return fs_error(g, ret, 4, code::BAD_DESCRIPTOR);
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
                self.listings.push(Some(entries.into_iter()));
                fs_ok_handle(g, ret, self.listings.len() - 1);
            }
            Err(e) => fs_error(g, ret, 4, io_error_code(e)),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_stat
    fn wasi_filesystem_types_method_descriptor_stat(&mut self, g: &mut Guest, d: i32, ret: i32) {
        match self.descriptor(d) {
            Some(path) => put_stat(g, ret, &path.to_path_buf()),
            None => fs_error(g, ret, 8, code::BAD_DESCRIPTOR),
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
        match get_str(g, ptr, len).ok_or(code::IO).and_then(|path| self.resolve(d, &path)) {
            Ok(full) => put_stat(g, ret, &full),
            Err(c) => fs_error(g, ret, 8, c),
        }
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_metadata_hash
    fn wasi_filesystem_types_method_descriptor_metadata_hash(&mut self, g: &mut Guest, d: i32, ret: i32) {
        match self.descriptor(d) {
            Some(path) => put_metadata_hash(g, ret, &path.to_path_buf()),
            None => fs_error(g, ret, 8, code::BAD_DESCRIPTOR),
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
        match get_str(g, ptr, len).ok_or(code::IO).and_then(|path| self.resolve(d, &path)) {
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
        let Some(Some(listing)) = self.listings.get_mut(stream as u32 as usize) else {
            return fs_error(g, ret, 4, code::BAD_DESCRIPTOR);
        };
        let next = listing.next();
        zero_tag(g, ret);
        match next {
            Some((kind, name)) => {
                put(g, ret + 4, &[1]);
                put(g, ret + 8, &[kind]);
                put_string(g, ret + 12, &name);
            }
            None => put(g, ret + 4, &[0]),
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
