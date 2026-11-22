// Templates of the fixed WASI / Emscripten import bodies, one per section:
// `//# <Variant>` (or `//# <Variant> files` for the file-table form) starts a
// section that runs to the next blank line. `@MEM@`, `@MEM_MUT@` and `@FDS@` are
// access placeholders (`WasiAccess::fill`), so these lines are not valid Rust and
// are not compiled here; the rendered methods are compiled by the e2e tests.
// A `@name(arg, ...)@` directive pulls in a fragment of `wasi_parts.tpl`.

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
// bytes across the iovecs, and stores the count at `nread` (0 on an
// error). Stdio is not seekable (ESPIPE, 70).
//# FdPread files
fn wasi_fd_pread(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {
    let r = self.wasi_read_iov(a0, a1, a2, Some(a3 as u64));
    self.wasi_put_count(a4, r)
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
// file position (`write_at`), storing the count at `nwritten` (0 on an
// error). Stdio is not seekable (ESPIPE, 70).
//# FdPwrite files
fn wasi_fd_pwrite(&mut self, a0: i32, a1: i32, a2: i32, a3: i64, a4: i32) -> i32 {
    let r = self.wasi_write_iov(a0, a1, a2, Some(a3 as u64));
    self.wasi_put_count(a4, r)
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

// `fd_write(fd, iovs, iovs_len, nwritten)` writes the gathered iovec
// buffers to stdout/stderr (or an opened file) and stores the byte
// count at `nwritten` (0 on an error). Out-of-range pointers panic (a
// wasm trap).
//# FdWrite
fn wasi_fd_write(&mut self, a0: i32, a1: i32, a2: i32, a3: i32) -> i32 {
    let r = self.wasi_write_iov(a0, a1, a2, None);
    self.wasi_put_count(a3, r)
}

// `fd_read(fd, iovs, iovs_len, nread)` performs one read into the
// total iovec capacity, scatters it across the iovecs, and stores the
// byte count at `nread` (0 on EOF or an error). Out-of-range pointers
// panic (a trap).
//# FdRead
fn wasi_fd_read(&mut self, a0: i32, a1: i32, a2: i32, a3: i32) -> i32 {
    let r = self.wasi_read_iov(a0, a1, a2, None);
    self.wasi_put_count(a3, r)
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
        Err(e) => return Self::wasi_errno(&e),
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

// The per-descriptor single read/write behind the iovec loops: the outer `Err`
// is a fixed errno (ESPIPE for positioned stdio, EISDIR for the preopen root,
// EBADF for an unknown fd), the inner result is the I/O call's.
//# IovIo files
fn wasi_iov_read_once(&mut self, fd: i32, off: Option<u64>, buf: &mut [u8]) -> Result<std::io::Result<usize>, i32> {
    use std::io::Read;
    use std::os::unix::fs::FileExt;
    match (fd, off) {
        (0, None) => Ok(std::io::stdin().read(buf)),
        (0..=2, Some(_)) => Err(70),
        (3, _) => Err(31),
        _ => {
            let idx = fd as u32 as usize;
            match @FDS@.get_mut(idx.wrapping_sub(4)).and_then(|s| s.as_mut()).map(|t| &mut t.0) {
                Some(f) => Ok(match off {
                    Some(o) => f.read_at(buf, o),
                    None => f.read(buf),
                }),
                None => Err(8),
            }
        }
    }
}
fn wasi_iov_write_once(&mut self, fd: i32, off: Option<u64>, chunk: &[u8]) -> Result<std::io::Result<usize>, i32> {
    use std::io::Write;
    use std::os::unix::fs::FileExt;
    match (fd, off) {
        (1, None) => Ok(std::io::stdout().write(chunk)),
        (2, None) => Ok(std::io::stderr().write(chunk)),
        (0..=2, Some(_)) => Err(70),
        _ => {
            let idx = fd as u32 as usize;
            match @FDS@.get_mut(idx.wrapping_sub(4)).and_then(|s| s.as_mut()).map(|t| &mut t.0) {
                Some(f) => Ok(match off {
                    Some(o) => f.write_at(chunk, o),
                    None => f.write(chunk),
                }),
                None => Err(8),
            }
        }
    }
}

//# IovIo
fn wasi_iov_read_once(&mut self, fd: i32, off: Option<u64>, buf: &mut [u8]) -> Result<std::io::Result<usize>, i32> {
    use std::io::Read;
    match (fd, off) {
        (0, None) => Ok(std::io::stdin().read(buf)),
        (0..=2, Some(_)) => Err(70),
        _ => Err(8),
    }
}
fn wasi_iov_write_once(&mut self, fd: i32, off: Option<u64>, chunk: &[u8]) -> Result<std::io::Result<usize>, i32> {
    use std::io::Write;
    match (fd, off) {
        (1, None) => Ok(std::io::stdout().write(chunk)),
        (2, None) => Ok(std::io::stderr().write(chunk)),
        (0..=2, Some(_)) => Err(70),
        _ => Err(8),
    }
}

// The transfer loops behind `fd_read`/`fd_pread` and `fd_write`/`fd_pwrite`
// (`off` is the explicit offset of the positioned forms). Every iovec is
// validated before any I/O, and a count above 2^20 is EFAULT. A read is one
// syscall (EOF is a successful 0); a write loops, and any error after a partial
// transfer is reported as the short count. Totals are clamped to the memory
// size (and to u32::MAX, the width of the count), since a short transfer is always legal.
//# IovLoops
fn wasi_iovecs(&self, iovs: i32, n: i32) -> Result<Vec<(u64, u64)>, i32> {
    let n = n as u32;
    if n > 1 << 20 {
        return Err(21);
    }
    let mut list: Vec<(u64, u64)> = Vec::new();
    for i in 0..n {
        let e = iovs as u32 as u64 + i as u64 * 8;
        let rec = &@MEM@[Self::wasi_span(e, 8)];
        let ptr = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]) as u64;
        let len = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]) as u64;
        let _ = &@MEM@[Self::wasi_span(ptr, len)];
        list.push((ptr, len));
    }
    Ok(list)
}
fn wasi_put_count(&mut self, at: i32, r: Result<usize, i32>) -> i32 {
    let (n, rc) = match r {
        Ok(n) => (n, 0),
        Err(e) => (0, e),
    };
    @MEM_MUT@[Self::wasi_span(at as u32 as u64, 4)].copy_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_le_bytes());
    rc
}
fn wasi_read_iov(&mut self, fd: i32, iovs: i32, n: i32, off: Option<u64>) -> Result<usize, i32> {
    let list = self.wasi_iovecs(iovs, n)?;
    let cap: u64 = list.iter().map(|&(_, len)| len).sum();
    let mut tmp = vec![0u8; Self::wasi_usize(cap.min(@MEM@.len() as u64).min(u32::MAX as u64))];
    let got = loop {
        match self.wasi_iov_read_once(fd, off, &mut tmp)? {
            Ok(k) => break k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Self::wasi_errno(&e)),
        }
    };
    let mut at = 0usize;
    for (ptr, len) in list {
        if at >= got {
            break;
        }
        let take = len.min((got - at) as u64) as usize;
        @MEM_MUT@[Self::wasi_span(ptr, take as u64)].copy_from_slice(&tmp[at..at + take]);
        at += take;
    }
    Ok(got)
}
fn wasi_write_iov(&mut self, fd: i32, iovs: i32, n: i32, off: Option<u64>) -> Result<usize, i32> {
    let list = self.wasi_iovecs(iovs, n)?;
    let limit = @MEM@.len().min(u32::MAX as usize);
    let mut buf: Vec<u8> = Vec::new();
    for (ptr, len) in list {
        if buf.len() < limit {
            buf.extend_from_slice(&@MEM@[Self::wasi_span(ptr, len)]);
        }
    }
    buf.truncate(limit);
    let mut done = 0usize;
    // Runs at least once, even for an empty buffer: the zero-length call is what
    // reports EBADF for an unknown fd.
    loop {
        let chunk = &buf[done..];
        let at = off.map(|o| o.wrapping_add(done as u64));
        match self.wasi_iov_write_once(fd, at, chunk) {
            Ok(Ok(0)) if chunk.is_empty() => break,
            Ok(Ok(0)) if done > 0 => break,
            Ok(Ok(0)) => return Err(29),
            Ok(Ok(k)) => done += k,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Ok(Err(_)) | Err(_) if done > 0 => break,
            Ok(Err(e)) => return Err(Self::wasi_errno(&e)),
            Err(rc) => return Err(rc),
        }
        if done >= buf.len() {
            break;
        }
    }
    Ok(done)
}

