// Fragments that several bodies in `wasi_fns.tpl` share. A body pulls one in with
// a `@name(arg, ...)@` directive (see `wasi::expand_parts`): on a line of its own
// the directive is replaced by the fragment's lines, inside a line by its single
// line. `@1@`..`@4@` stand for the directive's arguments. A fragment is a
// section that runs to the next blank line, `//# <name>` or, when it depends on
// the module, `//# <name> files` / `//# <name> locked`. Like `wasi_fns.tpl` this
// is not compiled here; the e2e tests compile the rendered methods.

// Read a UTF-8 path from linear memory at (ptr `@2@`, len `@3@`), lexically
// contain it, and bind the result — resolved against the instance's preopen
// root — to a local named `@1@`. An absolute path, a `..` escape, or a symlink
// resolving outside the preopen (`wasi_confine`, with `@4@` deciding whether the
// final component is resolved) returns ENOTCAPABLE (76); invalid UTF-8 returns
// EINVAL (28). The scratch locals are derived from `@1@`, so two uses in one body
// (e.g. `path_rename`) do not collide.
//# contain
    let @1@_p = @2@ as u32 as usize;
    let @1@_len = @3@ as u32 as usize;
    let @1@_raw = @MEM@[Self::wasi_span(@1@_p as u64, @1@_len as u64)].to_vec();
    let @1@_s = match std::str::from_utf8(&@1@_raw) { Ok(s) => s.to_owned(), Err(_) => return 28 };
    let mut @1@_rel = std::path::PathBuf::new();
    for comp in std::path::Path::new(&@1@_s).components() {
        match comp {
            std::path::Component::Normal(c) => @1@_rel.push(c),
            std::path::Component::CurDir => {}
            _ => return 76,
        }
    }
    let @1@ = self.wasi_preopen.join(&@1@_rel);
    if !self.wasi_confine(&@1@, @4@) {
        return 76;
    }

// Validate the `fst_flags` (bit 0 ATIM, 1 ATIM_NOW, 2 MTIM, 3 MTIM_NOW; a set and
// its `_NOW` twin conflict) and build `times` from the nanosecond timestamps in
// `@1@` (atim) / `@2@` (mtim); `@3@` holds the flags. An unset side is left
// unchanged.
//# times
    let fl = @3@ as u32;
    if fl > 15 || (fl & 1 != 0 && fl & 2 != 0) || (fl & 4 != 0 && fl & 8 != 0) {
        return 28;
    }
    let now = std::time::SystemTime::now();
    let mut times = std::fs::FileTimes::new();
    if fl & 1 != 0 {
        times = times.set_accessed(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(@1@ as u64));
    } else if fl & 2 != 0 {
        times = times.set_accessed(now);
    }
    if fl & 4 != 0 {
        times = times.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(@2@ as u64));
    } else if fl & 8 != 0 {
        times = times.set_modified(now);
    }

// A Rust `bool` expression: whether descriptor `@1@` (an `i32`) is open. Stdio is
// always open; with a file table so are the preopen (3) and the live slots.
//# open files
(0..=3).contains(&@1@) || @FDS@.get((@1@ as u32 as usize).wrapping_sub(4)).is_some_and(|s| s.is_some())

//# open
(0..=2).contains(&@1@)

// Insert `(@1@, @2@)` into the descriptor table as `idx`. When the table is
// locked the slot search and the insertion must run under one guard: two threads
// could otherwise take the same slot, and a guard in a `match` scrutinee must not
// be locked again in its arms.
//# alloc_fd locked
    let idx = {
        let mut fds = @FDS@;
        fds.alloc((@1@, @2@))
    };

//# alloc_fd
    let idx = @FDS@.alloc((@1@, @2@));
