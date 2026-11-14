// Templates of the fixed WASI / Emscripten import bodies, one per section:
// `//# <Variant>` (or `//# <Variant> files` for the file-table form) starts a
// section that runs to the next blank line. `@MEM@`, `@MEM_MUT@` and `@FDS@` are
// access placeholders (`WasiAccess::fill`), so these lines are not valid Rust and
// are not compiled here; the rendered methods are compiled by the e2e tests.
// Bodies assembled from parts (`FdWrite`, `PathOpen`, `PollOneoff`, ...) stay in
// `wasi.rs`.

// `proc_exit(code)` ends the process; it never returns.
//# ProcExit
fn wasi_proc_exit(&mut self, a0: i32) {
    std::process::exit(a0);
}

// `fd_close(fd)` drops an opened file (fd >= 4) from the table; a
// stdio/preopen fd (or one already closed) is a no-op. Without a file
// table there is nothing to close, so it always reports success.
//# FdClose files
fn wasi_fd_close(&mut self, a0: i32) -> i32 {
    let idx = a0 as u32 as usize;
    @FDS@.release(idx.wrapping_sub(4));
    0
}

//# FdClose
fn wasi_fd_close(&mut self, a0: i32) -> i32 {
    0
}

// `fd_seek(fd, offset, whence, newoffset)` seeks an opened file
// (fd >= 4) and writes the resulting offset; stdio fds are not
// seekable and return ESPIPE (70). Whence: 0=set, 1=cur, 2=end.
//# FdSeek files
fn wasi_fd_seek(&mut self, a0: i32, a1: i64, a2: i32, a3: i32) -> i32 {
    use std::io::Seek;
    let pos = match a2 {
        0 => std::io::SeekFrom::Start(a1 as u64),
        1 => std::io::SeekFrom::Current(a1),
        2 => std::io::SeekFrom::End(a1),
        _ => return 28,
    };
    let idx = a0 as u32 as usize;
    let newoff = match @FDS@.get_mut(idx.wrapping_sub(4)).and_then(|s| s.as_mut()).map(|t| &mut t.0) {
        Some(f) => match f.seek(pos) { Ok(n) => n, Err(_) => return 29 },
        None => return 70,
    };
    let w = a3 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(w as u64, 8)].copy_from_slice(&newoff.to_le_bytes());
    0
}

// Without a file table the only fds are the non-seekable stdio
// streams, so seek always returns ESPIPE (70).
//# FdSeek
fn wasi_fd_seek(&mut self, a0: i32, a1: i64, a2: i32, a3: i32) -> i32 {
    70
}

// `fd_fdstat_get(fd, buf)` writes a 24-byte `fdstat`. A stdio fd (0-2)
// is a character device (filetype 2) with all rights; with a file
// table, fd 3 is the preopen directory (3) and fd >= 4 an open regular
// file (4). Any other fd returns EBADF (8). Rust's `File::open` calls
// this right after `path_open`, so the file cases are required.
//# FdFdstatGet files
fn wasi_fd_fdstat_get(&mut self, a0: i32, a1: i32) -> i32 {
    let filetype = if (0..=2).contains(&a0) {
        2u8
    } else if a0 == 3 {
        3u8
    } else {
        let idx = a0 as u32 as usize;
        match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
            Some(_) => 4u8,
            None => return 8,
        }
    };
    let b = a1 as u32 as usize;
    let mut stat = [0u8; 24];
    stat[0] = filetype;
    stat[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    stat[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    @MEM_MUT@[Self::wasi_span(b as u64, 24)].copy_from_slice(&stat);
    0
}

//# FdFdstatGet
fn wasi_fd_fdstat_get(&mut self, a0: i32, a1: i32) -> i32 {
    if !(0..=2).contains(&a0) {
        return 8;
    }
    let b = a1 as u32 as usize;
    let mut stat = [0u8; 24];
    stat[0] = 2;
    stat[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    stat[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    @MEM_MUT@[Self::wasi_span(b as u64, 24)].copy_from_slice(&stat);
    0
}

// `clock_time_get(clock_id, precision, time)` writes the current
// time in nanoseconds since the Unix epoch (the clock id and
// precision are ignored). A pre-epoch clock yields 0.
//# ClockTimeGet
fn wasi_clock_time_get(&mut self, a0: i32, a1: i64, a2: i32) -> i32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let t = a2 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(t as u64, 8)].copy_from_slice(&now.to_le_bytes());
    0
}

// `random_get(buf, buf_len)` fills the buffer with OS entropy read
// from `/dev/urandom`. Returns 0 on success or an errno on failure.
//# RandomGet
fn wasi_random_get(&mut self, a0: i32, a1: i32) -> i32 {
    use std::io::Read;
    let buf = a0 as u32 as usize;
    let len = a1 as u32 as usize;
    let mut tmp = vec![0u8; len];
    let read = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut tmp));
    if read.is_err() {
        return 29;
    }
    @MEM_MUT@[Self::wasi_span(buf as u64, len as u64)].copy_from_slice(&tmp);
    0
}

