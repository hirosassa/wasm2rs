fn wasi_confine(&self, full: &std::path::Path, follow: bool) -> bool {
    let root = match self.wasi_preopen.canonicalize() { Ok(r) => r, Err(_) => return false };
    let mut cur = if follow { Some(full) } else { full.parent() };
    while let Some(c) = cur {
        match c.canonicalize() {
            Ok(r) => return r.starts_with(&root),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(c).is_ok() {
                    // Another thread may have created `c` since the miss above, so
                    // only a path that still does not resolve is a dangling symlink.
                    return match c.canonicalize() { Ok(r) => r.starts_with(&root), Err(_) => false };
                }
                cur = c.parent();
            }
            Err(_) => return false,
        }
    }
    false
}
