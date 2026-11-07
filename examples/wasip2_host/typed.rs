// A WASI preview2 host written against the typed `Host` trait that
// `wasm2rs --wit-host --glue` generates from a component. It has no canonical-ABI
// code: strings, lists, results and resources arrive as Rust values, and the generated
// `Glue` lowers them to the module's flat `Imports`. It is source text appended to the
// generated code (the `Host` trait and its types come from the component), covering
// the interfaces a `println!` program uses: stdout, the environment, exit, output
// streams and pollables, plus stdin (always at its end), stderr and the terminal
// queries (not a terminal), which a std program asks for at startup. Handles: 0 = stdin,
// 1 = stdout, 2 = stderr; a pollable is an index into `pollables`: ready at once, or
// at the deadline a clock subscription gave it. It also serves `wasi:random` and, read-only,
// `wasi:filesystem` (`with_preopen`): `preopens.get-directories`, `descriptor.open-at`,
// `read-via-stream`, `stat`, `stat-at`, `get-flags`, `read-directory`, `readlink-at`,
// `metadata-hash` (and `-at`), `directory-entry-stream.read-directory-entry` and file
// `input-stream.read` (`blocking-read` and `skip` are not implemented), with the semantics
// of `host.rs`: one preopened directory shown as "/", descriptors and streams in slabs
// (the preopen is descriptor 0 and never freed; a file's input stream is
// `FIRST_FILE_STREAM + slot`, clear of the stdio handles), paths resolved under the
// preopen (absolute paths, `..` and symlinks that lead out are `not-permitted`), writes
// refused with `read-only`.
//
// Like `host.rs`, each method sits between `// optional: <method>` and `// end optional`
// and is kept only when the generated trait declares it (see `keep_declared_methods` in
// `filter.rs`), so one source serves components that import different subsets. A
// component that imports a method this host lacks does not build.

#[allow(unused_imports)]
use std::io::Write;
#[allow(unused_imports)]
use std::path::{Component, Path, PathBuf};
#[allow(unused_imports)]
use std::time::{Duration, Instant};

#[allow(dead_code)]
fn os_seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(0u8)
}

/// Blocks until `deadline`.
#[allow(dead_code)]
fn sleep_until(deadline: Instant) {
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
}

#[allow(dead_code)]
struct TypedHost {
    env: Vec<(String, String)>,
    /// The generator state (splitmix64, seeded from the OS-seeded `std` hasher): fine for
    /// `HashMap` seeds and examples, not for keys or tokens.
    rng: u64,
    /// One entry per subscription: the instant it becomes ready (`None`: already
    /// ready). A dropped pollable keeps its slot.
    pollables: Vec<Option<Instant>>,
    /// The canonicalized preopened directory (`with_preopen`), shown to the guest as "/".
    root: Option<PathBuf>,
    /// Open descriptors; slot 0 is the preopen.
    descriptors: Slab<OpenDescriptor>,
    /// Streams behind the input-stream handles `FIRST_FILE_STREAM..`.
    files: Slab<FileStream>,
    /// Directory listings behind `directory-entry-stream` handles: the entries (type, name)
    /// still to be read.
    listings: Slab<std::vec::IntoIter<(std::fs::FileType, String)>>,
}

#[allow(dead_code)]
impl TypedHost {
    fn new(env: Vec<(String, String)>) -> Self {
        TypedHost {
            env,
            rng: os_seed(),
            pollables: Vec::new(),
            root: None,
            descriptors: Slab::new(),
            files: Slab::new(),
            listings: Slab::new(),
        }
    }

    /// Gives the guest the directory `root` (as its "/"), read-only.
    fn with_preopen(mut self, root: PathBuf) -> Self {
        let root = root.canonicalize().expect("preopen: directory does not exist");
        self.descriptors.insert(OpenDescriptor { path: root.clone(), file: None }); // slot 0
        self.root = Some(root);
        self
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
        let OpenDescriptor { path, file } = self.open_descriptor(descriptor)?;
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
        self.pollables.push(deadline);
        Pollable((self.pollables.len() - 1) as u32)
    }

    fn deadline(&self, pollable: Pollable) -> Option<Instant> {
        self.pollables.get(pollable.0 as usize).copied().flatten()
    }
    // end optional
}

/// An open descriptor: the preopen and other directories hold their path (resolved again
/// on each use), a file its open `File`.
#[allow(dead_code)]
struct OpenDescriptor {
    path: PathBuf,
    file: Option<std::fs::File>,
}