// `args_sizes_get(argc, argv_buf_size)` reports the argument count
// and the total byte size of the argument strings (each NUL-
// terminated), taken from the process's real argv.
//# ArgsSizesGet
fn wasi_args_sizes_get(&mut self, a0: i32, a1: i32) -> i32 {
    let items: Vec<String> = std::env::args().collect();
    let count = items.len() as u32;
    let size: u32 = items.iter().map(|s| s.len() as u32 + 1).sum();
    let c = a0 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(c as u64, 4)].copy_from_slice(&count.to_le_bytes());
    let b = a1 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(b as u64, 4)].copy_from_slice(&size.to_le_bytes());
    0
}

// `args_get(argv, argv_buf)` writes each argument's pointer into the
// `argv` array and its NUL-terminated bytes into `argv_buf`.
//# ArgsGet
fn wasi_args_get(&mut self, a0: i32, a1: i32) -> i32 {
    let items: Vec<String> = std::env::args().collect();
    let mut pv = a0 as u32 as u64;
    let mut pb = a1 as u32 as u64;
    for item in &items {
        let bytes = item.as_bytes();
        @MEM_MUT@[Self::wasi_span(pv, 4)].copy_from_slice(&(pb as u32).to_le_bytes());
        pv += 4;
        @MEM_MUT@[Self::wasi_span(pb, bytes.len() as u64)].copy_from_slice(bytes);
        pb += bytes.len() as u64;
        @MEM_MUT@[Self::wasi_span(pb, 1)][0] = 0;
        pb += 1;
    }
    0
}

