// A minimal WASI preview2 host for the lowered imports of a `wasm32-wasip2`
// program (`println!`, arguments, environment, exit, stdin, `poll`, clocks). It is source text appended to
// the generated code (see `examples/wasip2_run.rs`), not a crate: the `Imports` trait
// it implements is generated per module. Build the module with `named_imports`,
// `named_exports` and `reentrant_imports`: the methods are named after the imports,
// and each receives the guest `Instance`, so the host reads and writes the guest's
// linear memory through pointers and calls its `cabi_realloc` to build the lists
// and strings it returns.
//
// The trait has one method per import of the module, so a method for an import the
// module does not have is an error. Blocks between `// optional: <method>` and
// `// end optional` are kept only when the generated trait declares `<method>`
// (see `keep_declared_methods` in `examples/wasip2_run.rs`).
//
// Handles: 0 = stdin, 1 = stdout, 2 = stderr; terminals are placeholders. A pollable
// is an index into `pollables`: `None` is always ready (streams), `Some(deadline)`
// becomes ready at the deadline (clocks); `poll` and `block` sleep until then.
// `read` blocks on the real stdin, so an input stream is always ready too.
// Random numbers come from a small non-cryptographic generator (splitmix64) seeded
// from the OS-seeded `std` hasher: fine for `HashMap` seeds and examples, not for
// keys or tokens.
// Not implemented: files, sockets.

use std::time::{Duration, Instant};

struct P2Host {
    /// Read only by the optional `get-arguments` block.
    #[allow(dead_code)]
    args: Vec<String>,
    env: Vec<(String, String)>,
    pollables: Vec<Option<Instant>>,
    /// The monotonic clock's zero; read only by the optional clock blocks.
    #[allow(dead_code)]
    start: Instant,
    /// The generator state; read only by the optional `wasi:random` blocks.
    #[allow(dead_code)]
    rng: u64,
}

type Guest = Instance<P2Host>;

impl P2Host {
    fn new(args: Vec<String>, env: Vec<(String, String)>) -> Self {
        P2Host { args, env, pollables: Vec::new(), start: Instant::now(), rng: os_seed() }
    }

    /// The next 64 random bits (splitmix64).
    #[allow(dead_code)]
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A `list<u8>` of `len` random bytes at `ret`: `(ptr, len)`.
    #[allow(dead_code)]
    fn random_bytes(&mut self, g: &mut Guest, len: i64, ret: i32) {
        let len = u32::try_from(len as u64).expect("random bytes: length over 4 GiB") as usize;
        let mut bytes = Vec::with_capacity(len + 8);
        while bytes.len() < len {
            bytes.extend_from_slice(&self.next_u64().to_le_bytes());
        }
        bytes.truncate(len);
        let ptr = if len == 0 { 4 } else { alloc(g, 1, len as i32) };
        put(g, ptr, &bytes);
        put_u32(g, ret, ptr);
        put_u32(g, ret + 4, len as i32);
    }

    /// A new pollable that is ready at `deadline` (always, for `None`).
    fn subscribe(&mut self, deadline: Option<Instant>) -> i32 {
        self.pollables.push(deadline);
        self.pollables.len() as i32 - 1
    }

    /// The pollable's deadline: `None` if it is always ready.
    fn deadline(&self, pollable: u32) -> Option<Instant> {
        self.pollables[pollable as usize]
    }

