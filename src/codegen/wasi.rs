use wasmparser::ValType;

/// A recognised WASI (`wasi_snapshot_preview1`) function that is generated as a
/// native inherent `Instance` method rather than dispatched through the host
/// trait. Only this small subset is native so far; every other WASI import
/// still falls back to the injected host trait.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WasiFn {
    ProcExit,
    FdWrite,
    FdRead,
    FdClose,
    FdSeek,
    FdFdstatGet,
    ArgsSizesGet,
    ArgsGet,
    EnvironSizesGet,
    EnvironGet,
    ClockTimeGet,
    RandomGet,
    PathOpen,
    FdPrestatGet,
    FdPrestatDirName,
    FdFilestatGet,
    SchedYield,
    ClockResGet,
    FdPread,
    FdPwrite,
    PathFilestatGet,
    PathCreateDirectory,
    PathRemoveDirectory,
    PathUnlinkFile,
    PathRename,
    PathSymlink,
    FdReaddir,
}

impl WasiFn {
    /// Match a `(module, name)` import against the native WASI subset. The
    /// signature must match too, so a mis-typed import is left to the host
    /// trait rather than bound to a wrong native body.
    pub(crate) fn recognise(
        module: &str,
        name: &str,
        params: &[ValType],
        results: &[ValType],
    ) -> Option<Self> {
        if module != "wasi_snapshot_preview1" {
            return None;
        }
        let candidate = match name {
            "proc_exit" => WasiFn::ProcExit,
            "fd_write" => WasiFn::FdWrite,
            "fd_read" => WasiFn::FdRead,
            "fd_close" => WasiFn::FdClose,
            "fd_seek" => WasiFn::FdSeek,
            "fd_fdstat_get" => WasiFn::FdFdstatGet,
            "args_sizes_get" => WasiFn::ArgsSizesGet,
            "args_get" => WasiFn::ArgsGet,
            "environ_sizes_get" => WasiFn::EnvironSizesGet,
            "environ_get" => WasiFn::EnvironGet,
            "clock_time_get" => WasiFn::ClockTimeGet,
            "random_get" => WasiFn::RandomGet,
            "path_open" => WasiFn::PathOpen,
            "fd_prestat_get" => WasiFn::FdPrestatGet,
            "fd_prestat_dir_name" => WasiFn::FdPrestatDirName,
            "fd_filestat_get" => WasiFn::FdFilestatGet,
            "sched_yield" => WasiFn::SchedYield,
            "clock_res_get" => WasiFn::ClockResGet,
            "fd_pread" => WasiFn::FdPread,
            "fd_pwrite" => WasiFn::FdPwrite,
            "path_filestat_get" => WasiFn::PathFilestatGet,
            "path_create_directory" => WasiFn::PathCreateDirectory,
            "path_remove_directory" => WasiFn::PathRemoveDirectory,
            "path_unlink_file" => WasiFn::PathUnlinkFile,
            "path_rename" => WasiFn::PathRename,
            "path_symlink" => WasiFn::PathSymlink,
            "fd_readdir" => WasiFn::FdReaddir,
            _ => return None,
        };
        (params == candidate.params() && results == candidate.results()).then_some(candidate)
    }