// environ mirrors argv, but each string is `KEY=VALUE`.
//# EnvironSizesGet
fn wasi_environ_sizes_get(&mut self, a0: i32, a1: i32) -> i32 {
    let items: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    let count = items.len() as u32;
    let size: u32 = items.iter().map(|s| s.len() as u32 + 1).sum();
    let c = a0 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(c as u64, 4)].copy_from_slice(&count.to_le_bytes());
    let b = a1 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(b as u64, 4)].copy_from_slice(&size.to_le_bytes());
    0
}

// environ mirrors argv, but each string is `KEY=VALUE`.
//# EnvironGet
fn wasi_environ_get(&mut self, a0: i32, a1: i32) -> i32 {
    let items: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    let mut pv = a0 as u32 as u64;
    let mut pb = a1 as u32 as u64;
    for item in &items {
        let bytes = item.as_bytes();
        @MEM_MUT@[Self::wasi_span(pv, 4)].copy_from_slice(&(pb as u32).to_le_bytes());
        pv += 4;
        @MEM_MUT@[Self::wasi_span(pb, bytes.len() as u64)].copy_from_slice(bytes);
        pb += bytes.len() as u64;
        @MEM_MUT@[Self::wasi_span(pb, 1)][0] = 0;
        pb += 1;
    }
    0
}