// `fd_prestat_get(fd, buf)` describes a preopened directory. Exactly
// one is advertised: fd 3, whose 8-byte prestat is `{ tag: u8 = 0
// (dir), pr_name_len: u32 = <preopen name's byte length> }` (1 for the
// default "."). Any other fd returns EBADF (8) so wasi-libc stops
// scanning for preopens.
//# FdPrestatGet
fn wasi_fd_prestat_get(&mut self, a0: i32, a1: i32) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    if a0 != 3 {
        return 8;
    }
    let name_len = self.wasi_preopen.as_os_str().as_bytes().len() as u32;
    let b = a1 as u32 as usize;
    let mut pre = [0u8; 8];
    pre[4..8].copy_from_slice(&name_len.to_le_bytes());
    @MEM_MUT@[Self::wasi_span(b as u64, 8)].copy_from_slice(&pre);
    0
}

// `fd_prestat_dir_name(fd, path, path_len)` writes the preopen's name.
// fd 3 is the preopen directory, named by the configured preopen root
// (the process CWD "." by default). Other fds are EBADF (8).
//# FdPrestatDirName
fn wasi_fd_prestat_dir_name(&mut self, a0: i32, a1: i32, a2: i32) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    if a0 != 3 {
        return 8;
    }
    let name = self.wasi_preopen.as_os_str().as_bytes().to_vec();
    let p = a1 as u32 as usize;
    let n = (a2 as u32 as usize).min(name.len());
    @MEM_MUT@[Self::wasi_span(p as u64, n as u64)].copy_from_slice(&name[..n]);
    0
}

// `fd_filestat_get(fd, buf)` writes a 64-byte `filestat`. Only the
// filetype (@16) and byte size (@32) are populated: a stdio fd (0-2)
// is a character device (2) of size 0; an opened file (fd >= 4) is a
// regular file (4) with its real length. `nlink` (@24) is 1.
//# FdFilestatGet
fn wasi_fd_filestat_get(&mut self, a0: i32, a1: i32) -> i32 {
    let (filetype, size) = if (0..=2).contains(&a0) {
        (2u8, 0u64)
    } else if a0 == 3 {
        (3u8, 0u64)
    } else {
        let idx = a0 as u32 as usize;
        match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
            Some(f) => match f.metadata() { Ok(m) => (4u8, m.len()), Err(_) => return 29 },
            None => return 8,
        }
    };
    let mut st = [0u8; 64];
    st[16] = filetype;
    st[24..32].copy_from_slice(&1u64.to_le_bytes());
    st[32..40].copy_from_slice(&size.to_le_bytes());
    let b = a1 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(b as u64, 64)].copy_from_slice(&st);
    0
}

// `fd_tell(fd, offset)` writes the descriptor's current offset (a u64)
// without moving it. Stdio is not seekable (ESPIPE, 70).
//# FdTell files
fn wasi_fd_tell(&mut self, a0: i32, a1: i32) -> i32 {
    use std::io::Seek;
    if (0..=2).contains(&a0) {
        return 70;
    }
    let idx = a0 as u32 as usize;
    let off = match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => match (&*f).stream_position() { Ok(n) => n, Err(_) => return 29 },
        None => return if a0 == 3 { 31 } else { 8 },
    };
    let w = a1 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(w as u64, 8)].copy_from_slice(&off.to_le_bytes());
    0
}

//# FdTell
fn wasi_fd_tell(&mut self, a0: i32, a1: i32) -> i32 {
    if (0..=2).contains(&a0) { 70 } else { 8 }
}

