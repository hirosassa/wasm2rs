// A minimal WASI preview2 host for the lowered imports of a `wasm32-wasip2`
// hello-world core module (`println!` and exit). It is source text appended to the
// generated code (see `examples/wasip2_run.rs`), not a crate: the `Imports` trait it
// implements is generated per module. Build the module with `named_imports` and
// `import_memory`, so the methods are named after the imports and this host owns the
// guest's linear memory, which the lowered functions read and write through
// pointers.
//
// The trait has one method per import of the module, so a method for an import the
// module does not have is an error. Blocks between `// optional: <method>` and
// `// end optional` are kept only when the generated trait declares `<method>`
// (see `keep_declared_methods` in `examples/wasip2_run.rs`).
//
// Handles: 0 = stdin, 1 = stdout, 2 = stderr; pollables and terminals are placeholders.
// Not implemented: `poll` (it returns a list, which needs the guest's `cabi_realloc`,
// and the host cannot call guest functions yet), stdin reads, files, clocks.

struct P2Host {
    mem: Vec<u8>,
}

impl P2Host {
    /// A host lending `pages` 64 KiB pages: at least the module's declared minimum.
    fn new(pages: usize) -> Self {
        P2Host { mem: vec![0; pages * 65536] }
    }

    fn put(&mut self, at: i32, bytes: &[u8]) {
        let at = at as u32 as usize;
        self.mem[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// `result<_, _>` at `ptr`: the `ok` case is a zero tag byte.
    fn ok(&mut self, ptr: i32) {
        self.put(ptr, &[0]);
    }

    fn stream_write(&mut self, stream: i32, ptr: i32, len: i32) {
        use std::io::Write;
        let (ptr, len) = (ptr as u32 as usize, len as u32 as usize);
        let bytes = &self.mem[ptr..ptr + len];
        match stream {
            2 => std::io::stderr().write_all(bytes).unwrap(),
            _ => std::io::stdout().write_all(bytes).unwrap(),
        }
    }
}

impl Imports for P2Host {
    fn memory(&self) -> &[u8] {
        &self.mem
    }
    fn memory_mut(&mut self) -> &mut Vec<u8> {
        &mut self.mem
    }

    fn wasi_io_error_resource_drop_error(&mut self, _handle: i32) {}
    fn wasi_io_poll_resource_drop_pollable(&mut self, _handle: i32) {}
    fn wasi_io_streams_resource_drop_input_stream(&mut self, _handle: i32) {}
    fn wasi_io_streams_resource_drop_output_stream(&mut self, _handle: i32) {}
    fn wasi_cli_terminal_input_resource_drop_terminal_input(&mut self, _handle: i32) {}
    fn wasi_cli_terminal_output_resource_drop_terminal_output(&mut self, _handle: i32) {}

    // optional: wasi_cli_environment_get_arguments
    /// `list<string>` at `ret`: an empty list, `(ptr, len)`.
    fn wasi_cli_environment_get_arguments(&mut self, ret: i32) {
        self.put(ret, &4u32.to_le_bytes());
        self.put(ret + 4, &0u32.to_le_bytes());
    }
    // end optional

    /// `list<tuple<string, string>>` at `ret`: an empty list, `(ptr, len)`.
    fn wasi_cli_environment_get_environment(&mut self, ret: i32) {
        self.put(ret, &4u32.to_le_bytes());
        self.put(ret + 4, &0u32.to_le_bytes());
    }

    /// `exit(status: result)`: 0 is `ok`, 1 is `error`.
    fn wasi_cli_exit_exit(&mut self, status: i32) {
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::process::exit(status);
    }

    fn wasi_io_poll_method_pollable_block(&mut self, _pollable: i32) {}
    fn wasi_io_poll_poll(&mut self, _list: i32, _len: i32, _ret: i32) {
        panic!("wasi:io/poll.poll is not implemented: it returns a list through cabi_realloc");
    }

    /// `result<u64, stream-error>` at `ret`: `ok` with a fixed write budget.
    fn wasi_io_streams_method_output_stream_check_write(&mut self, _stream: i32, ret: i32) {
        self.ok(ret);
        self.put(ret + 8, &4096u64.to_le_bytes());
    }

    fn wasi_io_streams_method_output_stream_write(
        &mut self,
        stream: i32,
        ptr: i32,
        len: i32,
        ret: i32,
    ) {
        self.stream_write(stream, ptr, len);
        self.ok(ret);
    }

    fn wasi_io_streams_method_output_stream_blocking_flush(&mut self, _stream: i32, ret: i32) {
        use std::io::Write;
        std::io::stdout().flush().ok();
        self.ok(ret);
    }

    fn wasi_io_streams_method_output_stream_subscribe(&mut self, _stream: i32) -> i32 {
        3
    }

    fn wasi_cli_stdin_get_stdin(&mut self) -> i32 {
        0
    }
    fn wasi_cli_stdout_get_stdout(&mut self) -> i32 {
        1
    }
    fn wasi_cli_stderr_get_stderr(&mut self) -> i32 {
        2
    }

    /// `option<terminal>` at `ret`: `none`, the program is not attached to one.
    fn wasi_cli_terminal_stdin_get_terminal_stdin(&mut self, ret: i32) {
        self.ok(ret);
    }
    fn wasi_cli_terminal_stdout_get_terminal_stdout(&mut self, ret: i32) {
        self.ok(ret);
    }
    fn wasi_cli_terminal_stderr_get_terminal_stderr(&mut self, ret: i32) {
        self.ok(ret);
    }

    fn wasi_clocks_monotonic_clock_subscribe_duration(&mut self, _when: i64) -> i32 {
        3
    }
}
