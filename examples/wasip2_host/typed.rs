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
// A component that imports more than these interfaces gets a bigger `Host` trait, so
// this host only builds against components that import no more than it implements.

use std::io::Write;
use std::time::{Duration, Instant};

/// Blocks until `deadline`.
fn sleep_until(deadline: Instant) {
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
}

struct TypedHost {
    env: Vec<(String, String)>,
    /// One entry per subscription: the instant it becomes ready (`None`: already
    /// ready). A dropped pollable keeps its slot.
    pollables: Vec<Option<Instant>>,
}

impl TypedHost {
    fn new(env: Vec<(String, String)>) -> Self {
        TypedHost { env, pollables: Vec::new() }
    }

    fn subscribe(&mut self, deadline: Option<Instant>) -> Pollable {
        self.pollables.push(deadline);
        Pollable((self.pollables.len() - 1) as u32)
    }

    fn deadline(&self, pollable: Pollable) -> Option<Instant> {
        self.pollables.get(pollable.0 as usize).copied().flatten()
    }
}

impl Host for TypedHost {
    fn wasi_cli_stdout_get_stdout(&mut self) -> OutputStream {
        OutputStream(1)
    }

    fn wasi_cli_environment_get_environment(&mut self) -> Vec<(String, String)> {
        self.env.clone()
    }

    /// `exit(status: result)`: `ok` is 0, `error` is 1.
    fn wasi_cli_exit_exit(&mut self, status: Result<(), ()>) {
        std::io::stdout().flush().ok();
        std::process::exit(i32::from(status.is_err()));
    }

    fn wasi_io_error_resource_drop_error(&mut self, _handle: Error) {}

    fn wasi_io_poll_resource_drop_pollable(&mut self, _handle: Pollable) {}

    fn wasi_io_streams_resource_drop_input_stream(&mut self, _handle: InputStream) {}

    fn wasi_cli_terminal_input_resource_drop_terminal_input(&mut self, _handle: TerminalInput) {}

    fn wasi_cli_terminal_output_resource_drop_terminal_output(&mut self, _handle: TerminalOutput) {}

    fn wasi_cli_stdin_get_stdin(&mut self) -> InputStream {
        InputStream(0)
    }

    fn wasi_cli_stderr_get_stderr(&mut self) -> OutputStream {
        OutputStream(2)
    }

    fn wasi_cli_terminal_stdin_get_terminal_stdin(&mut self) -> Option<TerminalInput> {
        None
    }

    fn wasi_cli_terminal_stdout_get_terminal_stdout(&mut self) -> Option<TerminalOutput> {
        None
    }

    fn wasi_cli_terminal_stderr_get_terminal_stderr(&mut self) -> Option<TerminalOutput> {
        None
    }

    fn wasi_clocks_monotonic_clock_subscribe_duration(&mut self, when: u64) -> Pollable {
        self.subscribe(Some(Instant::now() + Duration::from_nanos(when)))
    }

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

    fn wasi_io_streams_resource_drop_output_stream(&mut self, _handle: OutputStream) {}

    fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable) {
        if let Some(deadline) = self.deadline(self_) {
            sleep_until(deadline);
        }
    }

    fn wasi_io_streams_method_output_stream_check_write(
        &mut self,
        _self_: OutputStream,
    ) -> Result<u64, StreamError> {
        Ok(4096)
    }

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

    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        _self_: OutputStream,
    ) -> Result<(), StreamError> {
        // stderr is unbuffered in `std`, so only stdout has anything to flush
        std::io::stdout().flush().map_err(|_| StreamError::Closed)
    }

    fn wasi_io_streams_method_output_stream_subscribe(&mut self, _self_: OutputStream) -> Pollable {
        self.subscribe(None)
    }
}