// `fd_renumber(from, to)` moves an open file onto descriptor `to`,
// closing whatever was there. Both must be open. Stdio and the preopen
// directory (fd 0-3) are virtual and cannot be rebound (ENOTSUP).
//# FdRenumber files
fn wasi_fd_renumber(&mut self, a0: i32, a1: i32) -> i32 {
    if a0 == a1 && a0 >= 0 && @FDS@.get((a0 as u32 as usize).wrapping_sub(4)).is_some_and(|s| s.is_some()) {
        return 0;
    }
    if (0..=3).contains(&a0) || (0..=3).contains(&a1) {
        return 58;
    }
    let from = (a0 as u32 as usize).wrapping_sub(4);
    let to = (a1 as u32 as usize).wrapping_sub(4);
    if @FDS@.get(from).is_none_or(|s| s.is_none()) {
        return 8;
    }
    if @FDS@.get(to).is_none_or(|s| s.is_none()) {
        return 8;
    }
    @FDS@.swap(from, to);
    @FDS@.release(from);
    0
}

//# FdRenumber
fn wasi_fd_renumber(&mut self, a0: i32, a1: i32) -> i32 {
    if (0..=2).contains(&a0) || (0..=2).contains(&a1) { 58 } else { 8 }
}

// `proc_raise(sig)`: an out-of-range signal is EINVAL (28); delivering
// a signal is not supported (ENOSYS, 52).
//# ProcRaise
fn wasi_proc_raise(&mut self, a0: i32) -> i32 {
    if (1..=31).contains(&a0) { 52 } else { 28 }
}

// The main thread is its own runtime thread (`is_main`, `is_runtime`,
// `can_block`, a 64 KiB default stack), then runs its TLS init.
//# EmscriptenInitMainThread
fn emscripten_init_main_thread_js(&mut self, a0: i32) {
    self._emscripten_thread_init(a0, 1, 1, 1, 65536, 0);
    self._emscripten_tls_init();
}

// Milliseconds since the process first asked, like `performance.now()`.
//# EmscriptenGetNow
fn emscripten_get_now(&mut self) -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

//# EmscriptenNoop0
fn emscripten_noop0(&mut self) {}

//# EmscriptenNoop1
fn emscripten_noop1(&mut self, a0: i32) {}

// `sched_yield()` hints the scheduler to yield the CPU; the single
// owning thread simply yields and reports success.
//# SchedYield
fn wasi_sched_yield(&mut self) -> i32 {
    std::thread::yield_now();
    0
}

// `clock_res_get(clock_id, resolution)` writes the clock's resolution
// (a u64 nanosecond count) at the pointer. The clock id is ignored and
// the finest representable resolution, 1 ns, is reported.
//# ClockResGet
fn wasi_clock_res_get(&mut self, a0: i32, a1: i32) -> i32 {
    let r = a1 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(r as u64, 8)].copy_from_slice(&1u64.to_le_bytes());
    0
}

// `fd_pread(fd, iovs, iovs_len, offset, nread)` reads at an explicit
// offset without moving the file position (`read_at`), scatters the
// bytes across the iovecs, and stores the count at `nread`. Stdio is
// not seekable (ESPIPE, 70).
//# FdPread files
fn wasi_fd_pread(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {
    use std::os::unix::fs::FileExt;
    let mut iovs: Vec<(u64, u64)> = Vec::new();
    let mut cap = 0u64;
    for i in 0..a2 as u32 {
        let e = a1 as u32 as u64 + i as u64 * 8;
        let rec = &@MEM@[Self::wasi_span(e, 8)];
        let ptr = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]) as u64;
        let len = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]) as u64;
        iovs.push((ptr, len));
        cap += len;
    }
    let mut tmp = vec![0u8; Self::wasi_usize(cap)];
    let n = match a0 {
        0 | 1 | 2 => return 70,
        _ => {
            let idx = a0 as u32 as usize;
            match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
                Some(f) => match f.read_at(&mut tmp, a3 as u64) { Ok(n) => n, Err(_) => return 29 },
                None => return 8,
            }
        }
    };
    let mut off = 0usize;
    for (ptr, len) in iovs {
        if off >= n {
            break;
        }
        let take = len.min((n - off) as u64) as usize;
        @MEM_MUT@[Self::wasi_span(ptr, take as u64)].copy_from_slice(&tmp[off..off + take]);
        off += take;
    }
    let w = a4 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(w as u64, 4)].copy_from_slice(&(n as u32).to_le_bytes());
    0
}

