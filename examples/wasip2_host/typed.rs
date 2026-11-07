// A WASI preview2 host written against the typed `Host` trait that
// `wasm2rs --wit-host --glue` generates from a component. It has no canonical-ABI
// code: strings, lists, results and resources arrive as Rust values, and the generated
// `Glue` lowers them to the module's flat `Imports`. It is source text appended to the
// generated code (the `Host` trait and its types come from the component), covering
// the interfaces a `println!` program uses: stdout, the environment, exit, output
// streams and pollables, plus stdin (always at its end), stderr and the terminal
// queries (not a terminal), which a std program asks for at startup. Handles: 0 = stdin,
// 1 = stdout, 2 = stderr; a pollable is an index into `pollables`: ready at once, or
// at the deadline a clock subscription gave it.
//
// Like `host.rs`, each method sits between `// optional: <method>` and `// end optional`
// and is kept only when the generated trait declares it (see `keep_declared_methods` in
// `filter.rs`), so one source serves components that import different subsets. A
// component that imports a method this host lacks does not build.

#[allow(unused_imports)]
use std::io::Write;
#[allow(unused_imports)]
use std::time::{Duration, Instant};

#[allow(dead_code)]
fn os_seed() -> u64 {
    use std::hash::BuildHasher;
    std::collections::hash_map::RandomState::new().hash_one(0u8)
}

/// Blocks until `deadline`.
#[allow(dead_code)]
fn sleep_until(deadline: Instant) {
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
}

#[allow(dead_code)]
struct TypedHost {
    env: Vec<(String, String)>,
    /// The generator state (splitmix64, seeded from the OS-seeded `std` hasher): fine for
    /// `HashMap` seeds and examples, not for keys or tokens.
    rng: u64,
    /// One entry per subscription: the instant it becomes ready (`None`: already
    /// ready). A dropped pollable keeps its slot.
    pollables: Vec<Option<Instant>>,
}

#[allow(dead_code)]
impl TypedHost {
    fn new(env: Vec<(String, String)>) -> Self {
        TypedHost { env, rng: os_seed(), pollables: Vec::new() }
    }

    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// `len` random bytes. A guest-chosen length above 64 MiB traps (as in wasmtime), so a
    /// guest cannot make the host allocate without bound.
    fn random_bytes(&mut self, len: u64) -> Vec<u8> {
        assert!(len <= 64 << 20, "random bytes: length over 64 MiB");
        std::iter::repeat_with(|| self.next_u64().to_le_bytes())
            .flatten()
            .take(len as usize)
            .collect()
    }

    // optional: wasi_io_poll_resource_drop_pollable
    fn subscribe(&mut self, deadline: Option<Instant>) -> Pollable {
        self.pollables.push(deadline);
        Pollable((self.pollables.len() - 1) as u32)
    }

    fn deadline(&self, pollable: Pollable) -> Option<Instant> {
        self.pollables.get(pollable.0 as usize).copied().flatten()
    }
    // end optional
}

impl Host for TypedHost {
    // optional: wasi_cli_stdout_get_stdout
    fn wasi_cli_stdout_get_stdout(&mut self) -> OutputStream {
        OutputStream(1)
    }
    // end optional

    // optional: wasi_cli_environment_get_environment
    fn wasi_cli_environment_get_environment(&mut self) -> Vec<(String, String)> {
        self.env.clone()
    }
    // end optional

    // optional: wasi_cli_exit_exit
    /// `exit(status: result)`: `ok` is 0, `error` is 1.
    fn wasi_cli_exit_exit(&mut self, status: Result<(), ()>) {
        std::io::stdout().flush().ok();
        std::process::exit(i32::from(status.is_err()));
    }
    // end optional

    // optional: wasi_io_error_resource_drop_error
    fn wasi_io_error_resource_drop_error(&mut self, _handle: Error) {}
    // end optional

    // optional: wasi_io_poll_resource_drop_pollable
    fn wasi_io_poll_resource_drop_pollable(&mut self, _handle: Pollable) {}
    // end optional

    // optional: wasi_io_streams_resource_drop_input_stream
    fn wasi_io_streams_resource_drop_input_stream(&mut self, _handle: InputStream) {}
    // end optional

    // optional: wasi_cli_terminal_input_resource_drop_terminal_input
    fn wasi_cli_terminal_input_resource_drop_terminal_input(&mut self, _handle: TerminalInput) {}
    // end optional

    // optional: wasi_cli_terminal_output_resource_drop_terminal_output
    fn wasi_cli_terminal_output_resource_drop_terminal_output(&mut self, _handle: TerminalOutput) {}
    // end optional

