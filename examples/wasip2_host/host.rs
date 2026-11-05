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
// Handles: 0 = stdin, 1 = stdout, 2 = stderr; pollables and terminals are
// placeholders, and every pollable is always ready.
// Not implemented: stdin reads, files, clocks.

/// The pollable every `subscribe` returns; it is always ready.
const PLACEHOLDER_POLLABLE: i32 = 3;

struct P2Host {
    /// Read only by the optional `get-arguments` block.
    #[allow(dead_code)]
    args: Vec<String>,
    env: Vec<(String, String)>,
}

type Guest = Instance<P2Host>;

impl P2Host {
    fn new(args: Vec<String>, env: Vec<(String, String)>) -> Self {
        P2Host { args, env }
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

    fn wasi_io_poll_method_pollable_block(&mut self, _g: &mut Guest, _pollable: i32) {}

    /// `poll(in: list<borrow<pollable>>) -> list<u32>`: every pollable is ready, so
    /// the result is the indices `0..len`.
    /// An empty list traps, as the interface specifies.
    fn wasi_io_poll_poll(&mut self, g: &mut Guest, _list: i32, len: i32, ret: i32) {
        assert!(len > 0, "wasi:io/poll.poll: empty list");
        let items = guest_list(g, ret, 4, len);
        for i in 0..len {
            put_u32(g, items + 4 * i, i);
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
        PLACEHOLDER_POLLABLE
    }

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
        _when: i64,
    ) -> i32 {
        PLACEHOLDER_POLLABLE
    }
}
