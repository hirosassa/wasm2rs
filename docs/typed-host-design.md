# The typed WASI preview2 host: design note

This note describes the typed WASI preview2 host of the `wasm2rs-wasi` crate as it is now:
`WasiHost`, written against the `Host` trait that the crate owns. It is library API. For how the
component was analysed and how the host came about, see
[component-model-poc.md](component-model-poc.md); for the library's design and plan, see
[wasm2rs-wasi-design.md](wasm2rs-wasi-design.md); for the `--wit-host` / `--glue` flags, see the
README. Each section names the code or test that backs it.

## Structure

- `wasm2rs-wasi/src/generated.rs` holds the `Host` trait (all 199 methods of the official WASI
  0.2.0 WIT, each defaulting to a trap) and its types, one module per defining interface
  (`wasi_filesystem_types::ErrorCode`, `wasi_sockets_network::ErrorCode`, ...). It is generated
  (`WitNaming::Qualified` + `default_trap`) and checked in; `tests/it/wasi_generated.rs`
  regenerates it and fails on any difference.
- `wasm2rs-wasi/src/host.rs` is `WasiHost` and the pure logic around it (slabs, streams, send
  queue, path resolution, socket helpers); `host/impl_host.rs` is `impl Host for WasiHost`
  (all 199 methods, no canonical-ABI code: the glue does all of that, `wasi:http` included, see
  `docs/wasi-http-design.md`); `tests/it/wasip2_wit_regression.rs` pins that none is left at the
  trapping default.
- **Per-component glue.** The glue still depends on the component, so it is generated per
  component, in the external-types mode (`--wit-host --glue --external-types=wasm2rs_wasi`, or
  `WitOptions::external_types`): only `Glue<H: wasm2rs_wasi::Host>`, naming the library's types by
  path. The program is the transpiled core module, the glue and a `main`, linked against the
  library; nothing of the host is compiled again, and a component importing a method the host
  lacks builds and traps at the call.
- **Consumers.** `examples/wasip2_run.rs` builds the program with `rustc --extern
  wasm2rs_wasi=<rlib>`. The tests do the same: `tests/it/wasip2_typed_host.rs` has `build_program`
  (glue + a `main` that makes `Instance::new(Glue(host))`, linked through
  `common::build_with_wasi`, which builds the rlib once per run), and each
  `tests/it/wasip2_typed_*.rs` builds a component from WIT and a core module from WAT.
- Configuration is by builder methods on `WasiHost`: `with_arguments`, `with_preopen` (read-only
  directory shown to the guest as `/`), `with_writable_preopen`, `with_external_network` (lifts
  the loopback-only rule). `wasip2_run` maps `$WASIP2_PREOPEN` and `$WASIP2_PREOPEN_WRITABLE=1`
  to the first two; it never calls `with_external_network`. A fuller configuration surface is
  planned (P2 of the library design).
- The host runs one guest call at a time and is single-threaded except for the stdin thread.

## Handles and tables

- A resource handle is the index of a slot in a `Slab<T>`: `insert` reuses the most recently
  freed slot, `remove` frees one. Dropping a descriptor, file input/output stream,
  directory-entry stream, TCP/UDP socket, datagram stream, resolve-address stream, connection
  stream or `error` frees its slot. A **pollable keeps its slot** (`resource-drop` of a
  pollable is a no-op; the `pollables` table only grows). `network` and terminal resources have
  no state.
- A guest must not use a handle after dropping it. The host does not detect that: a stale handle
  may fail, succeed, or name a newer resource once the slot is reused.
- Fixed handles: input stream 0 is stdin; output streams 1 and 2 are stdout and stderr.
  A file's streams are `FIRST_FILE_STREAM (3) + slot` in `files` (input) or `outputs` (output).
  A connection's streams are `TCP_STREAM + slot` in `tcp_in` / `tcp_out`.
  Descriptor 0 is the preopen and is never freed.

## Stream model