// `path_open(dirfd, dirflags, path, path_len, oflags, rights_base,
// rights_inheriting, fdflags, opened_fd)` opens a file *within* the
// preopen (fd 3). Absolute paths and ".." escapes are refused with
// ENOTCAPABLE (76); the new descriptor is written at `opened_fd`.
// Containment is lexical (component-wise) plus `wasi_confine`, which
// canonicalizes the resolved path so a symlink pointing outside the
// preopen is refused too.
//# PathOpen
fn wasi_path_open(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i64, a6: i64, a7: i32, a8: i32) -> i32 {
    if a0 != 3 {
        return 8;
    }
@contain(full, a2, a3, true)@
    let creat = a4 & 0x1 != 0;
    let excl = a4 & 0x4 != 0;
    let trunc = a4 & 0x8 != 0;
    let want_write = (a5 as u64) & (1 << 6) != 0;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    if want_write || creat || trunc {
        opts.write(true);
    }
    if creat {
        opts.create(true);
    }
    if excl {
        opts.create_new(true);
    }
    if trunc {
        opts.truncate(true);
    }
    let file = match opts.open(&full) {
        Ok(f) => f,
        Err(e) => return Self::wasi_errno(&e),
    };
@alloc_fd(file, full)@
    let fd = idx as u32 + 4;
    let w = a8 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(w as u64, 4)].copy_from_slice(&fd.to_le_bytes());
    0
}

// `path_filestat_get(dirfd, flags, path, path_len, buf)` stats a path
// within the preopen (fd 3) *without opening it*, writing a 64-byte
// `filestat` (filetype @16, nlink=1 @24, size @32). Path containment
// is as in `path_open`: absolute paths, ".." escapes, and symlink
// escapes are refused with ENOTCAPABLE (76). `flags` (symlink follow) is ignored;
// `std::fs::metadata` follows symlinks.
//# PathFilestatGet
fn wasi_path_filestat_get(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32) -> i32 {
    if a0 != 3 {
        return 8;
    }
@contain(rel, a2, a3, true)@
    let meta = match std::fs::metadata(&rel) {
        Ok(m) => m,
        Err(e) => return Self::wasi_errno(&e),
    };
    let filetype = if meta.is_dir() { 3u8 } else if meta.is_file() { 4u8 } else { 0u8 };
    let mut st = [0u8; 64];
    st[16] = filetype;
    st[24..32].copy_from_slice(&1u64.to_le_bytes());
    st[32..40].copy_from_slice(&meta.len().to_le_bytes());
    let b = a4 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(b as u64, 64)].copy_from_slice(&st);
    0
}

// `path_create_directory(fd, path)`, `path_remove_directory(fd,
// path)`, and `path_unlink_file(fd, path)` mutate a single path
// within the preopen (fd 3). Path containment is as in `path_open`
// (absolute paths, ".." escapes, and symlink escapes are
// ENOTCAPABLE, 76).
//# PathCreateDirectory
fn wasi_path_create_directory(&mut self, a0: i32, a1: i32, a2: i32) -> i32 {
    if a0 != 3 {
        return 8;
    }
@contain(rel, a1, a2, false)@
    match std::fs::create_dir(&rel) {
        Ok(()) => 0,
        Err(e) => Self::wasi_errno(&e),
    }
}

// `path_create_directory(fd, path)`, `path_remove_directory(fd,
// path)`, and `path_unlink_file(fd, path)` mutate a single path
// within the preopen (fd 3). Path containment is as in `path_open`
// (absolute paths, ".." escapes, and symlink escapes are
// ENOTCAPABLE, 76).
//# PathRemoveDirectory
fn wasi_path_remove_directory(&mut self, a0: i32, a1: i32, a2: i32) -> i32 {
    if a0 != 3 {
        return 8;
    }
@contain(rel, a1, a2, false)@
    match std::fs::remove_dir(&rel) {
        Ok(()) => 0,
        Err(e) => Self::wasi_errno(&e),
    }
}

