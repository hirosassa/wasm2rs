# wasm2rs-wasi on Windows

`wasm2rs-wasi` builds and its test suite runs on `windows-latest` in CI (job
`wasm2rs-wasi on Windows`, #205). Windows is supported with the limits below; Unix is the
reference platform, and only the Unix runs also check the preopen escape cases
(`tests/preopen_confinement.rs`, which needs symlinks).

## What differs from Unix

Observed in CI, not guesses:

| Area | Windows behaviour |
| --- | --- |
| `symlink-at` | `unsupported` (no symlink creation) |
| `stat` / `stat-at` | no device/inode or link count: `link-count` is always 1 |
| `metadata-hash`, `is-same-object` | a file is identified by its path string (case-sensitive, not normalised), so a hard link, or `a` vs `A`, counts as a different object |
| timestamps | NTFS keeps 100 ns ticks, so finer values are rounded |
| `sync` / `sync-data` | `access` on a read-only handle or a directory (the OS refuses to flush it); on Unix both succeed, so this is a deviation |
| `set-times-at` through a path | answered `access` for a file that is open (cause not diagnosed; likely the path open lacks write-attributes access) |

cap-std's own Windows `dev`/`ino`/`nlink` panic unless the metadata came from an open
handle, so the host does not call them off Unix (`Stat::from` in `src/host/fs.rs`).

## What is not verified

Escape attempts through symlinks and junctions are not tested on Windows. The confinement
itself is cap-std's; do not rely on it for untrusted guests on Windows until a test covers it.
