# The typed WASI preview2 host: design note

This note describes `examples/wasip2_host/typed.rs` as it is now: a WASI preview2 host written
against the typed `Host` trait that `wasm2rs --wit-host --glue` generates. It is an example, not
library API. For how the component was analysed and how the host came about, see
[component-model-poc.md](component-model-poc.md); for the `--wit-host` / `--glue` flags, see the
README. Each section names the code or test that backs it.

## Structure

- `examples/wasip2_host/typed.rs` is **source text**, not a compiled module. It has no
  `mod`/`use` header of its own: it is appended to the generated code (the transpiled core
  module, then the `Host` trait and `Glue` from `wit_host_glue`, then the `type_aliases`, then
  this file, then a `main`). `TypedHost` implements the generated `Host` trait; the glue does
  all canonical-ABI work.
- `examples/wasip2_host/filter.rs` holds the pure string helpers that shape that source:
  - `keep_declared_methods(host, generated)` drops every `// optional: <method>` ...
    `// end optional` block unless the generated trait declares `fn <method>(`. A marker
    `// optional: a | b` keeps its block if any listed method is declared (used for helpers and
    types shared by several methods).
  - `type_aliases` emits `type FsErrorCode = ..;` and `type NetErrorCode = ..;`, using
    `error_type_of` to find the type name the generator chose.
    A component importing both filesystem and sockets gets two `error-code` enums that the
    generator spells `ErrorCode` and `ErrorCode2` in import order; the host only uses the aliases.
    `NetErrorCode` is found from tcp, udp or ip-name-lookup.
- **Trimmed builds.** A component that imports only `println!` things gets a host without
  filesystem, socket, UDP or lookup code, because the generated trait lacks those methods. A
  component that imports a function `typed.rs` has no block for fails to build with rustc's
  E0046 (missing trait items).
- **Consumers.** `examples/wasip2_run.rs` embeds the host with
  `include_str!("wasip2_host/typed.rs")` and `include!("wasip2_host/filter.rs")`. The tests do
  the same: `tests/it/wasip2_typed_host.rs` has `const HOST: &str = include_str!(..)` plus
  `build_program` (glue + aliases + trimmed host + a `main` that makes
  `Instance::new(Glue(host))`), and each `tests/it/wasip2_typed_*.rs` builds a component from WIT
  and a core module from WAT. `tests/it/wasip2_typed_filter.rs` tests `filter.rs` directly.
- Configuration is by builder methods on `TypedHost`: `with_arguments`, `with_preopen` (read-only
  directory shown to the guest as `/`), `with_writable_preopen`, `with_external_network` (lifts
  the loopback-only rule). `wasip2_run` maps `$WASIP2_PREOPEN` and `$WASIP2_PREOPEN_WRITABLE=1`
  to the first two; it never calls `with_external_network`.
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
- `exit` and `Drop for TypedHost` (`flush_all_connections`) flush every connection's queue,
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
- A pollable's table slot is never freed; a program that subscribes in an endless loop grows it.
- Directory descriptors are resolved from their path on every use; the check and the use are
  separate steps, so a guest racing a host-side swap of a directory for a symlink can escape the
  preopen. Preopen only a directory you trust the program with.
- The TCP socket options are not applied to the OS socket. UDP socket options are
  `not-supported`. Only `std::net` is used (no `rustix`), so nothing finer is possible.
- `wasip2_run` never allows addresses outside loopback and does not flush the send queues after
  a trap.
- The stdin thread is never joined; a blocked stdin read cannot be cancelled.
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
| filter helpers, both interfaces | `wasip2_typed_filter.rs`, `wasip2_typed_both.rs` |