// `path_create_directory(fd, path)`, `path_remove_directory(fd,
// path)`, and `path_unlink_file(fd, path)` mutate a single path
// within the preopen (fd 3). Path containment is as in `path_open`
// (absolute paths, ".." escapes, and symlink escapes are
// ENOTCAPABLE, 76).
//# PathUnlinkFile
fn wasi_path_unlink_file(&mut self, a0: i32, a1: i32, a2: i32) -> i32 {
    if a0 != 3 {
        return 8;
    }
@contain(rel, a1, a2, false)@
    match std::fs::remove_file(&rel) {
        Ok(()) => 0,
        Err(e) => Self::wasi_errno(&e),
    }
}

// `path_rename(fd, old_path, new_fd, new_path)` renames within the
// preopen; both dirfds must be fd 3 and both paths are contained.
//# PathRename
fn wasi_path_rename(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i32) -> i32 {
    if a0 != 3 || a3 != 3 {
        return 8;
    }
@contain(old, a1, a2, false)@
@contain(new, a4, a5, false)@
    match std::fs::rename(&old, &new) {
        Ok(()) => 0,
        Err(e) => Self::wasi_errno(&e),
    }
}

// `path_symlink(old_path, fd, new_path)` creates a symlink at
// `new_path` (contained in the preopen, fd 3). `old_path` is the
// link's *contents*, stored verbatim; an absolute target is refused
// (ENOTCAPABLE) and a relative one that climbs out of the preopen
// is caught when a later path resolves through it (`wasi_confine`).
//# PathSymlink
fn wasi_path_symlink(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32) -> i32 {
    if a2 != 3 {
        return 8;
    }
    let tp = a0 as u32 as usize;
    let tlen = a1 as u32 as usize;
    let traw = @MEM@[Self::wasi_span(tp as u64, tlen as u64)].to_vec();
    let target = match std::str::from_utf8(&traw) { Ok(s) => s.to_owned(), Err(_) => return 28 };
    if std::path::Path::new(&target).is_absolute() {
        return 76;
    }
@contain(link, a3, a4, false)@
    match std::os::unix::fs::symlink(&target, &link) {
        Ok(()) => 0,
        Err(e) => Self::wasi_errno(&e),
    }
}

// `path_link(old_fd, old_flags, old_path, new_fd, new_path)` creates
// a hard link at `new_path` to `old_path`; both dirfds must be fd 3
// and both paths are contained. `old_flags` (symlink follow) is
// ignored (`std::fs::hard_link` does not follow the final symlink).
//# PathLink
fn wasi_path_link(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i32, a6: i32) -> i32 {
    if a0 != 3 || a4 != 3 {
        return 8;
    }
@contain(old, a2, a3, true)@
@contain(new, a5, a6, false)@
    match std::fs::hard_link(&old, &new) {
        Ok(()) => 0,
        Err(e) => Self::wasi_errno(&e),
    }
}

// `fd_sync(fd)` / `fd_datasync(fd)` flush an open file to disk; stdio
// and the preopen directory have nothing to flush.
//# FdSync files
fn wasi_fd_sync(&mut self, a0: i32) -> i32 {
    if (0..=2).contains(&a0) {
        return 0;
    }
    if a0 == 3 {
        return 0;
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => match f.sync_all() { Ok(_) => 0, Err(e) => Self::wasi_errno(&e) },
        None => 8,
    }
}

//# FdSync
fn wasi_fd_sync(&mut self, a0: i32) -> i32 {
    if (0..=2).contains(&a0) {
        return 0;
    }
    8
}

// `fd_sync(fd)` / `fd_datasync(fd)` flush an open file to disk; stdio
// and the preopen directory have nothing to flush.
//# FdDatasync files
fn wasi_fd_datasync(&mut self, a0: i32) -> i32 {
    if (0..=2).contains(&a0) {
        return 0;
    }
    if a0 == 3 {
        return 0;
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => match f.sync_data() { Ok(_) => 0, Err(e) => Self::wasi_errno(&e) },
        None => 8,
    }
}

//# FdDatasync
fn wasi_fd_datasync(&mut self, a0: i32) -> i32 {
    if (0..=2).contains(&a0) {
        return 0;
    }
    8
}