/// A stream over a duplicate of a descriptor's `File`. Duplicates share one file offset,
/// so each stream keeps its own position and seeks to it before every read.
#[allow(dead_code)]
struct FileStream {
    file: std::fs::File,
    pos: u64,
}

#[allow(dead_code)]
impl FileStream {
    fn over(file: &std::fs::File, pos: u64) -> Result<Self, Fs> {
        Ok(FileStream { file: file.try_clone().map_err(io_error_code)?, pos })
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(self.pos))?;
        let n = self.file.read(buf)?;
        self.pos += n as u64;
        Ok(n)
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

/// The handle of the first file's input stream (0 to 2 are stdin, stdout, stderr).
#[allow(dead_code)]
const FIRST_FILE_STREAM: u32 = 3;

/// The slot in `files` of an input-stream handle (`None` for stdin, stdout, stderr).
#[allow(dead_code)]
fn file_index(stream: u32) -> Option<usize> {
    stream.checked_sub(FIRST_FILE_STREAM).map(|i| i as usize)
}

/// The `error-code`s this host returns, without the generated type (which a component
/// that imports no filesystem interface does not have); `From<Fs> for ErrorCode` below.
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

// The next blocks use generated types, so each is kept only if the component imports one of
// the methods that need it.

// optional: wasi_filesystem_types_method_descriptor_open_at | wasi_filesystem_types_method_descriptor_read_via_stream | wasi_filesystem_types_method_descriptor_get_flags | wasi_filesystem_types_method_descriptor_read_directory | wasi_filesystem_types_method_descriptor_stat | wasi_filesystem_types_method_descriptor_stat_at | wasi_filesystem_types_method_descriptor_readlink_at | wasi_filesystem_types_method_descriptor_metadata_hash | wasi_filesystem_types_method_descriptor_metadata_hash_at | wasi_filesystem_types_method_directory_entry_stream_read_directory_entry
impl From<Fs> for ErrorCode {
    fn from(e: Fs) -> Self {
        match e {
            Fs::Access => ErrorCode::Access,
            Fs::BadDescriptor => ErrorCode::BadDescriptor,
            Fs::Exist => ErrorCode::Exist,
            Fs::Invalid => ErrorCode::Invalid,
            Fs::Io => ErrorCode::Io,
            Fs::IsDirectory => ErrorCode::IsDirectory,
            Fs::NoEntry => ErrorCode::NoEntry,
            Fs::NotDirectory => ErrorCode::NotDirectory,
            Fs::NotEmpty => ErrorCode::NotEmpty,
            Fs::Unsupported => ErrorCode::Unsupported,
            Fs::NotPermitted => ErrorCode::NotPermitted,
            Fs::ReadOnly => ErrorCode::ReadOnly,
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

impl Host for TypedHost {
    // optional: wasi_cli_stdout_get_stdout
    fn wasi_cli_stdout_get_stdout(&mut self) -> OutputStream {
        OutputStream(1)
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
        if let Some(index) = file_index(handle.0) {
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

    // optional: wasi_io_poll_poll
    /// The indices of the ready pollables, waiting for the earliest deadline if none
    /// is ready yet. An empty list traps, as the interface specifies.
    fn wasi_io_poll_poll(&mut self, in_: Vec<Pollable>) -> Vec<u32> {
        assert!(!in_.is_empty(), "wasi:io/poll.poll: empty list");
        // `None` as soon as one pollable is ready already: then nothing to wait for
        let pending = in_.iter().map(|p| self.deadline(*p)).collect::<Option<Vec<_>>>();
        if let Some(first) = pending.and_then(|d| d.into_iter().min()) {
            sleep_until(first);
        }
        let now = Instant::now();
        in_.iter()
            .enumerate()
            .filter(|(_, p)| self.deadline(**p).is_none_or(|d| d <= now))
            .map(|(i, _)| i as u32)
            .collect()
    }
    // end optional

    // optional: wasi_io_streams_resource_drop_output_stream
    fn wasi_io_streams_resource_drop_output_stream(&mut self, _handle: OutputStream) {}
    // end optional

    // optional: wasi_io_poll_method_pollable_block
    fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable) {
        if let Some(deadline) = self.deadline(self_) {
            sleep_until(deadline);
        }
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
        let written = match self_.0 {
            2 => std::io::stderr().write_all(&contents),
            _ => std::io::stdout().write_all(&contents),
        };
        written.map_err(|_| StreamError::Closed)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_flush
    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        _self_: OutputStream,
    ) -> Result<(), StreamError> {
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
    /// Path flags are ignored (links are always followed, within the preopen). The preopen
    /// is read-only: creating, truncating or writing is `read-only`.
    fn wasi_filesystem_types_method_descriptor_open_at(
        &mut self,
        self_: Descriptor,
        _path_flags: PathFlags,
        path: String,
        open_flags: OpenFlags,
        flags: DescriptorFlags,
    ) -> Result<Descriptor, ErrorCode> {
        let changes = open_flags.contains(OpenFlags::CREATE)
            || open_flags.contains(OpenFlags::EXCLUSIVE)
            || open_flags.contains(OpenFlags::TRUNCATE);
        if changes
            || flags.contains(DescriptorFlags::WRITE)
            || flags.contains(DescriptorFlags::MUTATE_DIRECTORY)
        {
            return Err(Fs::ReadOnly.into());
        }
        let full = self.resolve(self_.0, &path)?;
        let is_dir = full.is_dir();
        if open_flags.contains(OpenFlags::DIRECTORY) && !is_dir {
            return Err(Fs::NotDirectory.into());
        }
        let file = if is_dir {
            None
        } else {
            Some(std::fs::File::open(&full).map_err(io_error_code)?)
        };
        let slot = self.descriptors.insert(OpenDescriptor { path: full, file });
        Ok(Descriptor(slot as u32))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_via_stream
    /// A new stream over the file, starting at `offset`.
    fn wasi_filesystem_types_method_descriptor_read_via_stream(
        &mut self,
        self_: Descriptor,
        offset: u64,
    ) -> Result<InputStream, ErrorCode> {
        let file = self.open_descriptor(self_.0)?.file.as_ref().ok_or(Fs::IsDirectory)?;
        let stream = FileStream::over(file, offset)?;
        let slot = self.files.insert(stream);
        Ok(InputStream(FIRST_FILE_STREAM + slot as u32))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_readlink_at
    /// The target of a symlink, if it is relative and stays under the preopen.
    fn wasi_filesystem_types_method_descriptor_readlink_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<String, ErrorCode> {
        Ok(self.read_link_target(self_.0, &path)?)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_get_flags
    /// Every descriptor is `read` (writing is refused at `open-at`).
    fn wasi_filesystem_types_method_descriptor_get_flags(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorFlags, ErrorCode> {
        self.open_descriptor(self_.0)?;
        Ok(DescriptorFlags::READ)
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_read_directory
    fn wasi_filesystem_types_method_descriptor_read_directory(
        &mut self,
        self_: Descriptor,
    ) -> Result<DirectoryEntryStream, ErrorCode> {
        let entries = self.list_directory(self_.0)?;
        Ok(DirectoryEntryStream(self.listings.insert(entries.into_iter()) as u32))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_stat
    fn wasi_filesystem_types_method_descriptor_stat(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorStat, ErrorCode> {
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
    ) -> Result<DescriptorStat, ErrorCode> {
        let full = self.resolve(self_.0, &path)?;
        Ok(descriptor_stat(std::fs::metadata(full).map_err(io_error_code)?))
    }
    // end optional

    // optional: wasi_filesystem_types_method_descriptor_metadata_hash
    fn wasi_filesystem_types_method_descriptor_metadata_hash(
        &mut self,
        self_: Descriptor,
    ) -> Result<MetadataHashValue, ErrorCode> {
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
    ) -> Result<MetadataHashValue, ErrorCode> {
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
    ) -> Result<Option<DirectoryEntry>, ErrorCode> {
        let listing = self.listings.get_mut(self_.0 as usize).ok_or(Fs::BadDescriptor)?;
        Ok(listing.next().map(|(kind, name)| DirectoryEntry { type_: descriptor_type(kind), name }))
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_read
    /// Reads at most 4096 bytes of a file stream at once; `closed` at its end (and for
    /// stdin, which is always at its end). An unknown or dropped stream traps.
    fn wasi_io_streams_method_input_stream_read(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<Vec<u8>, StreamError> {
        let Some(index) = file_index(self_.0) else {
            return Err(StreamError::Closed);
        };
        let file = self.files.get_mut(index).expect("read from an unknown or dropped stream");
        let mut buf = vec![0u8; len.min(4096) as usize];
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
}
