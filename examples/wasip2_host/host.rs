// A minimal WASI preview2 host for the lowered imports of a `wasm32-wasip2`
// program (`println!`, arguments, environment, exit). It is source text appended to
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
// Not implemented: files, sockets, random.

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
}

type Guest = Instance<P2Host>;

impl P2Host {
    fn new(args: Vec<String>, env: Vec<(String, String)>) -> Self {
        P2Host { args, env, pollables: Vec::new(), start: Instant::now() }
    }

    /// A new pollable that is ready at `deadline` (always, for `None`).
    fn subscribe(&mut self, deadline: Option<Instant>) -> i32 {
        self.pollables.push(deadline);
        self.pollables.len() as i32 - 1
    }

    /// Sleeps until the pollable is ready.
    fn block_on(&self, pollable: i32) {
        if let Some(deadline) = self.pollables[pollable as u32 as usize] {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

fn put(g: &mut Guest, at: i32, bytes: &[u8]) {
    let at = at as u32 as usize;
    g.memory()[at..at + bytes.len()].copy_from_slice(bytes);
}

fn put_u32(g: &mut Guest, at: i32, value: i32) {
    put(g, at, &value.to_le_bytes());
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
        let handles: Vec<usize> = (0..len)
            .map(|i| {
                let at = list as u32 as usize + 4 * i as usize;
                u32::from_le_bytes(g.memory()[at..at + 4].try_into().unwrap()) as usize
            })
            .collect();
        let ready = loop {
            let now = Instant::now();
            let ready: Vec<i32> = (0..len)
                .filter(|&i| self.pollables[handles[i as usize]].is_none_or(|d| d <= now))
                .collect();
            if !ready.is_empty() {
                break ready;
            }
            let earliest = handles.iter().filter_map(|&h| self.pollables[h]).min().unwrap();
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
    /// `closed` error at end of input, the error's tag byte, both at `ret + 4`. It
    /// blocks until at least one byte (or end of input) is available.
    fn wasi_io_streams_method_input_stream_read(
        &mut self,
        g: &mut Guest,
        _stream: i32,
        len: i64,
        ret: i32,
    ) {
        use std::io::Read;
        let mut buf = vec![0u8; len.clamp(0, 4096) as usize];
        let n = std::io::stdin().read(&mut buf).unwrap_or(0);
        if n == 0 {
            put(g, ret, &[1]);
            put(g, ret + 4, &[1]);
        } else {
            zero_tag(g, ret);
            let ptr = alloc(g, 1, n as i32);
            put(g, ptr, &buf[..n]);
            put_u32(g, ret + 4, ptr);
            put_u32(g, ret + 8, n as i32);
        }
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
}