// `fd_advise(fd, offset, len, advice)` is a hint, so a valid descriptor
// just succeeds; an out-of-range advice value (> 5) is EINVAL.
//# FdAdvise files
fn wasi_fd_advise(&mut self, a0: i32, a1: i64, a2: i64, a3: i32) -> i32 {
    if !(0..=5).contains(&a3) {
        return 28;
    }
    if (0..=2).contains(&a0) {
        return 70;
    }
    if a0 == 3 {
        return 0;
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => 0,
        None => 8,
    }
}

//# FdAdvise
fn wasi_fd_advise(&mut self, a0: i32, a1: i64, a2: i64, a3: i32) -> i32 {
    if !(0..=5).contains(&a3) {
        return 28;
    }
    if (0..=2).contains(&a0) {
        return 70;
    }
    8
}

// `fd_allocate(fd, offset, len)` guarantees `offset + len` bytes exist:
// the file is extended with zeros if shorter and never shrunk.
//# FdAllocate files
fn wasi_fd_allocate(&mut self, a0: i32, a1: i64, a2: i64) -> i32 {
    if a1 < 0 || a2 <= 0 {
        return 28;
    }
    if (0..=2).contains(&a0) {
        return 70;
    }
    if a0 == 3 {
        return 31;
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => match f.metadata().and_then(|m| { let end = (a1 as u64).saturating_add(a2 as u64); if m.len() < end { f.set_len(end) } else { Ok(()) } }) { Ok(_) => 0, Err(e) => Self::wasi_errno(&e) },
        None => 8,
    }
}

//# FdAllocate
fn wasi_fd_allocate(&mut self, a0: i32, a1: i64, a2: i64) -> i32 {
    if a1 < 0 || a2 <= 0 {
        return 28;
    }
    if (0..=2).contains(&a0) {
        return 70;
    }
    8
}

// `fd_filestat_set_size(fd, size)` truncates or zero-extends a file.
//# FdFilestatSetSize files
fn wasi_fd_filestat_set_size(&mut self, a0: i32, a1: i64) -> i32 {
    if (0..=2).contains(&a0) {
        return 70;
    }
    if a0 == 3 {
        return 31;
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => match f.set_len(a1 as u64) { Ok(_) => 0, Err(e) => Self::wasi_errno(&e) },
        None => 8,
    }
}

//# FdFilestatSetSize
fn wasi_fd_filestat_set_size(&mut self, a0: i32, a1: i64) -> i32 {
    if (0..=2).contains(&a0) {
        return 70;
    }
    8
}

// `fd_filestat_set_times(fd, atim, mtim, fst_flags)` sets a file's
// access/modification times. The preopen directory is opened to reach
// its times; stdio has none (ESPIPE).
//# FdFilestatSetTimes files
fn wasi_fd_filestat_set_times(&mut self, a0: i32, a1: i64, a2: i64, a3: i32) -> i32 {
@times(a1, a2, a3)@
    if (0..=2).contains(&a0) {
        return 70;
    }
    if a0 == 3 {
        return match std::fs::File::open(&self.wasi_preopen).and_then(|d| d.set_times(times)) { Ok(_) => 0, Err(e) => Self::wasi_errno(&e) };
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => match f.set_times(times) { Ok(_) => 0, Err(e) => Self::wasi_errno(&e) },
        None => 8,
    }
}

//# FdFilestatSetTimes
fn wasi_fd_filestat_set_times(&mut self, a0: i32, a1: i64, a2: i64, a3: i32) -> i32 {
@times(a1, a2, a3)@
    if (0..=2).contains(&a0) {
        return 70;
    }
    8
}