    fn params(self) -> &'static [ValType] {
        use ValType::{I32, I64};
        match self {
            WasiFn::SchedYield => &[],
            WasiFn::ProcExit | WasiFn::FdClose => &[I32],
            WasiFn::FdWrite | WasiFn::FdRead => &[I32, I32, I32, I32],
            WasiFn::FdSeek => &[I32, I64, I32, I32],
            WasiFn::FdFdstatGet
            | WasiFn::ArgsSizesGet
            | WasiFn::ArgsGet
            | WasiFn::EnvironSizesGet
            | WasiFn::EnvironGet
            | WasiFn::RandomGet
            | WasiFn::FdPrestatGet
            | WasiFn::FdFilestatGet
            | WasiFn::ClockResGet => &[I32, I32],
            WasiFn::ClockTimeGet => &[I32, I64, I32],
            // fd, path, path_len.
            WasiFn::FdPrestatDirName
            | WasiFn::PathCreateDirectory
            | WasiFn::PathRemoveDirectory
            | WasiFn::PathUnlinkFile => &[I32, I32, I32],
            // old_path, old_path_len, fd, new_path, new_path_len.
            WasiFn::PathSymlink => &[I32, I32, I32, I32, I32],
            // fd, old_path, old_path_len, new_fd, new_path, new_path_len.
            WasiFn::PathRename => &[I32, I32, I32, I32, I32, I32],
            // fd, iovs, iovs_len, offset, nread/nwritten.
            WasiFn::FdPread | WasiFn::FdPwrite => &[I32, I32, I32, I64, I32],
            // fd, buf, buf_len, cookie, bufused.
            WasiFn::FdReaddir => &[I32, I32, I32, I64, I32],
            // fd, lookupflags, path, path_len, filestat_buf.
            WasiFn::PathFilestatGet => &[I32, I32, I32, I32, I32],
            // dirfd, dirflags, path, path_len, oflags, rights_base,
            // rights_inheriting, fdflags, opened_fd.
            WasiFn::PathOpen => &[I32, I32, I32, I32, I32, I64, I64, I32, I32],
        }
    }

    fn results(self) -> &'static [ValType] {
        match self {
            WasiFn::ProcExit => &[],
            WasiFn::FdWrite
            | WasiFn::FdRead
            | WasiFn::FdClose
            | WasiFn::FdSeek
            | WasiFn::FdFdstatGet
            | WasiFn::ArgsSizesGet
            | WasiFn::ArgsGet
            | WasiFn::EnvironSizesGet
            | WasiFn::EnvironGet
            | WasiFn::ClockTimeGet
            | WasiFn::RandomGet
            | WasiFn::PathOpen
            | WasiFn::FdPrestatGet
            | WasiFn::FdPrestatDirName
            | WasiFn::FdFilestatGet
            | WasiFn::SchedYield
            | WasiFn::ClockResGet
            | WasiFn::FdPread
            | WasiFn::FdPwrite
            | WasiFn::PathFilestatGet
            | WasiFn::PathCreateDirectory
            | WasiFn::PathRemoveDirectory
            | WasiFn::PathUnlinkFile
            | WasiFn::PathRename
            | WasiFn::PathSymlink
            | WasiFn::FdReaddir => &[ValType::I32],
        }
    }

    /// The inherent method name emitted for this function (see `call_expr`).
    pub(super) fn method(self) -> &'static str {
        match self {
            WasiFn::ProcExit => "wasi_proc_exit",
            WasiFn::FdWrite => "wasi_fd_write",
            WasiFn::FdRead => "wasi_fd_read",
            WasiFn::FdClose => "wasi_fd_close",
            WasiFn::FdSeek => "wasi_fd_seek",
            WasiFn::FdFdstatGet => "wasi_fd_fdstat_get",
            WasiFn::ArgsSizesGet => "wasi_args_sizes_get",
            WasiFn::ArgsGet => "wasi_args_get",
            WasiFn::EnvironSizesGet => "wasi_environ_sizes_get",
            WasiFn::EnvironGet => "wasi_environ_get",
            WasiFn::ClockTimeGet => "wasi_clock_time_get",
            WasiFn::RandomGet => "wasi_random_get",
            WasiFn::PathOpen => "wasi_path_open",
            WasiFn::FdPrestatGet => "wasi_fd_prestat_get",
            WasiFn::FdPrestatDirName => "wasi_fd_prestat_dir_name",
            WasiFn::FdFilestatGet => "wasi_fd_filestat_get",
            WasiFn::SchedYield => "wasi_sched_yield",
            WasiFn::ClockResGet => "wasi_clock_res_get",
            WasiFn::FdPread => "wasi_fd_pread",
            WasiFn::FdPwrite => "wasi_fd_pwrite",
            WasiFn::PathFilestatGet => "wasi_path_filestat_get",
            WasiFn::PathCreateDirectory => "wasi_path_create_directory",
            WasiFn::PathRemoveDirectory => "wasi_path_remove_directory",
            WasiFn::PathUnlinkFile => "wasi_path_unlink_file",
            WasiFn::PathRename => "wasi_path_rename",
            WasiFn::PathSymlink => "wasi_path_symlink",
            WasiFn::FdReaddir => "wasi_fd_readdir",
        }
    }

    /// Whether the native body accesses linear memory (`self.mem()`), so a
    /// module using it must declare or import a memory. The exceptions are the
    /// functions whose bodies never touch memory: `proc_exit`, and the `fd_`
    /// stubs that only return an errno (`fd_close`, `fd_seek`).
    pub(super) fn needs_memory(self) -> bool {
        !matches!(
            self,
            WasiFn::ProcExit | WasiFn::FdClose | WasiFn::FdSeek | WasiFn::SchedYield
        )
    }

    /// The Rust source for this WASI method's inherent-method definition. When
    /// `files` is set the module also imports the preopen/`path_open` functions,
    /// so `fd_read`/`fd_write`/`fd_seek`/`fd_close` route descriptors >= 4 to the
    /// instance's `wasi_fds` table instead of handling stdio alone.
    pub(super) fn lines(self, files: bool) -> Vec<String> {
        let owned = |lines: &[&str]| lines.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        match self {
            // `proc_exit(code)` ends the process; it never returns.
            WasiFn::ProcExit => owned(&[
                "fn wasi_proc_exit(&mut self, a0: i32) {",
                "    std::process::exit(a0);",
                "}",
            ]),
            // `fd_write(fd, iovs, iovs_len, nwritten)` gathers the iovec buffers
            // from linear memory, writes them to stdout (fd 1) or stderr (fd 2),
            // stores the byte count at `nwritten`, and returns 0 on success (an
            // errno otherwise). Out-of-range pointers panic (a wasm trap).
            WasiFn::FdWrite => {
                let mut body = vec![
                    "fn wasi_fd_write(&mut self, a0: i32, a1: i32, a2: i32, a3: i32) -> i32 {",
                    "    use std::io::Write;",
                    "    let mut buf: Vec<u8> = Vec::new();",
                    "    for i in 0..a2 as usize {",
                    "        let e = a1 as u32 as usize + i * 8;",
                    "        let ptr = u32::from_le_bytes([self.mem()[e], self.mem()[e + 1], self.mem()[e + 2], self.mem()[e + 3]]) as usize;",
                    "        let len = u32::from_le_bytes([self.mem()[e + 4], self.mem()[e + 5], self.mem()[e + 6], self.mem()[e + 7]]) as usize;",
                    "        buf.extend_from_slice(&self.mem()[ptr..ptr + len]);",
                    "    }",
                    "    let ok = match a0 {",
                    "        1 => std::io::stdout().write_all(&buf).is_ok(),",
                    "        2 => std::io::stderr().write_all(&buf).is_ok(),",
                ];
                // fd 1/2 are stdout/stderr; with a file table, fd >= 4 writes to
                // the opened `std::fs::File`. Any other fd is EBADF (8).
                if files {
                    body.extend([
                        "        _ => {",
                        "            let idx = a0 as u32 as usize;",
                        "            match self.wasi_fds.get_mut(idx.wrapping_sub(4)).and_then(|s| s.as_mut()).map(|t| &mut t.0) {",
                        "                Some(f) => f.write_all(&buf).is_ok(),",
                        "                None => return 8,",
                        "            }",
                        "        }",
                    ]);
                } else {
                    body.push("        _ => return 8,");
                }
                body.extend([
                    "    };",
                    "    if !ok {",
                    "        return 29;",
                    "    }",
                    "    let n = (buf.len() as u32).to_le_bytes();",
                    "    let w = a3 as u32 as usize;",
                    "    self.mem_mut()[w..w + 4].copy_from_slice(&n);",
                    "    0",
                    "}",
                ]);
                owned(&body)
            }
            // `fd_read(fd, iovs, iovs_len, nread)` performs one read from stdin
            // (fd 0) into the total iovec capacity, scatters the bytes across the
            // iovec buffers, stores the byte count at `nread`, and returns 0 on
            // success (an errno otherwise). Out-of-range pointers panic (a trap).
            WasiFn::FdRead => {
                let mut body = vec![
                    "fn wasi_fd_read(&mut self, a0: i32, a1: i32, a2: i32, a3: i32) -> i32 {",
                    "    use std::io::Read;",
                    "    let mut iovs: Vec<(usize, usize)> = Vec::new();",
                    "    let mut cap = 0usize;",
                    "    for i in 0..a2 as usize {",
                    "        let e = a1 as u32 as usize + i * 8;",
                    "        let ptr = u32::from_le_bytes([self.mem()[e], self.mem()[e + 1], self.mem()[e + 2], self.mem()[e + 3]]) as usize;",
                    "        let len = u32::from_le_bytes([self.mem()[e + 4], self.mem()[e + 5], self.mem()[e + 6], self.mem()[e + 7]]) as usize;",
                    "        iovs.push((ptr, len));",
                    "        cap += len;",
                    "    }",
                    "    let mut tmp = vec![0u8; cap];",
                    "    let n = match a0 {",
                    "        0 => match std::io::stdin().read(&mut tmp) { Ok(n) => n, Err(_) => return 29 },",
                ];
                // fd 0 reads stdin; with a file table, fd >= 4 reads the opened
                // `std::fs::File`. Any other fd is EBADF (8).
                if files {
                    body.extend([
                        "        _ => {",
                        "            let idx = a0 as u32 as usize;",
                        "            match self.wasi_fds.get_mut(idx.wrapping_sub(4)).and_then(|s| s.as_mut()).map(|t| &mut t.0) {",
                        "                Some(f) => match f.read(&mut tmp) { Ok(n) => n, Err(_) => return 29 },",
                        "                None => return 8,",
                        "            }",
                        "        }",
                    ]);
                } else {
                    body.push("        _ => return 8,");
                }
                body.extend([
                    "    };",
                    "    let mut off = 0usize;",
                    "    for (ptr, len) in iovs {",
                    "        if off >= n {",
                    "            break;",
                    "        }",
                    "        let take = len.min(n - off);",
                    "        self.mem_mut()[ptr..ptr + take].copy_from_slice(&tmp[off..off + take]);",
                    "        off += take;",
                    "    }",
                    "    let w = a3 as u32 as usize;",
                    "    self.mem_mut()[w..w + 4].copy_from_slice(&(n as u32).to_le_bytes());",
                    "    0",
                    "}",
                ]);
                owned(&body)
            }
            // `fd_close(fd)` drops an opened file (fd >= 4) from the table; a
            // stdio/preopen fd (or one already closed) is a no-op. Without a file
            // table there is nothing to close, so it always reports success.
            WasiFn::FdClose if files => owned(&[
                "fn wasi_fd_close(&mut self, a0: i32) -> i32 {",
                "    let idx = a0 as u32 as usize;",
                "    if let Some(slot) = self.wasi_fds.get_mut(idx.wrapping_sub(4)) {",
                "        *slot = None;",
                "    }",
                "    0",
                "}",
            ]),
            WasiFn::FdClose => owned(&[
                "fn wasi_fd_close(&mut self, a0: i32) -> i32 {",
                "    0",
                "}",
            ]),
            // `fd_seek(fd, offset, whence, newoffset)` seeks an opened file
            // (fd >= 4) and writes the resulting offset; stdio fds are not
            // seekable and return ESPIPE (70). Whence: 0=set, 1=cur, 2=end.
            WasiFn::FdSeek if files => owned(&[
                "fn wasi_fd_seek(&mut self, a0: i32, a1: i64, a2: i32, a3: i32) -> i32 {",
                "    use std::io::Seek;",
                "    let pos = match a2 {",
                "        0 => std::io::SeekFrom::Start(a1 as u64),",
                "        1 => std::io::SeekFrom::Current(a1),",
                "        2 => std::io::SeekFrom::End(a1),",
                "        _ => return 28,",
                "    };",
                "    let idx = a0 as u32 as usize;",
                "    let newoff = match self.wasi_fds.get_mut(idx.wrapping_sub(4)).and_then(|s| s.as_mut()).map(|t| &mut t.0) {",
                "        Some(f) => match f.seek(pos) { Ok(n) => n, Err(_) => return 29 },",
                "        None => return 70,",
                "    };",
                "    let w = a3 as u32 as usize;",
                "    self.mem_mut()[w..w + 8].copy_from_slice(&newoff.to_le_bytes());",
                "    0",
                "}",
            ]),
            // Without a file table the only fds are the non-seekable stdio
            // streams, so seek always returns ESPIPE (70).
            WasiFn::FdSeek => owned(&[
                "fn wasi_fd_seek(&mut self, a0: i32, a1: i64, a2: i32, a3: i32) -> i32 {",
                "    70",
                "}",
            ]),
            // `fd_fdstat_get(fd, buf)` writes a 24-byte `fdstat`. A stdio fd (0-2)
            // is a character device (filetype 2) with all rights; with a file
            // table, fd 3 is the preopen directory (3) and fd >= 4 an open regular
            // file (4). Any other fd returns EBADF (8). Rust's `File::open` calls
            // this right after `path_open`, so the file cases are required.
            WasiFn::FdFdstatGet if files => owned(&[
                "fn wasi_fd_fdstat_get(&mut self, a0: i32, a1: i32) -> i32 {",
                "    let filetype = if (0..=2).contains(&a0) {",
                "        2u8",
                "    } else if a0 == 3 {",
                "        3u8",
                "    } else {",
                "        let idx = a0 as u32 as usize;",
                "        match self.wasi_fds.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {",
                "            Some(_) => 4u8,",
                "            None => return 8,",
                "        }",
                "    };",
                "    let b = a1 as u32 as usize;",
                "    let mut stat = [0u8; 24];",
                "    stat[0] = filetype;",
                "    stat[8..16].copy_from_slice(&u64::MAX.to_le_bytes());",
                "    stat[16..24].copy_from_slice(&u64::MAX.to_le_bytes());",
                "    self.mem_mut()[b..b + 24].copy_from_slice(&stat);",
                "    0",
                "}",
            ]),
            WasiFn::FdFdstatGet => owned(&[
                "fn wasi_fd_fdstat_get(&mut self, a0: i32, a1: i32) -> i32 {",
                "    if !(0..=2).contains(&a0) {",
                "        return 8;",
                "    }",
                "    let b = a1 as u32 as usize;",
                "    let mut stat = [0u8; 24];",
                "    stat[0] = 2;",
                "    stat[8..16].copy_from_slice(&u64::MAX.to_le_bytes());",
                "    stat[16..24].copy_from_slice(&u64::MAX.to_le_bytes());",
                "    self.mem_mut()[b..b + 24].copy_from_slice(&stat);",
                "    0",
                "}",
            ]),
            // `args_sizes_get(argc, argv_buf_size)` reports the argument count
            // and the total byte size of the argument strings (each NUL-
            // terminated), taken from the process's real argv.
            WasiFn::ArgsSizesGet => sizes_lines("wasi_args_sizes_get", "std::env::args()"),
            // `args_get(argv, argv_buf)` writes each argument's pointer into the
            // `argv` array and its NUL-terminated bytes into `argv_buf`.
            WasiFn::ArgsGet => get_lines("wasi_args_get", "std::env::args()"),
            // environ mirrors argv, but each string is `KEY=VALUE`.
            WasiFn::EnvironSizesGet => sizes_lines(
                "wasi_environ_sizes_get",
                "std::env::vars().map(|(k, v)| format!(\"{k}={v}\"))",
            ),
            WasiFn::EnvironGet => get_lines(
                "wasi_environ_get",
                "std::env::vars().map(|(k, v)| format!(\"{k}={v}\"))",
            ),
            // `clock_time_get(clock_id, precision, time)` writes the current
            // time in nanoseconds since the Unix epoch (the clock id and
            // precision are ignored). A pre-epoch clock yields 0.
            WasiFn::ClockTimeGet => owned(&[
                "fn wasi_clock_time_get(&mut self, a0: i32, a1: i64, a2: i32) -> i32 {",
                "    let now = std::time::SystemTime::now()",
                "        .duration_since(std::time::UNIX_EPOCH)",
                "        .map(|d| d.as_nanos() as u64)",
                "        .unwrap_or(0);",
                "    let t = a2 as u32 as usize;",
                "    self.mem_mut()[t..t + 8].copy_from_slice(&now.to_le_bytes());",
                "    0",
                "}",
            ]),
            // `random_get(buf, buf_len)` fills the buffer with OS entropy read
            // from `/dev/urandom`. Returns 0 on success or an errno on failure.
            WasiFn::RandomGet => owned(&[
                "fn wasi_random_get(&mut self, a0: i32, a1: i32) -> i32 {",
                "    use std::io::Read;",
                "    let buf = a0 as u32 as usize;",
                "    let len = a1 as u32 as usize;",
                "    let mut tmp = vec![0u8; len];",
                "    let read = std::fs::File::open(\"/dev/urandom\").and_then(|mut f| f.read_exact(&mut tmp));",
                "    if read.is_err() {",
                "        return 29;",
                "    }",
                "    self.mem_mut()[buf..buf + len].copy_from_slice(&tmp);",
                "    0",
                "}",
            ]),
            // `fd_prestat_get(fd, buf)` describes a preopened directory. Exactly
            // one is advertised: fd 3, whose 8-byte prestat is `{ tag: u8 = 0
            // (dir), pr_name_len: u32 = 1 }`. Any other fd returns EBADF (8) so
            // wasi-libc stops scanning for preopens.
            WasiFn::FdPrestatGet => owned(&[
                "fn wasi_fd_prestat_get(&mut self, a0: i32, a1: i32) -> i32 {",
                "    if a0 != 3 {",
                "        return 8;",
                "    }",
                "    let b = a1 as u32 as usize;",
                "    let mut pre = [0u8; 8];",
                "    pre[4..8].copy_from_slice(&1u32.to_le_bytes());",
                "    self.mem_mut()[b..b + 8].copy_from_slice(&pre);",
                "    0",
                "}",
            ]),
            // `fd_prestat_dir_name(fd, path, path_len)` writes the preopen's name.
            // fd 3 is the current directory, named ".". Other fds are EBADF (8).
            WasiFn::FdPrestatDirName => owned(&[
                "fn wasi_fd_prestat_dir_name(&mut self, a0: i32, a1: i32, a2: i32) -> i32 {",
                "    if a0 != 3 {",
                "        return 8;",
                "    }",
                "    let name = b\".\";",
                "    let p = a1 as u32 as usize;",
                "    let n = (a2 as u32 as usize).min(name.len());",
                "    self.mem_mut()[p..p + n].copy_from_slice(&name[..n]);",
                "    0",
                "}",
            ]),
            // `path_open(dirfd, dirflags, path, path_len, oflags, rights_base,
            // rights_inheriting, fdflags, opened_fd)` opens a file *within* the
            // preopen (fd 3). Absolute paths and ".." escapes are refused with
            // ENOTCAPABLE (76); the new descriptor is written at `opened_fd`.
            // Containment is *lexical* (component-wise), which stops a hostile
            // guest from naming a path outside the preopen; it does not chase
            // symlinks per-component, so a symlink already present inside the
            // preopen that points outside is followed (as with a plain `open`).
            WasiFn::PathOpen => owned(&[
                "fn wasi_path_open(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i64, a6: i64, a7: i32, a8: i32) -> i32 {",
                "    if a0 != 3 {",
                "        return 8;",
                "    }",
                "    let p = a2 as u32 as usize;",
                "    let len = a3 as u32 as usize;",
                "    let raw = self.mem()[p..p + len].to_vec();",
                "    let path = match std::str::from_utf8(&raw) { Ok(s) => s, Err(_) => return 28 };",
                "    let mut rel = std::path::PathBuf::new();",
                "    for comp in std::path::Path::new(path).components() {",
                "        match comp {",
                "            std::path::Component::Normal(c) => rel.push(c),",
                "            std::path::Component::CurDir => {}",
                "            _ => return 76,",
                "        }",
                "    }",
                "    let creat = a4 & 0x1 != 0;",
                "    let excl = a4 & 0x4 != 0;",
                "    let trunc = a4 & 0x8 != 0;",
                "    let want_write = (a5 as u64) & (1 << 6) != 0;",
                "    let mut opts = std::fs::OpenOptions::new();",
                "    opts.read(true);",
                "    if want_write || creat || trunc {",
                "        opts.write(true);",
                "    }",
                "    if creat {",
                "        opts.create(true);",
                "    }",
                "    if excl {",
                "        opts.create_new(true);",
                "    }",
                "    if trunc {",
                "        opts.truncate(true);",
                "    }",
                "    let file = match opts.open(&rel) {",
                "        Ok(f) => f,",
                "        Err(e) => return match e.kind() {",
                "            std::io::ErrorKind::NotFound => 44,",
                "            std::io::ErrorKind::PermissionDenied => 2,",
                "            std::io::ErrorKind::AlreadyExists => 20,",
                "            _ => 29,",
                "        },",
                "    };",
                "    let idx = match self.wasi_fds.iter().position(|s| s.is_none()) {",
                "        Some(i) => { self.wasi_fds[i] = Some((file, rel)); i }",
                "        None => { self.wasi_fds.push(Some((file, rel))); self.wasi_fds.len() - 1 }",
                "    };",
                "    let fd = idx as u32 + 4;",
                "    let w = a8 as u32 as usize;",
                "    self.mem_mut()[w..w + 4].copy_from_slice(&fd.to_le_bytes());",
                "    0",
                "}",
            ]),
            // `fd_filestat_get(fd, buf)` writes a 64-byte `filestat`. Only the
            // filetype (@16) and byte size (@32) are populated: a stdio fd (0-2)
            // is a character device (2) of size 0; an opened file (fd >= 4) is a
            // regular file (4) with its real length. `nlink` (@24) is 1.
            WasiFn::FdFilestatGet => owned(&[
                "fn wasi_fd_filestat_get(&mut self, a0: i32, a1: i32) -> i32 {",
                "    let (filetype, size) = if (0..=2).contains(&a0) {",
                "        (2u8, 0u64)",
                "    } else if a0 == 3 {",
                "        (3u8, 0u64)",
                "    } else {",
                "        let idx = a0 as u32 as usize;",
                "        match self.wasi_fds.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {",
                "            Some(f) => match f.metadata() { Ok(m) => (4u8, m.len()), Err(_) => return 29 },",
                "            None => return 8,",
                "        }",
                "    };",
                "    let mut st = [0u8; 64];",
                "    st[16] = filetype;",
                "    st[24..32].copy_from_slice(&1u64.to_le_bytes());",
                "    st[32..40].copy_from_slice(&size.to_le_bytes());",
                "    let b = a1 as u32 as usize;",
                "    self.mem_mut()[b..b + 64].copy_from_slice(&st);",
                "    0",
                "}",
            ]),
            // `sched_yield()` hints the scheduler to yield the CPU; the single
            // owning thread simply yields and reports success.
            WasiFn::SchedYield => owned(&[
                "fn wasi_sched_yield(&mut self) -> i32 {",
                "    std::thread::yield_now();",
                "    0",
                "}",
            ]),
            // `clock_res_get(clock_id, resolution)` writes the clock's resolution
            // (a u64 nanosecond count) at the pointer. The clock id is ignored and
            // the finest representable resolution, 1 ns, is reported.
            WasiFn::ClockResGet => owned(&[
                "fn wasi_clock_res_get(&mut self, a0: i32, a1: i32) -> i32 {",
                "    let r = a1 as u32 as usize;",
                "    self.mem_mut()[r..r + 8].copy_from_slice(&1u64.to_le_bytes());",
                "    0",
                "}",
            ]),
            // `fd_pread(fd, iovs, iovs_len, offset, nread)` reads at an explicit
            // offset without moving the file position (`read_at`), scatters the
            // bytes across the iovecs, and stores the count at `nread`. Stdio is
            // not seekable (ESPIPE, 70).
            WasiFn::FdPread if files => owned(&[
                "fn wasi_fd_pread(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {",
                "    use std::os::unix::fs::FileExt;",
                "    let mut iovs: Vec<(usize, usize)> = Vec::new();",
                "    let mut cap = 0usize;",
                "    for i in 0..a2 as usize {",
                "        let e = a1 as u32 as usize + i * 8;",
                "        let ptr = u32::from_le_bytes([self.mem()[e], self.mem()[e + 1], self.mem()[e + 2], self.mem()[e + 3]]) as usize;",
                "        let len = u32::from_le_bytes([self.mem()[e + 4], self.mem()[e + 5], self.mem()[e + 6], self.mem()[e + 7]]) as usize;",
                "        iovs.push((ptr, len));",
                "        cap += len;",
                "    }",
                "    let mut tmp = vec![0u8; cap];",
                "    let n = match a0 {",
                "        0 | 1 | 2 => return 70,",
                "        _ => {",
                "            let idx = a0 as u32 as usize;",
                "            match self.wasi_fds.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {",
                "                Some(f) => match f.read_at(&mut tmp, a3 as u64) { Ok(n) => n, Err(_) => return 29 },",
                "                None => return 8,",
                "            }",
                "        }",
                "    };",
                "    let mut off = 0usize;",
                "    for (ptr, len) in iovs {",
                "        if off >= n {",
                "            break;",
                "        }",
                "        let take = len.min(n - off);",
                "        self.mem_mut()[ptr..ptr + take].copy_from_slice(&tmp[off..off + take]);",
                "        off += take;",
                "    }",
                "    let w = a4 as u32 as usize;",
                "    self.mem_mut()[w..w + 4].copy_from_slice(&(n as u32).to_le_bytes());",
                "    0",
                "}",
            ]),
            // Without a file table only the non-seekable stdio fds exist, so a
            // positioned read is ESPIPE (70) for stdio and EBADF (8) otherwise.
            WasiFn::FdPread => owned(&[
                "fn wasi_fd_pread(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {",
                "    match a0 {",
                "        0 | 1 | 2 => 70,",
                "        _ => 8,",
                "    }",
                "}",
            ]),
            // `fd_pwrite(fd, iovs, iovs_len, offset, nwritten)` gathers the iovec
            // buffers and writes them at an explicit offset without moving the
            // file position (`write_at`), storing the count at `nwritten`. Stdio
            // is not seekable (ESPIPE, 70).
            WasiFn::FdPwrite if files => owned(&[
                "fn wasi_fd_pwrite(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {",
                "    use std::os::unix::fs::FileExt;",
                "    let mut buf: Vec<u8> = Vec::new();",
                "    for i in 0..a2 as usize {",
                "        let e = a1 as u32 as usize + i * 8;",
                "        let ptr = u32::from_le_bytes([self.mem()[e], self.mem()[e + 1], self.mem()[e + 2], self.mem()[e + 3]]) as usize;",
                "        let len = u32::from_le_bytes([self.mem()[e + 4], self.mem()[e + 5], self.mem()[e + 6], self.mem()[e + 7]]) as usize;",
                "        buf.extend_from_slice(&self.mem()[ptr..ptr + len]);",
                "    }",
                "    let n = match a0 {",
                "        0 | 1 | 2 => return 70,",
                "        _ => {",
                "            let idx = a0 as u32 as usize;",
                "            match self.wasi_fds.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {",
                "                Some(f) => match f.write_at(&buf, a3 as u64) { Ok(n) => n, Err(_) => return 29 },",
                "                None => return 8,",
                "            }",
                "        }",
                "    };",
                "    let w = a4 as u32 as usize;",
                "    self.mem_mut()[w..w + 4].copy_from_slice(&(n as u32).to_le_bytes());",
                "    0",
                "}",
            ]),
            // Without a file table only the non-seekable stdio fds exist, so a
            // positioned write is ESPIPE (70) for stdio and EBADF (8) otherwise.
            WasiFn::FdPwrite => owned(&[
                "fn wasi_fd_pwrite(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {",
                "    match a0 {",
                "        0 | 1 | 2 => 70,",
                "        _ => 8,",
                "    }",
                "}",
            ]),
            // `path_filestat_get(dirfd, flags, path, path_len, buf)` stats a path
            // within the preopen (fd 3) *without opening it*, writing a 64-byte
            // `filestat` (filetype @16, nlink=1 @24, size @32). Path containment
            // is lexical, as in `path_open`: absolute paths and ".." escapes are
            // refused with ENOTCAPABLE (76). `flags` (symlink follow) is ignored;
            // `std::fs::metadata` follows symlinks.
            WasiFn::PathFilestatGet => owned(&[
                "fn wasi_path_filestat_get(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32) -> i32 {",
                "    if a0 != 3 {",
                "        return 8;",
                "    }",
                "    let p = a2 as u32 as usize;",
                "    let len = a3 as u32 as usize;",
                "    let raw = self.mem()[p..p + len].to_vec();",
                "    let path = match std::str::from_utf8(&raw) { Ok(s) => s, Err(_) => return 28 };",
                "    let mut rel = std::path::PathBuf::new();",
                "    for comp in std::path::Path::new(path).components() {",
                "        match comp {",
                "            std::path::Component::Normal(c) => rel.push(c),",
                "            std::path::Component::CurDir => {}",
                "            _ => return 76,",
                "        }",
                "    }",
                "    let meta = match std::fs::metadata(&rel) {",
                "        Ok(m) => m,",
                "        Err(e) => return match e.kind() {",
                "            std::io::ErrorKind::NotFound => 44,",
                "            std::io::ErrorKind::PermissionDenied => 2,",
                "            _ => 29,",
                "        },",
                "    };",
                "    let filetype = if meta.is_dir() { 3u8 } else if meta.is_file() { 4u8 } else { 0u8 };",
                "    let mut st = [0u8; 64];",
                "    st[16] = filetype;",
                "    st[24..32].copy_from_slice(&1u64.to_le_bytes());",
                "    st[32..40].copy_from_slice(&meta.len().to_le_bytes());",
                "    let b = a4 as u32 as usize;",
                "    self.mem_mut()[b..b + 64].copy_from_slice(&st);",
                "    0",
                "}",
            ]),
            // `path_create_directory(fd, path)`, `path_remove_directory(fd,
            // path)`, and `path_unlink_file(fd, path)` mutate a single path
            // within the preopen (fd 3). Path containment is lexical, as in
            // `path_open` (absolute paths and ".." escapes are ENOTCAPABLE, 76).
            WasiFn::PathCreateDirectory => {
                path_mutate_lines("wasi_path_create_directory", "std::fs::create_dir")
            }
            WasiFn::PathRemoveDirectory => {
                path_mutate_lines("wasi_path_remove_directory", "std::fs::remove_dir")
            }
            WasiFn::PathUnlinkFile => {
                path_mutate_lines("wasi_path_unlink_file", "std::fs::remove_file")
            }
            // `path_rename(fd, old_path, new_fd, new_path)` renames within the
            // preopen; both dirfds must be fd 3 and both paths are contained.
            WasiFn::PathRename => {
                let mut body = owned(&[
                    "fn wasi_path_rename(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i32) -> i32 {",
                    "    if a0 != 3 || a3 != 3 {",
                    "        return 8;",
                    "    }",
                ]);
                body.extend(contain_path("old", "a1", "a2"));
                body.extend(contain_path("new", "a4", "a5"));
                body.extend(fs_result_lines("std::fs::rename(&old, &new)"));
                body.extend(owned(&["}"]));
                body
            }
            // `path_symlink(old_path, fd, new_path)` creates a symlink at
            // `new_path` (contained in the preopen, fd 3). `old_path` is the
            // link's *contents* (an arbitrary target string), so it is copied
            // verbatim and not containment-checked.
            WasiFn::PathSymlink => {
                let mut body = owned(&[
                    "fn wasi_path_symlink(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32) -> i32 {",
                    "    if a2 != 3 {",
                    "        return 8;",
                    "    }",
                    "    let tp = a0 as u32 as usize;",
                    "    let tlen = a1 as u32 as usize;",
                    "    let traw = self.mem()[tp..tp + tlen].to_vec();",
                    "    let target = match std::str::from_utf8(&traw) { Ok(s) => s.to_owned(), Err(_) => return 28 };",
                ]);
                body.extend(contain_path("link", "a3", "a4"));
                body.extend(fs_result_lines(
                    "std::os::unix::fs::symlink(&target, &link)",
                ));
                body.extend(owned(&["}"]));
                body
            }
            // `fd_readdir(fd, buf, buf_len, cookie, bufused)` enumerates a
            // directory: fd 3 is the preopen ".", and fd >= 4 is a directory
            // opened via `path_open` (whose recorded path is re-opened with
            // `read_dir`). It writes packed `dirent` records (a 24-byte header
            // then the name) starting at `cookie`, stores the byte count at
            // `bufused`, and truncates the final record when the buffer fills
            // (then bufused == buf_len signals "call again"). A synthetic "."
            // and ".." lead the listing; real entries follow sorted by name so a
            // resumed `cookie` addresses the same slot. This is emitted only with
            // a file table (`FdReaddir` forces `wasi_files`).
            WasiFn::FdReaddir => owned(&[
                "fn wasi_fd_readdir(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {",
                "    use std::os::unix::ffi::OsStrExt;",
                "    use std::os::unix::fs::DirEntryExt;",
                "    let dir = if a0 == 3 {",
                "        std::path::PathBuf::from(\".\")",
                "    } else {",
                "        let idx = a0 as u32 as usize;",
                "        match self.wasi_fds.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()) {",
                "            Some((_, p)) => p.clone(),",
                "            None => return 8,",
                "        }",
                "    };",
                "    let mut entries: Vec<(u64, Vec<u8>, u8)> = vec![(0, b\".\".to_vec(), 3u8), (0, b\"..\".to_vec(), 3u8)];",
                "    let rd = match std::fs::read_dir(&dir) {",
                "        Ok(rd) => rd,",
                "        Err(e) => return match e.kind() {",
                "            std::io::ErrorKind::NotFound => 44,",
                "            std::io::ErrorKind::PermissionDenied => 2,",
                "            _ => 29,",
                "        },",
                "    };",
                "    let mut reals: Vec<(u64, Vec<u8>, u8)> = Vec::new();",
                "    for ent in rd {",
                "        let ent = match ent { Ok(e) => e, Err(_) => return 29 };",
                "        let ftype = match ent.file_type() {",
                "            Ok(t) => if t.is_dir() { 3u8 } else if t.is_symlink() { 7u8 } else if t.is_file() { 4u8 } else { 0u8 },",
                "            Err(_) => 0u8,",
                "        };",
                "        reals.push((ent.ino(), ent.file_name().as_bytes().to_vec(), ftype));",
                "    }",
                "    reals.sort_by(|a, b| a.1.cmp(&b.1));",
                "    entries.extend(reals);",
                "    let buf = a1 as u32 as usize;",
                "    let buf_len = a2 as u32 as usize;",
                "    let start = a3 as u64 as usize;",
                "    let mut used = 0usize;",
                "    for (i, (ino, name, ftype)) in entries.iter().enumerate().skip(start) {",
                "        if used >= buf_len {",
                "            break;",
                "        }",
                "        let mut head = [0u8; 24];",
                "        head[0..8].copy_from_slice(&((i as u64) + 1).to_le_bytes());",
                "        head[8..16].copy_from_slice(&ino.to_le_bytes());",
                "        head[16..20].copy_from_slice(&(name.len() as u32).to_le_bytes());",
                "        head[20] = *ftype;",
                "        let take = (buf_len - used).min(24);",
                "        self.mem_mut()[buf + used..buf + used + take].copy_from_slice(&head[..take]);",
                "        used += take;",
                "        if take < 24 || used >= buf_len {",
                "            break;",
                "        }",
                "        let take = (buf_len - used).min(name.len());",
                "        self.mem_mut()[buf + used..buf + used + take].copy_from_slice(&name[..take]);",
                "        used += take;",
                "        if take < name.len() {",
                "            break;",
                "        }",
                "    }",
                "    let w = a4 as u32 as usize;",
                "    self.mem_mut()[w..w + 4].copy_from_slice(&(used as u32).to_le_bytes());",
                "    0",
                "}",
            ]),
        }
    }
}

