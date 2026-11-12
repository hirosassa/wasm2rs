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
| `stat` / `stat-at`, `metadata-hash`, `is-same-object` | a file or directory is identified by the volume serial and file index of an open handle (for a path, the host opens it to read them), so hard links and `a` vs `A` are one object and `link-count` is real. An entry that cannot be opened (and a symlink looked up without `symlink-follow`) falls back to its path string |
| timestamps | NTFS keeps 100 ns ticks, so finer values are rounded |
| `sync` / `sync-data` | succeed on a read-only handle or a directory, as on Unix, but flush nothing: Windows refuses `FlushFileBuffers` without write access, so the host skips it. Writes made through another handle to the same file, and directory entries, get no durability barrier from this call |

cap-std's own Windows `dev`/`ino`/`nlink` need an unstable `std` API, so the host reads them
from a handle with `winapi-util` (`Stat::identified_by` in `src/host/fs.rs`).

## What is not verified

Escape attempts through symlinks and junctions are not tested on Windows. The confinement
itself is cap-std's; do not rely on it for untrusted guests on Windows until a test covers it.