// `path_filestat_set_times(fd, flags, path, path_len, atim, mtim,
// fst_flags)` sets the times of a path within the preopen (fd 3).
// `std` cannot touch a symlink itself, so a lookup without
// SYMLINK_FOLLOW (flags bit 0) that lands on a symlink is ENOTSUP.
//# PathFilestatSetTimes
fn wasi_path_filestat_set_times(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i64, a5: i64, a6: i32) -> i32 {
    if a0 != 3 {
        return 8;
    }
@times(a4, a5, a6)@
@contain(p, a2, a3, true)@
    if a1 & 1 == 0 {
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => return 58,
            Ok(_) => {}
            Err(e) => return Self::wasi_errno(&e),
        }
    }
    match std::fs::File::open(&p).and_then(|f| f.set_times(times)) { Ok(_) => 0, Err(e) => Self::wasi_errno(&e) }
}

// `path_readlink(fd, path, path_len, buf, buf_len, bufused)` copies a
// symlink's target (truncated to `buf_len`) into `buf` and stores the
// byte count at `bufused`. A path that is not a symlink is EINVAL.
//# PathReadlink
fn wasi_path_readlink(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i32) -> i32 {
    use std::os::unix::ffi::OsStrExt;
    if a0 != 3 {
        return 8;
    }
@contain(p, a1, a2, false)@
    let target = match std::fs::read_link(&p) {
        Ok(t) => t,
        Err(e) => return Self::wasi_errno(&e),
    };
    let bytes = target.as_os_str().as_bytes();
    let n = bytes.len().min(a4 as u32 as usize);
    let b = a3 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(b as u64, n as u64)].copy_from_slice(&bytes[..n]);
    let w = a5 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(w as u64, 4)].copy_from_slice(&(n as u32).to_le_bytes());
    0
}

// `fd_fdstat_set_flags(fd, flags)`: `std` cannot change APPEND/NONBLOCK
// etc. on an open descriptor, so clearing every flag succeeds and any
// other request is ENOTSUP rather than being silently ignored.
//# FdFdstatSetFlags files
fn wasi_fd_fdstat_set_flags(&mut self, a0: i32, a1: i32) -> i32 {
    if (0..=2).contains(&a0) {
        return if a1 == 0 { 0 } else { 58 };
    }
    if a0 == 3 {
        return if a1 == 0 { 0 } else { 58 };
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => if a1 == 0 { 0 } else { 58 },
        None => 8,
    }
}

//# FdFdstatSetFlags
fn wasi_fd_fdstat_set_flags(&mut self, a0: i32, a1: i32) -> i32 {
    if (0..=2).contains(&a0) {
        return if a1 == 0 { 0 } else { 58 };
    }
    8
}

// `fd_fdstat_set_rights(fd, base, inheriting)` is deprecated and rights
// are not tracked, so a valid descriptor accepts any narrowing.
//# FdFdstatSetRights files
fn wasi_fd_fdstat_set_rights(&mut self, a0: i32, a1: i64, a2: i64) -> i32 {
    if (0..=2).contains(&a0) {
        return 0;
    }
    if a0 == 3 {
        return 0;
    }
    let idx = a0 as u32 as usize;
    match @FDS@.get(idx.wrapping_sub(4)).and_then(|s| s.as_ref()).map(|t| &t.0) {
        Some(f) => 0,
        None => 8,
    }
}

//# FdFdstatSetRights
fn wasi_fd_fdstat_set_rights(&mut self, a0: i32, a1: i64, a2: i64) -> i32 {
    if (0..=2).contains(&a0) {
        return 0;
    }
    8
}

