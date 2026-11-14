//! Filesystem descriptors: preopens, open descriptors, file streams and metadata.

use super::*;

/// A preopened directory.
pub(super) struct Preopen {
    /// The name the guest sees.
    pub(super) guest_name: String,
    /// Whether the guest may change anything under it.
    pub(super) writable: bool,
}

/// An open descriptor: a directory (the preopens included) holds the open directory, a file
/// its open `File`. Every path under a directory is resolved below that handle, never from
/// a host path, so what a check saw is what is used.
pub(super) struct OpenDescriptor {
    /// Where the descriptor was opened from, as a label only: it names the object where the
    /// platform has no device and inode numbers.
    pub(super) path: PathBuf,
    /// The index in `preopens` of the preopen this descriptor lies under.
    pub(super) preopen: usize,
    /// Whether a file was opened for `write`.
    pub(super) write: bool,
    /// Whether a file was opened for reading.
    pub(super) read: bool,
    pub(super) file: Option<std::fs::File>,
    pub(super) dir: Option<Dir>,
}

/// A stream over a duplicate of a descriptor's `File`. Duplicates share one file offset,
/// so each stream keeps its own position and seeks to it before every read or write (the
/// host runs one guest call at a time, so nothing interleaves).
pub(super) struct FileStream {
    pub(super) file: std::fs::File,
    pub(super) pos: u64,
    /// Every write goes to the end of the file.
    pub(super) append: bool,
}

impl FileStream {
    /// A stream over a duplicate of `file`, from `pos`.
    pub(super) fn over(file: &std::fs::File, pos: u64, append: bool) -> Result<Self, Fs> {
        Ok(FileStream {
            file: file.try_clone().map_err(io_error_code)?,
            pos,
            append,
        })
    }

    pub(super) fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(self.pos))?;
        let n = self.file.read(buf)?;
        self.pos = self.pos.saturating_add(to_u64(n));
        Ok(n)
    }

    pub(super) fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom, Write};
        let at = if self.append {
            SeekFrom::End(0)
        } else {
            SeekFrom::Start(self.pos)
        };
        self.pos = self.file.seek(at)?;
        self.file.write_all(bytes)?;
        self.pos = self.pos.saturating_add(to_u64(bytes.len()));
        Ok(())
    }
}

/// The handle of the first file's stream, input or output (0 to 2 are stdin, stdout, stderr).
pub(super) const FIRST_FILE_STREAM: u32 = 3;

/// The slot in `files` (input streams) or `outputs` (output streams) of a stream handle
/// (`None` for stdin, stdout, stderr).
pub(super) fn file_index(stream: u32) -> Option<usize> {
    stream.checked_sub(FIRST_FILE_STREAM).map(idx)
}