- `read_stream` is the one non-blocking read path (`read`, `blocking-read`, `skip`,
  `blocking-skip`, `splice`, `blocking-splice`), at most 4096 bytes per call.
  `read_stream_waiting` adds the waiting for the blocking variants: it waits (like the
  stream's `subscribe` pollable) on stdin and connections; a file never waits.
- A **zero-length** read/skip is `ok` and empty while the stream is open and `closed` at its end;
  it consumes nothing (stdin: buffer empty and eof; connection: a one-byte `peek`; file:
  position compared with the file's metadata length). The blocking variants first wait until the
  stream is readable.
- **Files.** A file stream is a `FileStream` over a duplicate (`try_clone`) of the descriptor's
  `File`. Duplicates share an offset, so each stream keeps its own position and seeks before
  every read or write. The descriptor remembers whether it was opened for `read` and `write`:
  `read-via-stream` needs `read` (`bad-descriptor` otherwise; a directory is `is-directory`),
  `get-flags` reports those two flags, and an `open-at` with no flags still reads.
- **TCP input** is a non-blocking duplicate of the connection. `read` returns an empty list when
  nothing has arrived; EOF is `closed`; any other I/O error is a failure (see below).
- **Stdin** is read by one thread, started on first use, that fills a buffer
  (`SharedStdin`: bytes, eof flag, optional error) under a mutex and notifies a condition
  variable. `read` therefore never blocks. A read error ends the input: the buffered bytes are
  delivered first, then the error is reported once. `poll`/`block` wait on that condition
  variable with a timeout at the earliest clock deadline, and re-check every millisecond while a
  socket pollable is among them (sockets have nothing to wake the wait).
- **Output** goes to `std` stdout/stderr (`write_all`), a file stream, or a connection's send
  queue. `check-write` is 4096 for everything except a connection. All four write methods trap (a Rust
  `assert!`) when asked for more than 4096 bytes. For a connection, `write` and `write-zeroes`
  also trap above the current `check-write`; `blocking-write-and-flush` and
  `blocking-write-zeroes-and-flush` only check the 4096 limit, queue the bytes and wait, so the
  64 KiB queue limit is soft for them (up to 64 KiB + 4096 bytes are held briefly).
- **splice / blocking-splice** move at most `min(len, 4096)` bytes (for a connection at most the
  permit). A dropped output is `closed` before anything is read. `splice` never waits for the
  source; `blocking-splice` waits for at least one byte (for a connection it first waits for the
  send queue to empty, then takes up to the permit). The bytes are queued like `write`'s.

## The TCP send queue and back-pressure

`TcpOut` (one per connection output stream) holds the non-blocking socket and a
`VecDeque<u8>` queue of at most `SEND_QUEUE_CAPACITY` = 64 KiB.

- `write` appends and then drains as far as the socket takes. **It never waits.**
- `check-write` drains first and returns `min(4096, room)`: 0 when the queue is full, and 0
  after a `flush` that left bytes queued, until they are all sent.
- The output-stream `subscribe` (`Wait::TcpWritable`) is ready while the permit is not 0, or
  when the stream failed or was dropped. So a guest that obeys `check-write` or polls never
  blocks on a peer that does not read, and a guest that calls `write`/`write-zeroes` over the permit traps.
- `flush` drains once and marks the flush pending if bytes remain. `blocking-flush`,
  `blocking-write-and-flush` and `blocking-write-zeroes-and-flush` empty the queue first,
  **waiting** (1 ms sleeps) for the peer; they do not return while a live peer never reads.
  The queue is never reordered, so the bytes stay in order.
- Dropping a connection's output stream, and `shutdown` of the send side (`send` or `both`),
  flush the queue first without a deadline. `shutdown` finds the queue through the slot and
  unique `id` kept in `NetState::Connected { out: (slot, id) }`, so a slot reused by another
  connection is not flushed by mistake.
- `exit` and `Drop for WasiHost` (`flush_all_connections`) flush every connection's queue,
  waiting up to **5 seconds in all** (one shared deadline). `wasip2_run` drops the instance
  before a normal exit; after a trap (exit code 134) it does not.
- The socket options keep-alive (enabled, idle time, interval, count), hop limit and receive/send
  buffer sizes are recorded per socket with defaults, validated (zero is `invalid-argument`),
  read back and inherited by accepted sockets, but not applied to the OS socket. The listen
  backlog size is only validated: zero is `invalid-argument`, `invalid-state` once a connection
  is started or made, and the value is ignored (not stored, read back or inherited). UDP options
  answer `not-supported`.

## Error reporting contract

- A failed stream operation is `stream-error::last-operation-failed(error)` with a live
  `wasi:io/error` resource, as the spec says. The message is stored in the `errors` slab;
  `[method]error.to-debug-string` returns it (empty for an unknown handle) and `resource-drop`
  of the `error` frees the slot. Every report makes a fresh handle that the guest drops.
- It is reported **once**, by the first operation that finds the failure; after that the stream
  is `closed` for every operation, including a zero-length read and the readiness of
  `subscribe` (a failed stream stays ready).
- Reported this way: a TCP failure (a reset, or a send queue the socket could not take: the
  queue is discarded), a read error on a connection or a file, a failed write or flush of
  stdout, stderr or a file, and a stdin read error (after the bytes read before it).
- Not failures, always `closed`: the end of a stream (EOF, an orderly hang-up) and a dropped
  stream. Per-stream "already reported" state lives in `failed_in` / `failed_out` (and in
  `TcpOut::reported` for connections); dropping the stream forgets it.
- Filesystem calls return `error-code` values instead (`bad-descriptor`, `is-directory`,
  `read-only`, ...). `open-at` with `create` + `exclusive` on a symlink name is `exist`
  (never follows, dangling or not); without `symlink-follow` a symlink as the last component is
  `loop`.

## Known limitations and open items

- `splice`: the bytes already read from the source are lost if the write then fails.
- Reads from non-regular files are not handled specially: a file read is a blocking `read` on
  the open `File`, so a FIFO or character device with nothing to read blocks the host; a
  zero-length read decides "end" from the file's metadata length (0 for such files).
  Read directly from the code; there is no test.
- Preopen confinement is done with `cap-std`: a directory descriptor holds the open directory
  and every path is resolved below it in one step (`openat` with `O_NOFOLLOW` per component), so
  a symlink swapped in after a check cannot lead out of a preopen, and a descriptor stays on the
  directory it opened when that directory is renamed or one of its ancestors becomes a symlink.
  What remains: (1) a symlink whose target is absolute is refused even when it points inside
  the preopen (the sandbox cannot tell); (2) `link-at` with `symlink-follow` and `rename-at` /
  `symlink-at` pre-checks (is the entry a symlink, is the target a directory) are separate steps
  from the operation, so a racing swap can change which in-preopen entry is affected or the
  error code, never leave the preopen; (3) a hard link made by the guest or any other process
  inside the preopen to a file outside stays a link to that file (no host can prevent it); (4)
  a symlink is followed only within the directory descriptor it is resolved from, so
  `sub/up -> ../f` is `not-permitted` through a descriptor for `sub` although it stays inside the
  preopen (as in wasmtime); (5) the confinement is for Unix (`openat`); other platforms are not tested.
- The TCP socket options are not applied to the OS socket. UDP socket options are
  `not-supported`. Only `std::net` is used (no `rustix`), so nothing finer is possible.
- `wasip2_run` never allows addresses outside loopback and does not flush the send queues after
  a trap.
- The stdin thread is never joined, and a read blocked inside the reader cannot be cancelled. It
  is started lazily (only when the guest uses stdin) and holds the stdin buffer weakly, so after
  the host is dropped it exits as soon as that pending read returns; until then it lingers.
- Waiting for a peer (flushes, `blocking-splice` on a connection, `shutdown`, dropping an output
  stream) polls with 1 ms sleeps and has no deadline except in `exit`/drop (5 s). A peer that
  stays alive and never reads blocks the guest there.
- `std` binds and listens in one call, so `finish-bind` does both; a bound socket cannot connect
  (`not-supported`) and `start-connect` connects at once.

## Where the behaviour is tested

| Area | Tests (`tests/it/`) |
| --- | --- |
| stdio, environment, exit, arguments | `wasip2_typed_host.rs`, `wasip2_typed_cli.rs`, `wasip2_typed_clocks.rs` |
| stdin and its errors | `wasip2_typed_stdin.rs` |
| `wasi:random` | `wasip2_typed_random.rs` |
| streams, splice, permit | `wasip2_typed_streams.rs`, `wasip2_typed_splice.rs`, `wasip2_typed_splice_permit.rs` |
| send queue, flush, shutdown, failed peer | `wasip2_typed_send_queue.rs`, `wasip2_typed_io_error.rs` |
| files and symlinks | `wasip2_typed_fs.rs`, `wasip2_typed_fs_write.rs`, `wasip2_typed_fs_edges.rs`, `wasip2_typed_fs_identity.rs`, `wasip2_typed_fs_symlink.rs` |
| sockets, options, UDP, lookup | `wasip2_typed_sockets.rs`, `wasip2_typed_tcp_options.rs`, `wasip2_typed_udp.rs`, `wasip2_typed_name_lookup.rs` |
| both filesystem and sockets (two `ErrorCode` enums) | `wasip2_typed_both.rs` |