/// The body of a single-path WASI mutator (`path_create_directory`,
/// `path_remove_directory`, `path_unlink_file`): refuse a dirfd other than the
/// preopen (fd 3), lexically contain the path, then apply `op` to it.
fn path_mutate_lines(method: &str, op: &str) -> Vec<String> {
    let mut body = vec![
        format!("fn {method}(&mut self, a0: i32, a1: i32, a2: i32) -> i32 {{"),
        "    if a0 != 3 {".to_string(),
        "        return 8;".to_string(),
        "    }".to_string(),
    ];
    body.extend(contain_path("rel", "a1", "a2"));
    body.extend(fs_result_lines(&format!("{op}(&rel)")));
    body.push("}".to_string());
    body
}

/// Emit the lines that read a UTF-8 path from linear memory at (`ptr_arg`,
/// `len_arg`) and build a lexically-contained relative `PathBuf` bound to a
/// local named `out`. An absolute path or a `..` escape returns ENOTCAPABLE
/// (76); invalid UTF-8 returns EINVAL (28). The scratch locals are derived from
/// `out`, so two calls in one body (e.g. `path_rename`) do not collide.
fn contain_path(out: &str, ptr_arg: &str, len_arg: &str) -> Vec<String> {
    vec![
        format!("    let {out}_p = {ptr_arg} as u32 as usize;"),
        format!("    let {out}_len = {len_arg} as u32 as usize;"),
        format!("    let {out}_raw = self.mem()[{out}_p..{out}_p + {out}_len].to_vec();"),
        format!(
            "    let {out}_s = match std::str::from_utf8(&{out}_raw) {{ Ok(s) => s.to_owned(), Err(_) => return 28 }};"
        ),
        format!("    let mut {out} = std::path::PathBuf::new();"),
        format!("    for comp in std::path::Path::new(&{out}_s).components() {{"),
        "        match comp {".to_string(),
        format!("            std::path::Component::Normal(c) => {out}.push(c),"),
        "            std::path::Component::CurDir => {}".to_string(),
        "            _ => return 76,".to_string(),
        "        }".to_string(),
        "    }".to_string(),
    ]
}