// Without a file table only the non-seekable stdio fds exist, so a
// positioned read is ESPIPE (70) for stdio and EBADF (8) otherwise.
//# FdPread
fn wasi_fd_pread(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {
    match a0 {
        0 | 1 | 2 => 70,
        _ => 8,
    }
}

// `fd_pwrite(fd, iovs, iovs_len, offset, nwritten)` gathers the iovec
// buffers and writes them at an explicit offset without moving the
// file position (`write_at`), storing the count at `nwritten`. Stdio
// is not seekable (ESPIPE, 70).
//# FdPwrite files
fn wasi_fd_pwrite(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {
    use std::os::unix::fs::FileExt;
    let mut buf: Vec<u8> = Vec::new();
    for i in 0..a2 as u32 {
        let e = a1 as u32 as u64 + i as u64 * 8;
        let rec = &@MEM@[Self::wasi_span(e, 8)];
        let ptr = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]) as u64;
        let len = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]) as u64;
        buf.extend_from_slice(&@MEM@[Self::wasi_span(ptr, len)]);
    }
    let n = match a0 {
        0 | 1 | 2 => return 70,
        _ => {
            let idx = a0 as u32 as usize;
            match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
                Some(f) => match f.write_at(&buf, a3 as u64) { Ok(n) => n, Err(_) => return 29 },
                None => return 8,
            }
        }
    };
    let w = a4 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(w as u64, 4)].copy_from_slice(&(n as u32).to_le_bytes());
    0
}

// Without a file table only the non-seekable stdio fds exist, so a
// positioned write is ESPIPE (70) for stdio and EBADF (8) otherwise.
//# FdPwrite
fn wasi_fd_pwrite(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {
    match a0 {
        0 | 1 | 2 => 70,
        _ => 8,
    }
}

// `fd_readdir(fd, buf, buf_len, cookie, bufused)` enumerates a
// directory: fd 3 is the preopen root, and fd >= 4 is a directory
// opened via `path_open` (whose recorded path is re-opened with
// `read_dir`). It writes packed `dirent` records (a 24-byte header
// then the name) starting at `cookie`, stores the byte count at
// `bufused`, and truncates the final record when the buffer fills
// (then bufused == buf_len signals "call again"). A synthetic "."
// and ".." lead the listing; real entries follow sorted by name so a
// resumed `cookie` addresses the same slot. This is emitted only with
// a file table (`FdReaddir` forces `wasi_files`).
//# FdReaddir
fn wasi_fd_readdir(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::DirEntryExt;
    let dir = if a0 == 3 {
        self.wasi_preopen.clone()
    } else {
        let idx = a0 as u32 as usize;
        match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()) {
            Some((_, p)) => p.clone(),
            None => return 8,
        }
    };
    if !self.wasi_confine(&dir, true) {
        return 76;
    }
    let mut entries: Vec<(u64, Vec<u8>, u8)> = vec![(0, b".".to_vec(), 3u8), (0, b"..".to_vec(), 3u8)];
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => return match e.kind() {
            std::io::ErrorKind::NotFound => 44,
            std::io::ErrorKind::PermissionDenied => 2,
            _ => 29,
        },
    };
    let mut reals: Vec<(u64, Vec<u8>, u8)> = Vec::new();
    for ent in rd {
        let ent = match ent { Ok(e) => e, Err(_) => return 29 };
        let ftype = match ent.file_type() {
            Ok(t) => if t.is_dir() { 3u8 } else if t.is_symlink() { 7u8 } else if t.is_file() { 4u8 } else { 0u8 },
            Err(_) => 0u8,
        };
        reals.push((ent.ino(), ent.file_name().as_bytes().to_vec(), ftype));
    }
    reals.sort_by(|a, b| a.1.cmp(&b.1));
    entries.extend(reals);
    let buf = a1 as u32 as usize;
    let buf_len = a2 as u32 as usize;
    let start = usize::try_from(a3 as u64).unwrap_or(usize::MAX);
    let mut used = 0usize;
    for (i, (ino, name, ftype)) in entries.iter().enumerate().skip(start) {
        if used >= buf_len {
            break;
        }
        let mut head = [0u8; 24];
        head[0..8].copy_from_slice(&((i as u64) + 1).to_le_bytes());
        head[8..16].copy_from_slice(&ino.to_le_bytes());
        head[16..20].copy_from_slice(&(name.len() as u32).to_le_bytes());
        head[20] = *ftype;
        let take = (buf_len - used).min(24);
        @MEM_MUT@[Self::wasi_span(buf as u64 + used as u64, take as u64)].copy_from_slice(&head[..take]);
        used += take;
        if take < 24 || used >= buf_len {
            break;
        }
        let take = (buf_len - used).min(name.len());
        @MEM_MUT@[Self::wasi_span(buf as u64 + used as u64, take as u64)].copy_from_slice(&name[..take]);
        used += take;
        if take < name.len() {
            break;
        }
    }
    let w = a4 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(w as u64, 4)].copy_from_slice(&(used as u32).to_le_bytes());
    0
}