    /// Sleeps until the pollable is ready.
    fn block_on(&self, pollable: i32) {
        if let Some(deadline) = self.deadline(pollable as u32) {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

/// A seed from the OS: `RandomState` is keyed from the system's randomness.
#[allow(dead_code)]
fn os_seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(0u8)
}

fn put(g: &mut Guest, at: i32, bytes: &[u8]) {
    let at = at as u32 as usize;
    g.memory()[at..at + bytes.len()].copy_from_slice(bytes);
}

fn put_u32(g: &mut Guest, at: i32, value: i32) {
    put(g, at, &value.to_le_bytes());
}

fn get_u32(g: &mut Guest, at: i32) -> u32 {
    let at = at as u32 as usize;
    u32::from_le_bytes(g.memory()[at..at + 4].try_into().unwrap())
}

/// Memory the guest owns, from its own allocator (`cabi_realloc(0, 0, align, size)`).
fn alloc(g: &mut Guest, align: i32, size: i32) -> i32 {
    g.cabi_realloc(0, 0, align, size)
}

/// A string copied into guest memory, with its `(ptr, len)` written at `at`.
fn put_string(g: &mut Guest, at: i32, s: &str) {
    let ptr = alloc(g, 1, s.len() as i32);
    put(g, ptr, s.as_bytes());
    put_u32(g, at, ptr);
    put_u32(g, at + 4, s.len() as i32);
}

/// A `list<_>` of `count` elements of `size` bytes at `ret`: `(ptr, len)`. Returns
/// the elements' address for the caller to fill.
fn guest_list(g: &mut Guest, ret: i32, size: i32, count: i32) -> i32 {
    let ptr = if count == 0 { 4 } else { alloc(g, 4, size * count) };
    put_u32(g, ret, ptr);
    put_u32(g, ret + 4, count);
    ptr
}

/// A zero tag byte at `ptr`: `result::ok` or `option::none`.
fn zero_tag(g: &mut Guest, ptr: i32) {
    put(g, ptr, &[0]);
}

fn stream_write(g: &mut Guest, stream: i32, ptr: i32, len: i32) {
    use std::io::Write;
    let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
    let bytes = &g.memory()[ptr..ptr + len];
    match stream {
        2 => std::io::stderr().write_all(bytes).unwrap(),
        _ => std::io::stdout().write_all(bytes).unwrap(),
    }
}

impl Imports for P2Host {
    fn wasi_io_error_resource_drop_error(&mut self, _g: &mut Guest, _handle: i32) {}
    fn wasi_io_poll_resource_drop_pollable(&mut self, _g: &mut Guest, _handle: i32) {}
    fn wasi_io_streams_resource_drop_input_stream(&mut self, _g: &mut Guest, _handle: i32) {}
    fn wasi_io_streams_resource_drop_output_stream(&mut self, _g: &mut Guest, _handle: i32) {}
    fn wasi_cli_terminal_input_resource_drop_terminal_input(
        &mut self,
        _g: &mut Guest,
        _handle: i32,
    ) {
    }
    fn wasi_cli_terminal_output_resource_drop_terminal_output(
        &mut self,
        _g: &mut Guest,
        _handle: i32,
    ) {
    }

    // optional: wasi_cli_environment_get_arguments
    /// `list<string>` at `ret`: 8-byte `(ptr, len)` elements.
    fn wasi_cli_environment_get_arguments(&mut self, g: &mut Guest, ret: i32) {
        let items = guest_list(g, ret, 8, self.args.len() as i32);
        for (i, arg) in self.args.iter().enumerate() {
            put_string(g, items + 8 * i as i32, arg);
        }
    }
    // end optional

    /// `list<tuple<string, string>>` at `ret`: 16-byte `(key, value)` elements.
    fn wasi_cli_environment_get_environment(&mut self, g: &mut Guest, ret: i32) {
        let items = guest_list(g, ret, 16, self.env.len() as i32);
        for (i, (key, value)) in self.env.iter().enumerate() {
            let at = items + 16 * i as i32;
            put_string(g, at, key);
            put_string(g, at + 8, value);
        }
    }

    /// `exit(status: result)`: 0 is `ok`, 1 is `error`.
    fn wasi_cli_exit_exit(&mut self, _g: &mut Guest, status: i32) {
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::process::exit(status);
    }

    fn wasi_io_poll_method_pollable_block(&mut self, _g: &mut Guest, pollable: i32) {
        self.block_on(pollable);
    }

    /// `poll(in: list<borrow<pollable>>) -> list<u32>`: the indices of the ready
    /// pollables, sleeping until the earliest deadline if none is ready yet. An empty
    /// list traps, as the interface specifies.
    fn wasi_io_poll_poll(&mut self, g: &mut Guest, list: i32, len: i32, ret: i32) {
        assert!(len > 0, "wasi:io/poll.poll: empty list");
        let handles: Vec<u32> = (0..len).map(|i| get_u32(g, list + 4 * i)).collect();
        let ready = loop {
            let now = Instant::now();
            let ready: Vec<i32> = (0..len)
                .filter(|&i| self.deadline(handles[i as usize]).is_none_or(|d| d <= now))
                .collect();
            if !ready.is_empty() {
                break ready;
            }
            let earliest = handles.iter().filter_map(|&h| self.deadline(h)).min().unwrap();
            std::thread::sleep(earliest.saturating_duration_since(now));
        };
        let items = guest_list(g, ret, 4, ready.len() as i32);
        for (i, index) in ready.iter().enumerate() {
            put_u32(g, items + 4 * i as i32, *index);
        }
    }

    /// `result<u64, stream-error>` at `ret`: `ok` with a fixed write budget.
    fn wasi_io_streams_method_output_stream_check_write(
        &mut self,
        g: &mut Guest,
        _stream: i32,
        ret: i32,
    ) {
        zero_tag(g, ret);
        put(g, ret + 8, &4096u64.to_le_bytes());
    }

    fn wasi_io_streams_method_output_stream_write(
        &mut self,
        g: &mut Guest,
        stream: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        stream_write(g, stream, ptr, len);
        zero_tag(g, ret);
    }

    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        g: &mut Guest,
        _stream: i32,
        ret: i32,
    ) {
        use std::io::Write;
        std::io::stdout().flush().ok();
        zero_tag(g, ret);
    }

    fn wasi_io_streams_method_output_stream_subscribe(
        &mut self,
        _g: &mut Guest,
        _stream: i32,
    ) -> i32 {
        self.subscribe(None)
    }

    // optional: wasi_io_streams_method_input_stream_read
    /// `result<list<u8>, stream-error>` at `ret`: the tag, then the list or, for the
    /// `closed` error at end of input (or a read error), the error's tag byte, both
    /// at `ret + 4`. It blocks until at least one byte (or end of input) is available.
    fn wasi_io_streams_method_input_stream_read(
        &mut self,
        g: &mut Guest,
        _stream: i32,
        len: i64,
        ret: i32,
    ) {
        use std::io::Read;
        // `len` is a `u64`: a negative `i64` is a huge length, and 0 is an empty list.
        let want = (len as u64).min(4096) as usize;
        let mut buf = vec![0u8; want];
        let n = if want == 0 {
            0
        } else {
            match std::io::stdin().read(&mut buf) {
                Ok(0) | Err(_) => {
                    put(g, ret, &[1]); // result::err
                    put(g, ret + 4, &[1]); // stream-error::closed
                    return;
                }
                Ok(n) => n,
            }
        };
        zero_tag(g, ret);
        let ptr = if n == 0 { 4 } else { alloc(g, 1, n as i32) };
        put(g, ptr, &buf[..n]);
        put_u32(g, ret + 4, ptr);
        put_u32(g, ret + 8, n as i32);
    }
    // end optional

    // optional: wasi_io_streams_method_input_stream_subscribe
    fn wasi_io_streams_method_input_stream_subscribe(&mut self, _g: &mut Guest, _stream: i32) -> i32 {
        self.subscribe(None)
    }
    // end optional

    fn wasi_cli_stdin_get_stdin(&mut self, _g: &mut Guest) -> i32 {
        0
    }
    fn wasi_cli_stdout_get_stdout(&mut self, _g: &mut Guest) -> i32 {
        1
    }
    fn wasi_cli_stderr_get_stderr(&mut self, _g: &mut Guest) -> i32 {
        2
    }

    /// `option<terminal>` at `ret`: `none`, the program is not attached to one.
    fn wasi_cli_terminal_stdin_get_terminal_stdin(&mut self, g: &mut Guest, ret: i32) {
        zero_tag(g, ret);
    }
    fn wasi_cli_terminal_stdout_get_terminal_stdout(&mut self, g: &mut Guest, ret: i32) {
        zero_tag(g, ret);
    }
    fn wasi_cli_terminal_stderr_get_terminal_stderr(&mut self, g: &mut Guest, ret: i32) {
        zero_tag(g, ret);
    }

    fn wasi_clocks_monotonic_clock_subscribe_duration(
        &mut self,
        _g: &mut Guest,
        when: i64,
    ) -> i32 {
        let deadline = Instant::now() + Duration::from_nanos(when as u64);
        self.subscribe(Some(deadline))
    }

    // optional: wasi_clocks_monotonic_clock_subscribe_instant
    fn wasi_clocks_monotonic_clock_subscribe_instant(&mut self, _g: &mut Guest, when: i64) -> i32 {
        let deadline = self.start + Duration::from_nanos(when as u64);
        self.subscribe(Some(deadline))
    }
    // end optional

    // optional: wasi_clocks_monotonic_clock_now
    /// Nanoseconds since the host was created.
    fn wasi_clocks_monotonic_clock_now(&mut self, _g: &mut Guest) -> i64 {
        self.start.elapsed().as_nanos() as i64
    }
    // end optional

    // optional: wasi_clocks_wall_clock_now
    /// `datetime` at `ret`: seconds (`u64`), then nanoseconds (`u32`), since the epoch.
    fn wasi_clocks_wall_clock_now(&mut self, g: &mut Guest, ret: i32) {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        put(g, ret, &since.as_secs().to_le_bytes());
        put_u32(g, ret + 8, since.subsec_nanos() as i32);
    }
    // end optional

    // optional: wasi_random_random_get_random_bytes
    fn wasi_random_random_get_random_bytes(&mut self, g: &mut Guest, len: i64, ret: i32) {
        self.random_bytes(g, len, ret);
    }
    // end optional

    // optional: wasi_random_random_get_random_u64
    fn wasi_random_random_get_random_u64(&mut self, _g: &mut Guest) -> i64 {
        self.next_u64() as i64
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_bytes
    fn wasi_random_insecure_get_insecure_random_bytes(
        &mut self,
        g: &mut Guest,
        len: i64,
        ret: i32,
    ) {
        self.random_bytes(g, len, ret);
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_u64
    fn wasi_random_insecure_get_insecure_random_u64(&mut self, _g: &mut Guest) -> i64 {
        self.next_u64() as i64
    }
    // end optional

    // optional: wasi_random_insecure_seed_insecure_seed
    /// `tuple<u64, u64>` at `ret`.
    fn wasi_random_insecure_seed_insecure_seed(&mut self, g: &mut Guest, ret: i32) {
        let (a, b) = (self.next_u64(), self.next_u64());
        put(g, ret, &a.to_le_bytes());
        put(g, ret + 8, &b.to_le_bytes());
    }
    // end optional
}