/// The filesystem `error-code`s this host returns; `From<Fs> for FsErrorCode` below.
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub(super) enum Fs {
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

pub(super) fn io_error_code(e: std::io::Error) -> Fs {
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
pub(super) fn make_symlink(dir: &Dir, target: &str, link: &Path) -> std::io::Result<()> {
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
pub(super) fn sandbox_error(e: std::io::Error) -> Fs {
    if e.kind() == std::io::ErrorKind::PermissionDenied && e.raw_os_error().is_none() {
        Fs::NotPermitted
    } else {
        io_error_code(e)
    }
}

/// `path` as a path below a directory handle: `not-permitted` if it is absolute or has `..`
/// (such a path could not stay below the descriptor), and the empty path is the directory.
pub(super) fn below(path: &str) -> Result<&Path, Fs> {
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
pub(super) fn entry_path(path: &str) -> Result<&Path, Fs> {
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
pub(super) fn is_symlink(dir: &Dir, name: &Path) -> bool {
    dir.symlink_metadata(name)
        .is_ok_and(|meta| meta.is_symlink())
}

/// Whether `path` is relative and has no `..`, so it stays under the directory it is
/// joined to.
pub(super) fn stays_below(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
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
pub(super) fn names_an_entry(path: &str) -> bool {
    !(path.is_empty() || path == "." || path.ends_with('/') || path.ends_with("/."))
}

/// Directory 3, regular file 6, symlink 5 (seen only for a directory entry or a
/// `symlink_metadata` lookup), otherwise unknown.
pub(super) fn descriptor_type(t: &impl FileKind) -> DescriptorType {
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
pub(super) trait FileKind {
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
pub(super) struct Stat {
    pub(super) kind: DescriptorType,
    pub(super) size: u64,
    pub(super) link_count: u64,
    pub(super) accessed: Option<Datetime>,
    pub(super) modified: Option<Datetime>,
    /// Device and inode (volume serial and file index on Windows), which name the file within
    /// one run; `None` where the host could not read them.
    pub(super) id: Option<(u64, u64)>,
}

impl Stat {
    /// The identity and link count of the file behind an open handle. Windows only gives them
    /// for a handle, not for what a path names; a handle it cannot read leaves them unset.
    #[cfg(windows)]
    pub(super) fn identified_by(mut self, handle: &impl std::os::windows::io::AsHandle) -> Self {
        let info = handle
            .as_handle()
            .try_clone_to_owned()
            .and_then(|owned| winapi_util::file::information(std::fs::File::from(owned)));
        if let Ok(info) = info {
            self.link_count = info.number_of_links();
            self.id = Some((info.volume_serial_number(), info.file_index()));
        }
        self
    }

    #[cfg(not(windows))]
    pub(super) fn identified_by<H>(self, _handle: &H) -> Self {
        self
    }

    /// `identified_by` for the file or directory `path` names below `dir`, opened so that
    /// there is a handle to read; a symlink (not followed) or an entry that cannot be opened
    /// keeps its path as identity.
    #[cfg(windows)]
    pub(super) fn identified_below(self, dir: &Dir, path: &Path) -> Self {
        let handle = match self.kind {
            DescriptorType::Directory => dir.open_dir(path).map(|d| d.into_std_file()),
            DescriptorType::RegularFile => dir.open(path).map(|f| f.into_std()),
            _ => return self,
        };
        match handle {
            Ok(file) => self.identified_by(&file),
            Err(_) => self,
        }
    }

    #[cfg(not(windows))]
    pub(super) fn identified_below(self, _dir: &Dir, _path: &Path) -> Self {
        self
    }
}

impl From<std::fs::Metadata> for Stat {
    fn from(meta: std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let (link_count, id) = {
            use std::os::unix::fs::MetadataExt;
            (meta.nlink(), Some((meta.dev(), meta.ino())))
        };
        #[cfg(not(unix))]
        let (link_count, id) = (1u64, None);
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
        // cap-std's Windows `dev`/`ino`/`nlink` are unavailable (they need an unstable `std`
        // API), so off Unix this carries no identity until `identified_by` reads a handle.
        #[cfg(unix)]
        let (link_count, id) = { (meta.nlink(), Some((meta.dev(), meta.ino()))) };
        #[cfg(not(unix))]
        let (link_count, id) = (1u64, None);
        Stat {
            kind: descriptor_type(&meta.file_type()),
            size: meta.len(),
            link_count,
            accessed: datetime(meta.accessed().map(cap_std::time::SystemTime::into_std)),
            modified: datetime(meta.modified().map(cap_std::time::SystemTime::into_std)),
            id,
        }
    }
}

pub(super) fn datetime(time: std::io::Result<std::time::SystemTime>) -> Option<Datetime> {
    let since = time.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(Datetime {
        seconds: since.as_secs(),
        nanoseconds: since.subsec_nanos(),
    })
}

/// The status-change time is not reported.
pub(super) fn descriptor_stat(meta: Stat) -> DescriptorStat {
    DescriptorStat {
        type_: meta.kind,
        link_count: meta.link_count,
        size: meta.size,
        data_access_timestamp: meta.accessed,
        data_modification_timestamp: meta.modified,
        status_change_timestamp: None,
    }
}

/// What names a file within one run: its device and inode (its path where there are none).
pub(super) fn file_identity(meta: &Stat, path: &Path) -> (u64, u64, String) {
    match meta.id {
        Some((dev, ino)) => (dev, ino, String::new()),
        None => (0, 0, path.to_string_lossy().into_owned()),
    }
}

/// A hash of the file's identity (see `file_identity`), so the same file gives the same
/// value within one run and two files never do.
pub(super) fn metadata_hash(meta: &Stat, path: &Path) -> MetadataHashValue {
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
    pub(super) fn same_object(&self, a: u32, b: u32) -> bool {
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

/// Opens `path` below `dir` for `set-times`: a file or a directory, with the access to set
/// its times.
pub(super) fn open_for_set_times(
    dir: &Dir,
    path: &Path,
    follow: FollowSymlinks,
) -> std::io::Result<std::fs::File> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(follow);
    // Windows refuses SetFileTime on a handle without write-attributes access, which a
    // read-only open lacks; backup semantics lets the same open reach a directory.
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        options
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    dir.open_with(path, &options).map(|f| f.into_std())
}

/// The times of a `set-times` call. Nanoseconds of a second or more, or a time too far
/// ahead, are `invalid`.
pub(super) fn file_times(
    access: NewTimestamp,
    modification: NewTimestamp,
) -> Result<std::fs::FileTimes, Fs> {
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

/// The stream handle of a file stream's slot: file streams count up from `FIRST_FILE_STREAM`.
pub(super) fn file_stream_handle(slot: usize) -> u32 {
    FIRST_FILE_STREAM
        .checked_add(to_handle(slot))
        .unwrap_or_else(|| trap("stream handle table exhausted"))
}
