fn wasi_span(start: u64, len: u64) -> std::ops::Range<usize> {
    let Some(end) = start.checked_add(len) else { panic!("out of bounds memory access") };
    Self::wasi_usize(start)..Self::wasi_usize(end)
}

/// The WASI errno of an I/O error. The OS errno is preferred (the values below
/// are shared by Linux and macOS); the `ErrorKind` is the fallback.
fn wasi_errno(e: &std::io::Error) -> i32 {
    #[cfg(unix)]
    {
        match e.raw_os_error() {
            Some(1) => return 63,
            Some(2) => return 44,
            Some(3) => return 71,
            Some(4) => return 27,
            Some(5) => return 29,
            Some(6) => return 60,
            Some(9) => return 8,
            Some(12) => return 48,
            Some(13) => return 2,
            Some(16) => return 10,
            Some(17) => return 20,
            Some(18) => return 75,
            Some(19) => return 43,
            Some(20) => return 54,
            Some(21) => return 31,
            Some(22) => return 28,
            Some(23) => return 41,
            Some(24) => return 33,
            Some(25) => return 59,
            Some(26) => return 74,
            Some(27) => return 22,
            Some(28) => return 51,
            Some(29) => return 70,
            Some(30) => return 69,
            Some(31) => return 34,
            Some(32) => return 64,
            _ => {}
        }
    }
    match e.kind() {
        std::io::ErrorKind::NotFound => 44,
        std::io::ErrorKind::PermissionDenied => 2,
        std::io::ErrorKind::AlreadyExists => 20,
        std::io::ErrorKind::InvalidInput => 28,
        std::io::ErrorKind::BrokenPipe => 64,
        _ => 29,
    }
}

fn wasi_usize(n: u64) -> usize {
    match usize::try_from(n) {
        Ok(n) => n,
        Err(_) => panic!("out of bounds memory access"),
    }
}