/// The trailing `match <call> { Ok(()) => 0, Err(e) => <errno by kind> }` shared
/// by the path-mutating WASI methods that return only an errno. Error kinds map
/// to the same errnos as `path_open`; anything else collapses to EIO (29).
fn fs_result_lines(call: &str) -> Vec<String> {
    vec![
        format!("    match {call} {{"),
        "        Ok(()) => 0,".to_string(),
        "        Err(e) => match e.kind() {".to_string(),
        "            std::io::ErrorKind::NotFound => 44,".to_string(),
        "            std::io::ErrorKind::PermissionDenied => 2,".to_string(),
        "            std::io::ErrorKind::AlreadyExists => 20,".to_string(),
        "            _ => 29,".to_string(),
        "        },".to_string(),
        "    }".to_string(),
    ]
}

/// The body of a WASI `*_sizes_get` method: count the strings yielded by
/// `source` and their total NUL-terminated byte size, writing both as `u32`.
fn sizes_lines(method: &str, source: &str) -> Vec<String> {
    let mut lines = vec![
        format!("fn {method}(&mut self, a0: i32, a1: i32) -> i32 {{"),
        format!("    let items: Vec<String> = {source}.collect();"),
    ];
    lines.extend(
        [
            "    let count = items.len() as u32;",
            "    let size: u32 = items.iter().map(|s| s.len() as u32 + 1).sum();",
            "    let c = a0 as u32 as usize;",
            "    self.mem_mut()[c..c + 4].copy_from_slice(&count.to_le_bytes());",
            "    let b = a1 as u32 as usize;",
            "    self.mem_mut()[b..b + 4].copy_from_slice(&size.to_le_bytes());",
            "    0",
            "}",
        ]
        .map(str::to_string),
    );
    lines
}

/// The body of a WASI `*_get` method: write each string yielded by `source`
/// into the `a1` buffer (NUL-terminated) and its pointer into the `a0` array.
fn get_lines(method: &str, source: &str) -> Vec<String> {
    let mut lines = vec![
        format!("fn {method}(&mut self, a0: i32, a1: i32) -> i32 {{"),
        format!("    let items: Vec<String> = {source}.collect();"),
    ];
    lines.extend(
        [
            "    let mut pv = a0 as u32 as usize;",
            "    let mut pb = a1 as u32 as usize;",
            "    for item in &items {",
            "        let bytes = item.as_bytes();",
            "        self.mem_mut()[pv..pv + 4].copy_from_slice(&(pb as u32).to_le_bytes());",
            "        pv += 4;",
            "        self.mem_mut()[pb..pb + bytes.len()].copy_from_slice(bytes);",
            "        pb += bytes.len();",
            "        self.mem_mut()[pb] = 0;",
            "        pb += 1;",
            "    }",
            "    0",
            "}",
        ]
        .map(str::to_string),
    );
    lines
}