    // optional: wasi_cli_stdin_get_stdin
    fn wasi_cli_stdin_get_stdin(&mut self) -> InputStream {
        InputStream(0)
    }
    // end optional

    // optional: wasi_cli_stderr_get_stderr
    fn wasi_cli_stderr_get_stderr(&mut self) -> OutputStream {
        OutputStream(2)
    }
    // end optional

    // optional: wasi_cli_terminal_stdin_get_terminal_stdin
    fn wasi_cli_terminal_stdin_get_terminal_stdin(&mut self) -> Option<TerminalInput> {
        None
    }
    // end optional

    // optional: wasi_cli_terminal_stdout_get_terminal_stdout
    fn wasi_cli_terminal_stdout_get_terminal_stdout(&mut self) -> Option<TerminalOutput> {
        None
    }
    // end optional

    // optional: wasi_cli_terminal_stderr_get_terminal_stderr
    fn wasi_cli_terminal_stderr_get_terminal_stderr(&mut self) -> Option<TerminalOutput> {
        None
    }
    // end optional

    // optional: wasi_clocks_monotonic_clock_subscribe_duration
    fn wasi_clocks_monotonic_clock_subscribe_duration(&mut self, when: u64) -> Pollable {
        self.subscribe(Some(Instant::now() + Duration::from_nanos(when)))
    }
    // end optional

    // optional: wasi_io_poll_poll
    /// The indices of the ready pollables, waiting for the earliest deadline if none
    /// is ready yet. An empty list traps, as the interface specifies.
    fn wasi_io_poll_poll(&mut self, in_: Vec<Pollable>) -> Vec<u32> {
        assert!(!in_.is_empty(), "wasi:io/poll.poll: empty list");
        // `None` as soon as one pollable is ready already: then nothing to wait for
        let pending = in_.iter().map(|p| self.deadline(*p)).collect::<Option<Vec<_>>>();
        if let Some(first) = pending.and_then(|d| d.into_iter().min()) {
            sleep_until(first);
        }
        let now = Instant::now();
        in_.iter()
            .enumerate()
            .filter(|(_, p)| self.deadline(**p).is_none_or(|d| d <= now))
            .map(|(i, _)| i as u32)
            .collect()
    }
    // end optional

    // optional: wasi_io_streams_resource_drop_output_stream
    fn wasi_io_streams_resource_drop_output_stream(&mut self, _handle: OutputStream) {}
    // end optional

    // optional: wasi_io_poll_method_pollable_block
    fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable) {
        if let Some(deadline) = self.deadline(self_) {
            sleep_until(deadline);
        }
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_check_write
    fn wasi_io_streams_method_output_stream_check_write(
        &mut self,
        _self_: OutputStream,
    ) -> Result<u64, StreamError> {
        Ok(4096)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_write
    fn wasi_io_streams_method_output_stream_write(
        &mut self,
        self_: OutputStream,
        contents: Vec<u8>,
    ) -> Result<(), StreamError> {
        let written = match self_.0 {
            2 => std::io::stderr().write_all(&contents),
            _ => std::io::stdout().write_all(&contents),
        };
        written.map_err(|_| StreamError::Closed)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_blocking_flush
    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        _self_: OutputStream,
    ) -> Result<(), StreamError> {
        // stderr is unbuffered in `std`, so only stdout has anything to flush
        std::io::stdout().flush().map_err(|_| StreamError::Closed)
    }
    // end optional

    // optional: wasi_io_streams_method_output_stream_subscribe
    fn wasi_io_streams_method_output_stream_subscribe(&mut self, _self_: OutputStream) -> Pollable {
        self.subscribe(None)
    }
    // end optional

    // optional: wasi_random_random_get_random_bytes
    fn wasi_random_random_get_random_bytes(&mut self, len: u64) -> Vec<u8> {
        self.random_bytes(len)
    }
    // end optional

    // optional: wasi_random_random_get_random_u64
    fn wasi_random_random_get_random_u64(&mut self) -> u64 {
        self.next_u64()
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_bytes
    fn wasi_random_insecure_get_insecure_random_bytes(&mut self, len: u64) -> Vec<u8> {
        self.random_bytes(len)
    }
    // end optional

    // optional: wasi_random_insecure_get_insecure_random_u64
    fn wasi_random_insecure_get_insecure_random_u64(&mut self) -> u64 {
        self.next_u64()
    }
    // end optional

    // optional: wasi_random_insecure_seed_insecure_seed
    fn wasi_random_insecure_seed_insecure_seed(&mut self) -> (u64, u64) {
        (self.next_u64(), self.next_u64())
    }
    // end optional
}