// `poll_oneoff(in, out, nsubscriptions, nevents)` waits on clock and
// fd subscriptions. Stdio and open files are always ready, an unopened
// fd is an EBADF event. If any fd is ready the call returns at once
// (with every clock that has already expired); otherwise it sleeps
// until the earliest clock. The subscriptions are copied out first, so
// no memory or fd-table lock is held while sleeping.
//# PollOneoff
fn wasi_poll_oneoff(&mut self, a0: i32, a1: i32, a2: i32, a3: i32) -> i32 {
    let n = a2 as u32 as usize;
    if n == 0 {
        return 28;
    }
    let inp = a0 as u32 as usize;
    let subs = @MEM@[Self::wasi_span(inp as u64, n as u64 * 48)].to_vec();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut events: Vec<(u64, u8, u16)> = Vec::new();
    let mut clocks: Vec<(u64, std::time::Duration)> = Vec::new();
    for sub in subs.chunks_exact(48) {
        let userdata = u64::from_le_bytes(sub[0..8].try_into().unwrap());
        let tag = sub[8];
        match tag {
            0 => {
                let id = u32::from_le_bytes(sub[16..20].try_into().unwrap());
                let timeout = u64::from_le_bytes(sub[24..32].try_into().unwrap());
                let flags = u16::from_le_bytes(sub[40..42].try_into().unwrap());
                if id > 3 {
                    events.push((userdata, 0, 28));
                    continue;
                }
                let wait = if flags & 1 != 0 { timeout.saturating_sub(now) } else { timeout };
                clocks.push((userdata, std::time::Duration::from_nanos(wait)));
            }
            1 | 2 => {
                let fd = u32::from_le_bytes(sub[16..20].try_into().unwrap()) as i32;
                let open = @open(fd)@;
                events.push((userdata, tag, if open { 0 } else { 8 }));
            }
            _ => events.push((userdata, tag, 28)),
        }
    }
    let wait = if events.is_empty() {
        clocks.iter().map(|c| c.1).min().unwrap_or_default()
    } else {
        std::time::Duration::ZERO
    };
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
    for (userdata, due) in clocks {
        if due <= wait {
            events.push((userdata, 0, 0));
        }
    }
    let mut out = vec![0u8; events.len() * 32];
    for (ev, (userdata, kind, error)) in out.chunks_exact_mut(32).zip(&events) {
        ev[0..8].copy_from_slice(&userdata.to_le_bytes());
        ev[8..10].copy_from_slice(&error.to_le_bytes());
        ev[10] = *kind;
    }
    let o = a1 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(o as u64, (out.len()) as u64)].copy_from_slice(&out);
    let p = a3 as u32 as usize;
    @MEM_MUT@[Self::wasi_span(p as u64, 4)].copy_from_slice(&(events.len() as u32).to_le_bytes());
    0
}

// No socket descriptors exist in the table, so every `sock_*` call is
// EBADF (8) on an unopened fd and ENOTSOCK (57) on an open one.
//# SockAccept
fn wasi_sock_accept(&mut self, a0: i32, a1: i32, a2: i32) -> i32 {
    if @open(a0)@ { 57 } else { 8 }
}

// No socket descriptors exist in the table, so every `sock_*` call is
// EBADF (8) on an unopened fd and ENOTSOCK (57) on an open one.
//# SockRecv
fn wasi_sock_recv(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32, a5: i32) -> i32 {
    if @open(a0)@ { 57 } else { 8 }
}

// No socket descriptors exist in the table, so every `sock_*` call is
// EBADF (8) on an unopened fd and ENOTSOCK (57) on an open one.
//# SockSend
fn wasi_sock_send(&mut self, a0: i32, a1: i32, a2: i32, a3: i32, a4: i32) -> i32 {
    if @open(a0)@ { 57 } else { 8 }
}

// No socket descriptors exist in the table, so every `sock_*` call is
// EBADF (8) on an unopened fd and ENOTSOCK (57) on an open one.
//# SockShutdown
fn wasi_sock_shutdown(&mut self, a0: i32, a1: i32) -> i32 {
    if @open(a0)@ { 57 } else { 8 }
}

// Emscripten's thread-bookkeeping import taking two `i32`s: nothing to do natively.
//# EmscriptenNoop2
fn emscripten_noop2(&mut self, a0: i32, a1: i32) {}

// `emscripten_resize_heap(requested_bytes)`: a shared memory cannot move, so
// natively it always reports failure (0).
//# EmscriptenResizeHeap
fn emscripten_resize_heap(&mut self, a0: i32) -> i32 { 0 }
