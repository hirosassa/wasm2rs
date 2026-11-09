use super::*;
use crate::Host;
use crate::wasi_http_types::{FutureTrailers, HeaderError, Method, Scheme};
use http::{HttpFields, HttpIncomingBody, HttpIncomingResponse, HttpOptions, HttpOutgoingRequest};

impl Host for WasiHost {
    fn wasi_cli_stdout_get_stdout(&mut self) -> OutputStream {
        OutputStream(1)
    }

    fn wasi_cli_environment_get_arguments(&mut self) -> Vec<String> {
        self.args.clone()
    }

    fn wasi_cli_environment_get_environment(&mut self) -> Vec<(String, String)> {
        self.env.clone()
    }

    /// The configured initial cwd; by default "/" if a preopen is named "/", else `None`.
    fn wasi_cli_environment_initial_cwd(&mut self) -> Option<String> {
        match &self.cwd {
            Some(cwd) => cwd.clone(),
            None => self
                .preopens
                .iter()
                .any(|p| p.guest_name == "/")
                .then(|| "/".to_string()),
        }
    }

    /// `exit(status: result)`: `ok` is 0, `error` is 1. The send queues of the connections are
    /// flushed first (see `flush_all_connections`).
    fn wasi_cli_exit_exit(&mut self, status: Result<(), ()>) {
        self.flush_stdio();
        self.flush_all_connections();
        std::process::exit(i32::from(status.is_err()));
    }

    /// The message of the failure behind an `error` handle (an unknown one is empty).
    fn wasi_io_error_method_error_to_debug_string(&mut self, self_: Error) -> String {
        self.errors
            .get(self_.0 as usize)
            .map(|e| e.message.clone())
            .unwrap_or_default()
    }

    /// The filesystem `error-code` behind an `error` handle: only a failure of a file stream
    /// has one (`None` for a connection's or stdout's, and for a handle the host does not hold).
    fn wasi_filesystem_types_filesystem_error_code(&mut self, err: Error) -> Option<FsErrorCode> {
        self.errors.get(err.0 as usize)?.fs.map(FsErrorCode::from)
    }

    fn wasi_io_error_resource_drop_error(&mut self, handle: Error) {
        self.errors.remove(handle.0 as usize);
    }

    fn wasi_http_types_constructor_fields(&mut self) -> Fields {
        Fields(self.http_fields.insert(HttpFields::default()) as u32)
    }

    fn wasi_http_types_static_fields_from_list(
        &mut self,
        entries: Vec<(String, Vec<u8>)>,
    ) -> Result<Fields, HeaderError> {
        let fields = HttpFields::from_list(entries)?;
        Ok(Fields(self.http_fields.insert(fields) as u32))
    }

    fn wasi_http_types_method_fields_get(&mut self, self_: Fields, name: String) -> Vec<Vec<u8>> {
        self.http_fields(self_).get(&name)
    }

    fn wasi_http_types_method_fields_has(&mut self, self_: Fields, name: String) -> bool {
        self.http_fields(self_).has(&name)
    }

    fn wasi_http_types_method_fields_set(
        &mut self,
        self_: Fields,
        name: String,
        value: Vec<Vec<u8>>,
    ) -> Result<(), HeaderError> {
        self.http_fields_mut(self_).set(name, value)
    }

    fn wasi_http_types_method_fields_delete(
        &mut self,
        self_: Fields,
        name: String,
    ) -> Result<(), HeaderError> {
        self.http_fields_mut(self_).delete(&name)
    }

    fn wasi_http_types_method_fields_append(
        &mut self,
        self_: Fields,
        name: String,
        value: Vec<u8>,
    ) -> Result<(), HeaderError> {
        self.http_fields_mut(self_).append(name, value)
    }

    fn wasi_http_types_method_fields_entries(&mut self, self_: Fields) -> Vec<(String, Vec<u8>)> {
        self.http_fields(self_).entries()
    }

    /// A clone is always mutable, even of the immutable fields of a request.
    fn wasi_http_types_method_fields_clone(&mut self, self_: Fields) -> Fields {
        let copy = self.http_fields(self_).copy(false);
        Fields(self.http_fields.insert(copy) as u32)
    }

    /// Takes over the `headers` handle's contents: the guest gave the resource away.
    fn wasi_http_types_constructor_outgoing_request(&mut self, headers: Fields) -> OutgoingRequest {
        let fields = self.http_fields(headers).clone();
        self.http_fields.remove(headers.0 as usize);
        OutgoingRequest(self.http_requests.insert(HttpOutgoingRequest::new(fields)) as u32)
    }

    fn wasi_http_types_method_outgoing_request_method(&mut self, self_: OutgoingRequest) -> Method {
        self.http_request(self_).method.clone()
    }

    fn wasi_http_types_method_outgoing_request_set_method(
        &mut self,
        self_: OutgoingRequest,
        method: Method,
    ) -> Result<(), ()> {
        if !http::valid_method(&method) {
            return Err(());
        }
        self.http_request_mut(self_).method = method;
        Ok(())
    }

    fn wasi_http_types_method_outgoing_request_path_with_query(
        &mut self,
        self_: OutgoingRequest,
    ) -> Option<String> {
        self.http_request(self_).path_with_query.clone()
    }

    fn wasi_http_types_method_outgoing_request_set_path_with_query(
        &mut self,
        self_: OutgoingRequest,
        path_with_query: Option<String>,
    ) -> Result<(), ()> {
        if path_with_query
            .as_deref()
            .is_some_and(|p| !http::valid_path_with_query(p))
        {
            return Err(());
        }
        self.http_request_mut(self_).path_with_query = path_with_query;
        Ok(())
    }

    fn wasi_http_types_method_outgoing_request_scheme(
        &mut self,
        self_: OutgoingRequest,
    ) -> Option<Scheme> {
        self.http_request(self_).scheme.clone()
    }

    fn wasi_http_types_method_outgoing_request_set_scheme(
        &mut self,
        self_: OutgoingRequest,
        scheme: Option<Scheme>,
    ) -> Result<(), ()> {
        if scheme.as_ref().is_some_and(|s| !http::valid_scheme(s)) {
            return Err(());
        }
        self.http_request_mut(self_).scheme = scheme;
        Ok(())
    }

    fn wasi_http_types_method_outgoing_request_authority(
        &mut self,
        self_: OutgoingRequest,
    ) -> Option<String> {
        self.http_request(self_).authority.clone()
    }

    fn wasi_http_types_method_outgoing_request_set_authority(
        &mut self,
        self_: OutgoingRequest,
        authority: Option<String>,
    ) -> Result<(), ()> {
        if authority
            .as_deref()
            .is_some_and(|a| !http::valid_authority(a))
        {
            return Err(());
        }
        self.http_request_mut(self_).authority = authority;
        Ok(())
    }

    /// A snapshot: the request's headers never change, so it equals a live view. Immutable.
    fn wasi_http_types_method_outgoing_request_headers(
        &mut self,
        self_: OutgoingRequest,
    ) -> Fields {
        let view = self.http_request(self_).headers.copy(true);
        Fields(self.http_fields.insert(view) as u32)
    }

    fn wasi_http_types_constructor_request_options(&mut self) -> RequestOptions {
        RequestOptions(self.http_options.insert(HttpOptions::default()) as u32)
    }

    fn wasi_http_types_method_request_options_connect_timeout(
        &mut self,
        self_: RequestOptions,
    ) -> Option<u64> {
        self.http_option(self_).connect
    }

    fn wasi_http_types_method_request_options_set_connect_timeout(
        &mut self,
        self_: RequestOptions,
        duration: Option<u64>,
    ) -> Result<(), ()> {
        self.http_option_mut(self_).connect = duration;
        Ok(())
    }

    fn wasi_http_types_method_request_options_first_byte_timeout(
        &mut self,
        self_: RequestOptions,
    ) -> Option<u64> {
        self.http_option(self_).first_byte
    }

    fn wasi_http_types_method_request_options_set_first_byte_timeout(
        &mut self,
        self_: RequestOptions,
        duration: Option<u64>,
    ) -> Result<(), ()> {
        self.http_option_mut(self_).first_byte = duration;
        Ok(())
    }

    fn wasi_http_types_method_request_options_between_bytes_timeout(
        &mut self,
        self_: RequestOptions,
    ) -> Option<u64> {
        self.http_option(self_).between_bytes
    }

    fn wasi_http_types_method_request_options_set_between_bytes_timeout(
        &mut self,
        self_: RequestOptions,
        duration: Option<u64>,
    ) -> Result<(), ()> {
        self.http_option_mut(self_).between_bytes = duration;
        Ok(())
    }

    fn wasi_http_types_resource_drop_fields(&mut self, handle: Fields) {
        self.http_fields.remove(handle.0 as usize);
    }

    fn wasi_http_types_resource_drop_request_options(&mut self, handle: RequestOptions) {
        self.http_options.remove(handle.0 as usize);
    }

    fn wasi_http_types_resource_drop_outgoing_request(&mut self, handle: OutgoingRequest) {
        self.http_requests.remove(handle.0 as usize);
    }

    fn wasi_http_outgoing_handler_handle(
        &mut self,
        request: OutgoingRequest,
        options: Option<RequestOptions>,
    ) -> Result<FutureIncomingResponse, HttpErrorCode> {
        self.http_handle(request, options)
    }

    fn wasi_http_types_method_future_incoming_response_subscribe(
        &mut self,
        self_: FutureIncomingResponse,
    ) -> Pollable {
        self.subscribe_wait(Wait::HttpResponse(self_.0 as usize))
    }

    /// `None` while the request runs; then the result once (`Some(Err(()))` afterwards). A
    /// failure of the client is `internal-error` with its message.
    fn wasi_http_types_method_future_incoming_response_get(
        &mut self,
        self_: FutureIncomingResponse,
    ) -> Option<Result<Result<IncomingResponse, HttpErrorCode>, ()>> {
        let result = self.http_future_mut(self_).take()?;
        Some(result.map(|result| match result {
            Ok(response) => {
                let slot = self
                    .http_responses
                    .insert(HttpIncomingResponse::new(response));
                Ok(IncomingResponse(slot as u32))
            }
            Err(e) => Err(HttpErrorCode::InternalError(Some(e.to_string()))),
        }))
    }

    fn wasi_http_types_method_incoming_response_status(&mut self, self_: IncomingResponse) -> u16 {
        self.http_response(self_).status
    }

    fn wasi_http_types_method_incoming_response_headers(
        &mut self,
        self_: IncomingResponse,
    ) -> Fields {
        let copy = self.http_response(self_).headers.copy(true);
        Fields(self.http_fields.insert(copy) as u32)
    }

    /// Starts reading the body on a thread; it can be consumed once.
    fn wasi_http_types_method_incoming_response_consume(
        &mut self,
        self_: IncomingResponse,
    ) -> Result<IncomingBody, ()> {
        let reader = self.http_response_mut(self_).body.take().ok_or(())?;
        Ok(IncomingBody(
            self.http_bodies.insert(HttpIncomingBody::spawn(reader)) as u32,
        ))
    }

    /// The body's input stream (`HTTP_STREAM + slot`); it can be taken once.
    fn wasi_http_types_method_incoming_body_stream(
        &mut self,
        self_: IncomingBody,
    ) -> Result<InputStream, ()> {
        if !self.http_body_mut(self_).take_stream() {
            return Err(());
        }
        let handle = u32::try_from(HTTP_STREAM + self_.0 as usize).map_err(|_| ())?;
        Ok(InputStream(handle))
    }

    /// Ends the body (its thread stops). There are no trailers yet (H3): the future only
    /// exists so that the guest can drop it.
    fn wasi_http_types_static_incoming_body_finish(
        &mut self,
        this: IncomingBody,
    ) -> FutureTrailers {
        self.http_bodies
            .take(this.0 as usize)
            .unwrap_or_else(|| http_missing("incoming-body"));
        FutureTrailers(self.http_trailers.insert(()) as u32)
    }

    /// Only failures of a response body have an `error-code` (`None` for any other `error`).
    fn wasi_http_types_http_error_code(&mut self, err: Error) -> Option<HttpErrorCode> {
        self.errors.get(err.0 as usize)?.http.clone()
    }

    fn wasi_http_types_resource_drop_future_incoming_response(
        &mut self,
        handle: FutureIncomingResponse,
    ) {
        if let Some(future) = self.http_futures.take(handle.0 as usize) {
            future.cancel();
        }
    }

    fn wasi_http_types_resource_drop_incoming_response(&mut self, handle: IncomingResponse) {
        self.http_responses.remove(handle.0 as usize);
    }

    fn wasi_http_types_resource_drop_incoming_body(&mut self, handle: IncomingBody) {
        self.http_bodies.take(handle.0 as usize);
    }

    fn wasi_http_types_resource_drop_future_trailers(&mut self, handle: FutureTrailers) {
        self.http_trailers.remove(handle.0 as usize);
    }

    fn wasi_io_poll_resource_drop_pollable(&mut self, _handle: Pollable) {}

    fn wasi_io_streams_resource_drop_input_stream(&mut self, handle: InputStream) {
        self.failed_in.remove(&handle.0);
        if let Some(slot) = tcp_slot(handle.0) {
            self.tcp_in.remove(slot);
        } else if http_slot(handle.0).is_some() {
            // the body (and its thread) lives until the guest drops or finishes it
        } else if let Some(index) = file_index(handle.0) {
            self.files.remove(index);
        }
    }

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
        self.subscribe(Some(deadline_after(Instant::now(), when)))
    }

    /// An instant is on the host's clock (see `monotonic-clock.now`); one that has passed is
    /// ready at once. The wait itself is in real time.
    fn wasi_clocks_monotonic_clock_subscribe_instant(&mut self, when: u64) -> Pollable {
        let remaining = when.saturating_sub(self.clock.monotonic_now());
        self.subscribe(Some(deadline_after(Instant::now(), remaining)))
    }

    /// The host clock's monotonic time (by default, nanoseconds since the host was created).
    fn wasi_clocks_monotonic_clock_now(&mut self) -> u64 {
        self.clock.monotonic_now()
    }

    fn wasi_clocks_monotonic_clock_resolution(&mut self) -> u64 {
        self.clock.monotonic_resolution()
    }

    fn wasi_clocks_wall_clock_resolution(&mut self) -> Datetime {
        self.clock.wall_resolution()
    }

    fn wasi_clocks_wall_clock_now(&mut self) -> Datetime {
        self.clock.wall_now()
    }

    /// The indices of the ready pollables, waiting for the earliest deadline, for data
    /// on stdin or for a socket if none is ready yet. An empty list traps, as the interface specifies.
    fn wasi_io_poll_poll(&mut self, in_: Vec<Pollable>) -> Vec<u32> {
        assert!(!in_.is_empty(), "wasi:io/poll.poll: empty list");
        self.wait_ready(&in_)
    }

    fn wasi_io_streams_resource_drop_output_stream(&mut self, handle: OutputStream) {
        self.failed_out.remove(&handle.0);
        if let Some(slot) = tcp_slot(handle.0) {
            // what the guest wrote still reaches the peer (see `TcpOut`)
            if let Some(conn) = self.tcp_out.get(slot) {
                conn.flush_waiting();
            }
            self.tcp_out.remove(slot);
        } else if let Some(index) = file_index(handle.0) {
            self.outputs.remove(index);
        }
    }

    /// Whether the pollable is ready now, without waiting.
    fn wasi_io_poll_method_pollable_ready(&mut self, self_: Pollable) -> bool {
        let (buf, _) = &*self.stdin;
        let buf = buf.lock().unwrap();
        self.wait_of(self_).is_ready(Instant::now(), &buf, self)
    }

    fn wasi_io_poll_method_pollable_block(&mut self, self_: Pollable) {
        self.wait_ready(&[self_]);
    }

    /// 4096, except for a connection: its send queue is drained first and the permit is what
    /// still fits it (up to 4096), so 0 while the peer does not read.
    fn wasi_io_streams_method_output_stream_check_write(
        &mut self,
        self_: OutputStream,
    ) -> Result<u64, StreamError> {
        match tcp_slot(self_.0) {
            Some(slot) => self
                .tcp_permit(slot)
                .ok_or_else(|| self.tcp_out_error(slot)),
            None => Ok(4096),
        }
    }

    /// Writes without flushing. `contents` over 4096 bytes (what `check-write` returns) traps.
    fn wasi_io_streams_method_output_stream_write(
        &mut self,
        self_: OutputStream,
        contents: Vec<u8>,
    ) -> Result<(), StreamError> {
        assert!(
            contents.len() <= 4096,
            "write: more than check-write allows"
        );
        self.assert_within_permit(self_.0, contents.len() as u64, "write")?;
        self.write_stream(self_.0, &contents, false)
    }

    fn wasi_io_streams_method_output_stream_blocking_flush(
        &mut self,
        self_: OutputStream,
    ) -> Result<(), StreamError> {
        self.flush_stream(self_.0, true)
    }

    /// Drains a connection's send queue as far as the socket takes now, without waiting
    /// (`blocking-flush` waits until the queue is empty); the other streams hold nothing.
    fn wasi_io_streams_method_output_stream_flush(
        &mut self,
        self_: OutputStream,
    ) -> Result<(), StreamError> {
        self.flush_stream(self_.0, false)
    }

    /// Writes then flushes. `contents` over 4096 bytes (what `check-write` returns) traps.
    /// A connection's send queue is emptied first and then the bytes are sent, waiting for the
    /// peer to read, so this does not return while it never does.
    fn wasi_io_streams_method_output_stream_blocking_write_and_flush(
        &mut self,
        self_: OutputStream,
        contents: Vec<u8>,
    ) -> Result<(), StreamError> {
        assert!(
            contents.len() <= 4096,
            "blocking-write-and-flush: more than check-write allows"
        );
        self.write_stream(self_.0, &contents, true)?;
        self.flush_stream(self_.0, true)
    }

    /// Writes `len` zero bytes; a `len` over 4096 or over what `check-write` permits a
    /// connection (see `write`) traps.
    fn wasi_io_streams_method_output_stream_write_zeroes(
        &mut self,
        self_: OutputStream,
        len: u64,
    ) -> Result<(), StreamError> {
        assert!(len <= 4096, "write-zeroes: more than check-write allows");
        self.assert_within_permit(self_.0, len, "write-zeroes")?;
        self.write_stream(self_.0, &vec![0u8; len as usize], false)
    }

    /// `write-zeroes` then flush; a `len` over 4096 traps. Waits on a connection like
    /// `blocking-write-and-flush`.
    fn wasi_io_streams_method_output_stream_blocking_write_zeroes_and_flush(
        &mut self,
        self_: OutputStream,
        len: u64,
    ) -> Result<(), StreamError> {
        assert!(
            len <= 4096,
            "blocking-write-zeroes-and-flush: more than check-write allows"
        );
        self.write_stream(self_.0, &vec![0u8; len as usize], true)?;
        self.flush_stream(self_.0, true)
    }

    /// Moves at most `min(len, 4096)` bytes from `src` to this stream and returns how many:
    /// a dropped output is `closed` first (before anything is read), then `read`, then
    /// `write` (a zero `len` is ok while both are open). It does not wait for `src`: `src` at
    /// its end is `closed`, nothing available yet is `ok(0)`. For a connection it reads at
    /// most what `check-write` permits, so with the send queue full (permit 0) it returns
    /// `ok(0)` without touching `src`. A failed write is `last-operation-failed` once, then the
    /// output stream is `closed`. Known
    /// limitation: the bytes already read from `src` are lost then.
    fn wasi_io_streams_method_output_stream_splice(
        &mut self,
        self_: OutputStream,
        src: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        if !self.output_is_open(self_.0) {
            return Err(StreamError::Closed);
        }
        let budget = self.splice_budget(self_.0, len, false)?;
        if budget == 0 && len > 0 {
            return Ok(0);
        }
        let data = self.read_stream(src.0, budget)?;
        if !data.is_empty() {
            self.write_stream(self_.0, &data, false)?;
        }
        Ok(data.len() as u64)
    }

    /// Like `splice`, but waits while an open `src` (stdin or a connection) has nothing yet,
    /// so at least one byte is moved for a `len` over 0. For a connection it first waits for its
    /// send queue to empty (the peer to read), then reads at most what `check-write` permits.
    fn wasi_io_streams_method_output_stream_blocking_splice(
        &mut self,
        self_: OutputStream,
        src: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        if !self.output_is_open(self_.0) {
            return Err(StreamError::Closed);
        }
        let budget = self.splice_budget(self_.0, len, true)?;
        let data = self.read_stream_waiting(src.0, budget)?;
        if !data.is_empty() {
            self.write_stream(self_.0, &data, false)?;
        }
        Ok(data.len() as u64)
    }

    /// Ready at once, except for a connection: it is ready while its send queue has room.
    fn wasi_io_streams_method_output_stream_subscribe(&mut self, self_: OutputStream) -> Pollable {
        match tcp_slot(self_.0) {
            Some(slot) => self.subscribe_wait(Wait::TcpWritable(slot)),
            None => self.subscribe(None),
        }
    }

    fn wasi_random_random_get_random_bytes(&mut self, len: u64) -> Vec<u8> {
        self.secure_bytes(len)
    }

    fn wasi_random_random_get_random_u64(&mut self) -> u64 {
        let mut bytes = [0; 8];
        self.fill_secure(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn wasi_random_insecure_get_insecure_random_bytes(&mut self, len: u64) -> Vec<u8> {
        self.insecure_bytes(len)
    }

    fn wasi_random_insecure_get_insecure_random_u64(&mut self) -> u64 {
        self.random_source().insecure_u64()
    }

    fn wasi_random_insecure_seed_insecure_seed(&mut self) -> (u64, u64) {
        self.random_source().insecure_seed()
    }

    fn wasi_filesystem_types_resource_drop_descriptor(&mut self, handle: Descriptor) {
        if handle.0 as usize >= self.preopens.len() {
            // the preopens (the first slots) are never freed
            self.descriptors.remove(handle.0 as usize);
        }
    }

    fn wasi_filesystem_types_resource_drop_directory_entry_stream(
        &mut self,
        handle: DirectoryEntryStream,
    ) {
        self.listings.remove(handle.0 as usize);
    }

    /// The preopens with their guest names, in descriptor order.
    fn wasi_filesystem_preopens_get_directories(&mut self) -> Vec<(Descriptor, String)> {
        (0u32..)
            .zip(&self.preopens)
            .map(|(slot, p)| (Descriptor(slot), p.guest_name.clone()))
            .collect()
    }

    /// Without `symlink-follow` a symlink as the last component is `loop` (like `O_NOFOLLOW`),
    /// also when creating; `create` + `exclusive` on a symlink name is `exist` whatever the
    /// flags (it never follows, dangling or not). Links in the rest of the path, and a path
    /// naming a directory itself (`names_an_entry`), are resolved as ever, always within the preopen. Creating,
    /// truncating and writing are `read-only` unless the preopen is writable; then `create`,
    /// `exclusive` and `truncate` act at once, and the descriptor remembers whether it was
    /// opened for `write`.
    fn wasi_filesystem_types_method_descriptor_open_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
        open_flags: OpenFlags,
        flags: DescriptorFlags,
    ) -> Result<Descriptor, FsErrorCode> {
        let create = open_flags.contains(OpenFlags::CREATE);
        let exclusive = open_flags.contains(OpenFlags::EXCLUSIVE);
        let exclusive_create = create && exclusive;
        let truncate = open_flags.contains(OpenFlags::TRUNCATE);
        let writes = flags.contains(DescriptorFlags::WRITE);
        let changes = create || exclusive || truncate;
        if changes || writes || flags.contains(DescriptorFlags::MUTATE_DIRECTORY) {
            self.require_writable(self_.0)?;
        }
        let wants_directory = open_flags.contains(OpenFlags::DIRECTORY);
        if wants_directory && (changes || writes) {
            return Err(Fs::Invalid.into());
        }
        let follows = path_flags.contains(PathFlags::SYMLINK_FOLLOW);
        if (exclusive_create || !follows) && names_an_entry(&path) {
            let entry = self.resolve_entry(self_.0, &path)?;
            if entry.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) {
                // `create` + `exclusive` never follows: the name exists (before `loop`),
                // dangling or not
                return Err(if exclusive_create {
                    Fs::Exist
                } else {
                    Fs::Loop
                }
                .into());
            }
        }
        let full = if create {
            self.resolve_to_create(self_.0, &path)?
        } else {
            self.resolve(self_.0, &path)?
        };
        let is_dir = full.is_dir();
        if wants_directory && !is_dir {
            return Err(Fs::NotDirectory.into());
        }
        if is_dir && writes {
            return Err(Fs::IsDirectory.into());
        }
        if exclusive_create && full.exists() {
            return Err(Fs::Exist.into());
        }
        if create || truncate {
            std::fs::OpenOptions::new()
                .write(true)
                .create(create)
                .truncate(truncate)
                .open(&full)
                .map_err(io_error_code)?;
        }
        // a file that only writes need not be readable (and one with no flags reads)
        let reads = flags.contains(DescriptorFlags::READ) || !writes;
        let file = if is_dir {
            None
        } else {
            let opened = std::fs::OpenOptions::new()
                .read(reads)
                .write(writes)
                .open(&full);
            Some(opened.map_err(io_error_code)?)
        };
        let preopen = self.open_descriptor(self_.0)?.preopen;
        let slot = self.descriptors.insert(OpenDescriptor {
            path: full,
            preopen,
            write: writes,
            read: reads,
            file,
        });
        Ok(Descriptor(slot as u32))
    }

    /// A new stream over the file, starting at `offset`: `is-directory` for a directory,
    /// `bad-descriptor` if the descriptor was not opened for reading.
    fn wasi_filesystem_types_method_descriptor_read_via_stream(
        &mut self,
        self_: Descriptor,
        offset: u64,
    ) -> Result<InputStream, FsErrorCode> {
        let stream = FileStream::over(self.readable_descriptor_file(self_.0)?, offset, false)?;
        let slot = self.files.insert(stream);
        Ok(InputStream(FIRST_FILE_STREAM + slot as u32))
    }

    /// A new stream writing into the file from `offset`: `read-only` unless the preopen is
    /// writable; the descriptor must have been opened for `write`.
    fn wasi_filesystem_types_method_descriptor_write_via_stream(
        &mut self,
        self_: Descriptor,
        offset: u64,
    ) -> Result<OutputStream, FsErrorCode> {
        Ok(OutputStream(
            self.open_output_stream(self_.0, offset, false)?,
        ))
    }

    /// Like `write-via-stream`, but every write goes to the end of the file.
    fn wasi_filesystem_types_method_descriptor_append_via_stream(
        &mut self,
        self_: Descriptor,
    ) -> Result<OutputStream, FsErrorCode> {
        Ok(OutputStream(self.open_output_stream(self_.0, 0, true)?))
    }

    /// Both names are checked like any other entry; an existing target file is replaced. A
    /// symlink cannot be renamed (`not-permitted`).
    fn wasi_filesystem_types_method_descriptor_rename_at(
        &mut self,
        self_: Descriptor,
        old_path: String,
        new_descriptor: Descriptor,
        new_path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        self.require_writable(new_descriptor.0)?;
        let old = self.resolve_entry(self_.0, &old_path)?;
        let new = self.resolve_entry(new_descriptor.0, &new_path)?;
        // Not a symlink: moving one over the path of an open descriptor would lead it out of
        // the preopen. (A directory that holds a symlink can still be moved there;
        // `directory` resolves the path again on every use.)
        if old
            .symlink_metadata()
            .map_err(io_error_code)?
            .file_type()
            .is_symlink()
        {
            return Err(Fs::NotPermitted.into());
        }
        Ok(std::fs::rename(old, new).map_err(io_error_code)?)
    }

    /// A symlink named `new-path` (under this directory) holding `old-path`. The target must
    /// be relative and have no `..` (`not-permitted`; empty is `no-entry`), so every link
    /// made can be read back with `readlink-at`, and a taken name is `exist`. Needs a
    /// writable preopen.
    fn wasi_filesystem_types_method_descriptor_symlink_at(
        &mut self,
        self_: Descriptor,
        old_path: String,
        new_path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        if old_path.is_empty() {
            return Err(Fs::NoEntry.into());
        }
        if !stays_below(Path::new(&old_path)) {
            return Err(Fs::NotPermitted.into());
        }
        let link = self.resolve_entry(self_.0, &new_path)?;
        Ok(make_symlink(&old_path, &link).map_err(io_error_code)?)
    }

    /// A hard link. With `symlink-follow` a symlink `old-path` is followed; without it a
    /// symlink is `unsupported` (hosts differ on what to link) and a directory is
    /// `not-permitted`. Both names are checked like any other entry; a taken new name is
    /// `exist`. Needs a writable preopen.
    fn wasi_filesystem_types_method_descriptor_link_at(
        &mut self,
        self_: Descriptor,
        old_path_flags: PathFlags,
        old_path: String,
        new_descriptor: Descriptor,
        new_path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        self.require_writable(new_descriptor.0)?;
        let old = if old_path_flags.contains(PathFlags::SYMLINK_FOLLOW) {
            self.resolve(self_.0, &old_path)?
        } else {
            let entry = self.resolve_entry(self_.0, &old_path)?;
            if entry
                .symlink_metadata()
                .map_err(io_error_code)?
                .is_symlink()
            {
                return Err(Fs::Unsupported.into());
            }
            entry
        };
        if old.metadata().map_err(io_error_code)?.is_dir() {
            return Err(Fs::NotPermitted.into());
        }
        let new = self.resolve_entry(new_descriptor.0, &new_path)?;
        Ok(std::fs::hard_link(old, new).map_err(io_error_code)?)
    }

    /// Needs a writable preopen; a file must have been opened for `write` (a directory needs
    /// nothing more).
    fn wasi_filesystem_types_method_descriptor_set_times(
        &mut self,
        self_: Descriptor,
        data_access_timestamp: NewTimestamp,
        data_modification_timestamp: NewTimestamp,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        let d = self.open_descriptor(self_.0)?;
        if d.file.is_some() && !d.write {
            return Err(Fs::BadDescriptor.into());
        }
        let times = file_times(data_access_timestamp, data_modification_timestamp)?;
        Ok(self.with_descriptor_file(self_.0, |file| file.set_times(times))?)
    }

    /// `set-times` for the entry a path names. With `symlink-follow` a symlink is followed
    /// to its target; without it the entry itself is set, but `std` cannot set the times of
    /// a symlink, so that is `unsupported`. Needs a writable preopen.
    fn wasi_filesystem_types_method_descriptor_set_times_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
        data_access_timestamp: NewTimestamp,
        data_modification_timestamp: NewTimestamp,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        let times = file_times(data_access_timestamp, data_modification_timestamp)?;
        let full = if path_flags.contains(PathFlags::SYMLINK_FOLLOW) {
            self.resolve(self_.0, &path)?
        } else {
            let entry = self.resolve_entry(self_.0, &path)?;
            if entry
                .symlink_metadata()
                .map_err(io_error_code)?
                .is_symlink()
            {
                return Err(Fs::Unsupported.into());
            }
            entry
        };
        let file = std::fs::File::open(full).map_err(io_error_code)?;
        Ok(file.set_times(times).map_err(io_error_code)?)
    }

    /// Truncates or zero-extends the file. Like writing, it needs a writable preopen and a
    /// descriptor opened for `write`.
    fn wasi_filesystem_types_method_descriptor_set_size(
        &mut self,
        self_: Descriptor,
        size: u64,
    ) -> Result<(), FsErrorCode> {
        let file = self.writable_descriptor_file(self_.0)?;
        Ok(file.set_len(size).map_err(io_error_code)?)
    }

    /// Flushes the file or directory to disk. It changes nothing, so a read-only preopen
    /// allows it.
    fn wasi_filesystem_types_method_descriptor_sync(
        &mut self,
        self_: Descriptor,
    ) -> Result<(), FsErrorCode> {
        Ok(self.sync_descriptor(self_.0, false)?)
    }

    /// Like `sync`, without the metadata.
    fn wasi_filesystem_types_method_descriptor_sync_data(
        &mut self,
        self_: Descriptor,
    ) -> Result<(), FsErrorCode> {
        Ok(self.sync_descriptor(self_.0, true)?)
    }

    /// `exist` if it is there. This and the three below (and the other methods that change
    /// the preopen) are `read-only` unless it is writable, and never follow a symlink to
    /// its target.
    fn wasi_filesystem_types_method_descriptor_create_directory_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        let entry = self.resolve_entry(self_.0, &path)?;
        Ok(std::fs::create_dir(entry).map_err(io_error_code)?)
    }

    /// Removes a file or a symlink (the link, not its target); a directory is
    /// `is-directory`.
    fn wasi_filesystem_types_method_descriptor_unlink_file_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        let entry = self.resolve_entry(self_.0, &path)?;
        if entry.symlink_metadata().map_err(io_error_code)?.is_dir() {
            return Err(Fs::IsDirectory.into());
        }
        Ok(std::fs::remove_file(entry).map_err(io_error_code)?)
    }

    /// An empty directory only (`not-empty`); anything else, a symlink to a directory
    /// included, is `not-directory`.
    fn wasi_filesystem_types_method_descriptor_remove_directory_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<(), FsErrorCode> {
        self.require_writable(self_.0)?;
        let entry = self.resolve_entry(self_.0, &path)?;
        if !entry.symlink_metadata().map_err(io_error_code)?.is_dir() {
            return Err(Fs::NotDirectory.into());
        }
        Ok(std::fs::remove_dir(entry).map_err(io_error_code)?)
    }

    /// The target of a symlink, if it is relative and stays under the preopen.
    fn wasi_filesystem_types_method_descriptor_readlink_at(
        &mut self,
        self_: Descriptor,
        path: String,
    ) -> Result<String, FsErrorCode> {
        Ok(self.read_link_target(self_.0, &path)?)
    }

    /// `directory` for a directory and `regular-file` for a file; a descriptor stays on the
    /// file it opened, so never `symbolic-link`.
    fn wasi_filesystem_types_method_descriptor_get_type(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorType, FsErrorCode> {
        Ok(descriptor_type(
            self.descriptor_metadata(self_.0)?.file_type(),
        ))
    }

    /// Up to `length` bytes of the file from `offset`, without a stream or a position, and
    /// whether the end of the file was reached. Like `read-via-stream`: `is-directory` for a
    /// directory, `bad-descriptor` if it was not opened for reading. At most 1 MiB is
    /// returned per call (the guest asks again), so a huge `length` allocates nothing extra.
    fn wasi_filesystem_types_method_descriptor_read(
        &mut self,
        self_: Descriptor,
        length: u64,
        offset: u64,
    ) -> Result<(Vec<u8>, bool), FsErrorCode> {
        const LIMIT: u64 = 1 << 20;
        let mut stream = FileStream::over(self.readable_descriptor_file(self_.0)?, offset, false)?;
        let wanted = length.min(LIMIT) as usize;
        let mut data = Vec::new();
        let mut chunk = [0u8; 8192];
        while data.len() < wanted {
            let room = (wanted - data.len()).min(chunk.len());
            match stream.read(&mut chunk[..room]).map_err(io_error_code)? {
                0 => return Ok((data, true)),
                n => data.extend_from_slice(&chunk[..n]),
            }
        }
        Ok((data, false))
    }

    /// Writes `buffer` at `offset` without a stream or a position; past the end of the file
    /// the gap is zeros. Like `write-via-stream`: `read-only` unless the preopen is writable,
    /// and the descriptor must have been opened for `write`.
    fn wasi_filesystem_types_method_descriptor_write(
        &mut self,
        self_: Descriptor,
        buffer: Vec<u8>,
        offset: u64,
    ) -> Result<u64, FsErrorCode> {
        let file = self.writable_descriptor_file(self_.0)?;
        FileStream::over(file, offset, false)?
            .write_all(&buffer)
            .map_err(io_error_code)?;
        Ok(buffer.len() as u64)
    }

    /// Only a hint, so nothing is done with it (the range and the advice are not checked);
    /// like `read`, it needs a file: `is-directory` for a directory.
    fn wasi_filesystem_types_method_descriptor_advise(
        &mut self,
        self_: Descriptor,
        _offset: u64,
        _length: u64,
        _advice: Advice,
    ) -> Result<(), FsErrorCode> {
        self.open_descriptor(self_.0)?
            .file
            .as_ref()
            .ok_or(Fs::IsDirectory)?;
        Ok(())
    }

    /// Whether both descriptors name the same file (or directory).
    fn wasi_filesystem_types_method_descriptor_is_same_object(
        &mut self,
        self_: Descriptor,
        other: Descriptor,
    ) -> bool {
        self.same_object(self_.0, other.0)
    }

    /// `read` and `write` as the descriptor was opened (the preopen and directories read).
    fn wasi_filesystem_types_method_descriptor_get_flags(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorFlags, FsErrorCode> {
        let d = self.open_descriptor(self_.0)?;
        let read = if d.read { DescriptorFlags::READ.0 } else { 0 };
        let write = if d.write { DescriptorFlags::WRITE.0 } else { 0 };
        Ok(DescriptorFlags(read | write))
    }

    fn wasi_filesystem_types_method_descriptor_read_directory(
        &mut self,
        self_: Descriptor,
    ) -> Result<DirectoryEntryStream, FsErrorCode> {
        let entries = self.list_directory(self_.0)?;
        Ok(DirectoryEntryStream(
            self.listings.insert(entries.into_iter()) as u32,
        ))
    }

    fn wasi_filesystem_types_method_descriptor_stat(
        &mut self,
        self_: Descriptor,
    ) -> Result<DescriptorStat, FsErrorCode> {
        Ok(descriptor_stat(self.descriptor_metadata(self_.0)?))
    }

    /// Like `stat`, for a path under the descriptor. With `symlink-follow` a symlink is
    /// followed to its target; without it the link itself is described (type
    /// `symbolic-link`), and a link that leads out of the preopen is not an error.
    fn wasi_filesystem_types_method_descriptor_stat_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
    ) -> Result<DescriptorStat, FsErrorCode> {
        let meta = if path_flags.contains(PathFlags::SYMLINK_FOLLOW) || !names_an_entry(&path) {
            std::fs::metadata(self.resolve(self_.0, &path)?)
        } else {
            std::fs::symlink_metadata(self.resolve_entry(self_.0, &path)?)
        };
        Ok(descriptor_stat(meta.map_err(io_error_code)?))
    }

    fn wasi_filesystem_types_method_descriptor_metadata_hash(
        &mut self,
        self_: Descriptor,
    ) -> Result<MetadataHashValue, FsErrorCode> {
        let meta = self.descriptor_metadata(self_.0)?;
        Ok(metadata_hash(&meta, &self.open_descriptor(self_.0)?.path))
    }

    /// `metadata-hash` for a path under the descriptor. With `symlink-follow` a symlink is
    /// followed to its target (and hashes like it); without it the link's own identity is
    /// hashed. `.`, the empty path and a trailing `/` or `/.` name the directory itself,
    /// which is always resolved.
    fn wasi_filesystem_types_method_descriptor_metadata_hash_at(
        &mut self,
        self_: Descriptor,
        path_flags: PathFlags,
        path: String,
    ) -> Result<MetadataHashValue, FsErrorCode> {
        let (full, meta) =
            if path_flags.contains(PathFlags::SYMLINK_FOLLOW) || !names_an_entry(&path) {
                let full = self.resolve(self_.0, &path)?;
                let meta = std::fs::metadata(&full);
                (full, meta)
            } else {
                let full = self.resolve_entry(self_.0, &path)?;
                let meta = std::fs::symlink_metadata(&full);
                (full, meta)
            };
        Ok(metadata_hash(&meta.map_err(io_error_code)?, &full))
    }

    /// The next entry, `None` once the listing is exhausted.
    fn wasi_filesystem_types_method_directory_entry_stream_read_directory_entry(
        &mut self,
        self_: DirectoryEntryStream,
    ) -> Result<Option<DirectoryEntry>, FsErrorCode> {
        let listing = self
            .listings
            .get_mut(self_.0 as usize)
            .ok_or(Fs::BadDescriptor)?;
        Ok(listing.next().map(|(kind, name)| DirectoryEntry {
            type_: descriptor_type(kind),
            name,
        }))
    }

    /// Reads at most 4096 bytes of a file, connection or stdin stream at once (a
    /// connection's stream or stdin gives an empty list while nothing has arrived);
    /// `closed` at its end. An unknown or dropped stream traps.
    fn wasi_io_streams_method_input_stream_read(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<Vec<u8>, StreamError> {
        self.read_stream(self_.0, len)
    }

    /// Like `read`, but waits while stdin or a connection has nothing yet, so the list is
    /// never empty for a `len` over 0; `closed` at the end (for a `len` of 0 too).
    fn wasi_io_streams_method_input_stream_blocking_read(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<Vec<u8>, StreamError> {
        self.read_stream_waiting(self_.0, len)
    }

    /// Reads and discards at most 4096 bytes, returning how many (0 while a connection or
    /// stdin has nothing yet); `closed` at the end (for a `len` of 0 too).
    fn wasi_io_streams_method_input_stream_skip(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        Ok(self.read_stream(self_.0, len)?.len() as u64)
    }

    /// Like `skip`, but waits while stdin or a connection has nothing yet.
    fn wasi_io_streams_method_input_stream_blocking_skip(
        &mut self,
        self_: InputStream,
        len: u64,
    ) -> Result<u64, StreamError> {
        Ok(self.read_stream_waiting(self_.0, len)?.len() as u64)
    }

    fn wasi_sockets_network_resource_drop_network(&mut self, _handle: Network) {}

    /// The one network: handle 0, which no call looks at (the address policy is the host's).
    fn wasi_sockets_instance_network_instance_network(&mut self) -> Network {
        Network(0)
    }

    fn wasi_sockets_tcp_create_socket_create_tcp_socket(
        &mut self,
        address_family: IpAddressFamily,
    ) -> Result<TcpSocket, NetErrorCode> {
        let ipv6 = address_family == IpAddressFamily::Ipv6;
        let slot = self.sockets.insert(NetSocket {
            ipv6,
            state: NetState::Unbound,
            options: TcpOptions::default(),
        });
        Ok(TcpSocket(slot as u32))
    }

    fn wasi_sockets_tcp_resource_drop_tcp_socket(&mut self, handle: TcpSocket) {
        self.sockets.remove(handle.0 as usize);
    }

    /// `access-denied` outside loopback unless `with_external_network` was called.
    fn wasi_sockets_tcp_method_tcp_socket_start_bind(
        &mut self,
        self_: TcpSocket,
        _network: Network,
        local_address: IpSocketAddress,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_start_bind(self_.0, socket_addr(local_address))?)
    }

    fn wasi_sockets_tcp_method_tcp_socket_finish_bind(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_finish_bind(self_.0)?)
    }

    fn wasi_sockets_tcp_method_tcp_socket_start_listen(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_start_listen(self_.0)?)
    }

    fn wasi_sockets_tcp_method_tcp_socket_finish_listen(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_finish_listen(self_.0)?)
    }

    fn wasi_sockets_tcp_method_tcp_socket_local_address(
        &mut self,
        self_: TcpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.tcp_address(self_.0, false)?))
    }

    /// Connects at once; a refusal shows in `finish-connect`.
    fn wasi_sockets_tcp_method_tcp_socket_start_connect(
        &mut self,
        self_: TcpSocket,
        _network: Network,
        remote_address: IpSocketAddress,
    ) -> Result<(), NetErrorCode> {
        Ok(self.tcp_start_connect(self_.0, socket_addr(remote_address))?)
    }

    fn wasi_sockets_tcp_method_tcp_socket_finish_connect(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(InputStream, OutputStream), NetErrorCode> {
        let (input, output) = self.tcp_finish_connect(self_.0)?;
        Ok((InputStream(input as u32), OutputStream(output as u32)))
    }

    /// `would-block` if no connection is waiting.
    fn wasi_sockets_tcp_method_tcp_socket_accept(
        &mut self,
        self_: TcpSocket,
    ) -> Result<(TcpSocket, InputStream, OutputStream), NetErrorCode> {
        let (socket, input, output) = self.tcp_accept(self_.0)?;
        Ok((
            TcpSocket(socket as u32),
            InputStream(input as u32),
            OutputStream(output as u32),
        ))
    }

    fn wasi_sockets_tcp_method_tcp_socket_remote_address(
        &mut self,
        self_: TcpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.tcp_address(self_.0, true)?))
    }

    /// Ready when `accept` would not block (a socket that is not listening is always ready).
    fn wasi_sockets_tcp_method_tcp_socket_subscribe(&mut self, self_: TcpSocket) -> Pollable {
        self.subscribe_wait(Wait::TcpAccept(self_.0 as usize))
    }

    /// Ready when stdin has data or is at its end, or a connection's stream has data or is
    /// at its end; a file is always ready.
    fn wasi_io_streams_method_input_stream_subscribe(&mut self, self_: InputStream) -> Pollable {
        if let Some(slot) = tcp_slot(self_.0) {
            return self.subscribe_wait(Wait::TcpReadable(slot));
        }
        if let Some(slot) = http_slot(self_.0) {
            return self.subscribe_wait(Wait::HttpReadable(slot));
        }
        if self_.0 != 0 {
            return self.subscribe(None);
        }
        self.start_stdin();
        self.subscribe_wait(Wait::Stdin)
    }

    fn wasi_sockets_tcp_method_tcp_socket_is_listening(&mut self, self_: TcpSocket) -> bool {
        matches!(self.socket(self_.0).state, NetState::Listening { .. })
    }

    fn wasi_sockets_tcp_method_tcp_socket_address_family(
        &mut self,
        self_: TcpSocket,
    ) -> IpAddressFamily {
        if self.socket(self_.0).ipv6 {
            IpAddressFamily::Ipv6
        } else {
            IpAddressFamily::Ipv4
        }
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_listen_backlog_size(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        // `std` listens with its own backlog; the size is checked, not applied. While
        // listening it is ignored (as the spec allows), not an error.
        match self.socket(self_.0).state {
            NetState::ConnectStarted(_) | NetState::Connected { .. } | NetState::Closed => {
                Err(Net::InvalidState.into())
            }
            _ => Ok(()),
        }
    }

    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_enabled(
        &mut self,
        self_: TcpSocket,
    ) -> Result<bool, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_enabled)
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_enabled(
        &mut self,
        self_: TcpSocket,
        value: bool,
    ) -> Result<(), NetErrorCode> {
        self.socket(self_.0).options.keep_alive_enabled = value;
        Ok(())
    }

    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_idle_time(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_idle_time)
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_idle_time(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.keep_alive_idle_time = value;
        Ok(())
    }

    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_interval(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_interval)
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_interval(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.keep_alive_interval = value;
        Ok(())
    }

    fn wasi_sockets_tcp_method_tcp_socket_keep_alive_count(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u32, NetErrorCode> {
        Ok(self.socket(self_.0).options.keep_alive_count)
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_keep_alive_count(
        &mut self,
        self_: TcpSocket,
        value: u32,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.keep_alive_count = value;
        Ok(())
    }

    fn wasi_sockets_tcp_method_tcp_socket_hop_limit(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u8, NetErrorCode> {
        Ok(self.socket(self_.0).options.hop_limit)
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_hop_limit(
        &mut self,
        self_: TcpSocket,
        value: u8,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.hop_limit = value;
        Ok(())
    }

    fn wasi_sockets_tcp_method_tcp_socket_receive_buffer_size(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.receive_buffer_size)
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_receive_buffer_size(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.receive_buffer_size = value;
        Ok(())
    }

    fn wasi_sockets_tcp_method_tcp_socket_send_buffer_size(
        &mut self,
        self_: TcpSocket,
    ) -> Result<u64, NetErrorCode> {
        Ok(self.socket(self_.0).options.send_buffer_size)
    }

    fn wasi_sockets_tcp_method_tcp_socket_set_send_buffer_size(
        &mut self,
        self_: TcpSocket,
        value: u64,
    ) -> Result<(), NetErrorCode> {
        if value == 0 {
            return Err(Net::InvalidArgument.into());
        }
        self.socket(self_.0).options.send_buffer_size = value;
        Ok(())
    }

    fn wasi_sockets_tcp_method_tcp_socket_shutdown(
        &mut self,
        self_: TcpSocket,
        shutdown_type: ShutdownType,
    ) -> Result<(), NetErrorCode> {
        let how = match shutdown_type {
            ShutdownType::Receive => Shutdown::Read,
            ShutdownType::Send_ => Shutdown::Write,
            ShutdownType::Both => Shutdown::Both,
        };
        Ok(self.tcp_shutdown(self_.0, how)?)
    }

    fn wasi_sockets_udp_create_socket_create_udp_socket(
        &mut self,
        address_family: IpAddressFamily,
    ) -> Result<UdpSocket, NetErrorCode> {
        let ipv6 = address_family == IpAddressFamily::Ipv6;
        let slot = self.udp_sockets.insert(UdpSock {
            ipv6,
            state: UdpState::Unbound,
            generation: Arc::new(AtomicU64::new(0)),
        });
        Ok(UdpSocket(slot as u32))
    }

    fn wasi_sockets_udp_resource_drop_udp_socket(&mut self, handle: UdpSocket) {
        self.udp_sockets.remove(handle.0 as usize);
    }

    fn wasi_sockets_udp_resource_drop_incoming_datagram_stream(
        &mut self,
        handle: IncomingDatagramStream,
    ) {
        self.udp_in.remove(handle.0 as usize);
    }

    fn wasi_sockets_udp_resource_drop_outgoing_datagram_stream(
        &mut self,
        handle: OutgoingDatagramStream,
    ) {
        self.udp_out.remove(handle.0 as usize);
    }

    /// `access-denied` outside loopback unless `with_external_network` was called.
    fn wasi_sockets_udp_method_udp_socket_start_bind(
        &mut self,
        self_: UdpSocket,
        _network: Network,
        local_address: IpSocketAddress,
    ) -> Result<(), NetErrorCode> {
        Ok(self.udp_start_bind(self_.0, socket_addr(local_address))?)
    }

    fn wasi_sockets_udp_method_udp_socket_finish_bind(
        &mut self,
        self_: UdpSocket,
    ) -> Result<(), NetErrorCode> {
        Ok(self.udp_finish_bind(self_.0)?)
    }

    /// `invalid-state` unless the socket is bound; `Some(remote)` connects the streams to it.
    fn wasi_sockets_udp_method_udp_socket_stream(
        &mut self,
        self_: UdpSocket,
        remote_address: Option<IpSocketAddress>,
    ) -> Result<(IncomingDatagramStream, OutgoingDatagramStream), NetErrorCode> {
        let (input, output) = self.udp_stream(self_.0, remote_address.map(socket_addr))?;
        Ok((
            IncomingDatagramStream(input as u32),
            OutgoingDatagramStream(output as u32),
        ))
    }

    fn wasi_sockets_udp_method_udp_socket_local_address(
        &mut self,
        self_: UdpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.udp_address(self_.0, false)?))
    }

    /// `invalid-state` unless `stream` was called with a remote address.
    fn wasi_sockets_udp_method_udp_socket_remote_address(
        &mut self,
        self_: UdpSocket,
    ) -> Result<IpSocketAddress, NetErrorCode> {
        Ok(ip_socket_address(self.udp_address(self_.0, true)?))
    }

    fn wasi_sockets_udp_method_udp_socket_address_family(
        &mut self,
        self_: UdpSocket,
    ) -> IpAddressFamily {
        if self.udp_socket(self_.0).ipv6 {
            IpAddressFamily::Ipv6
        } else {
            IpAddressFamily::Ipv4
        }
    }

    fn wasi_sockets_udp_method_udp_socket_unicast_hop_limit(
        &mut self,
        _self_: UdpSocket,
    ) -> Result<u8, NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }

    fn wasi_sockets_udp_method_udp_socket_set_unicast_hop_limit(
        &mut self,
        _self_: UdpSocket,
        _value: u8,
    ) -> Result<(), NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }

    fn wasi_sockets_udp_method_udp_socket_receive_buffer_size(
        &mut self,
        _self_: UdpSocket,
    ) -> Result<u64, NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }

    fn wasi_sockets_udp_method_udp_socket_set_receive_buffer_size(
        &mut self,
        _self_: UdpSocket,
        _value: u64,
    ) -> Result<(), NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }

    fn wasi_sockets_udp_method_udp_socket_send_buffer_size(
        &mut self,
        _self_: UdpSocket,
    ) -> Result<u64, NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }

    fn wasi_sockets_udp_method_udp_socket_set_send_buffer_size(
        &mut self,
        _self_: UdpSocket,
        _value: u64,
    ) -> Result<(), NetErrorCode> {
        Err(NetErrorCode::NotSupported)
    }

    /// Always ready: binding completes in `finish-bind`.
    fn wasi_sockets_udp_method_udp_socket_subscribe(&mut self, _self_: UdpSocket) -> Pollable {
        self.subscribe(None)
    }

    /// An empty list if nothing is queued (or `max_results` is 0).
    fn wasi_sockets_udp_method_incoming_datagram_stream_receive(
        &mut self,
        self_: IncomingDatagramStream,
        max_results: u64,
    ) -> Result<Vec<IncomingDatagram>, NetErrorCode> {
        let got = self.udp_receive(self_.0, max_results)?;
        Ok(got
            .into_iter()
            .map(|(data, from)| IncomingDatagram {
                data,
                remote_address: ip_socket_address(from),
            })
            .collect())
    }

    /// Ready when `receive` would return a datagram.
    fn wasi_sockets_udp_method_incoming_datagram_stream_subscribe(
        &mut self,
        self_: IncomingDatagramStream,
    ) -> Pollable {
        self.subscribe_wait(Wait::UdpReadable(self_.0 as usize))
    }

    fn wasi_sockets_udp_method_outgoing_datagram_stream_check_send(
        &mut self,
        self_: OutgoingDatagramStream,
    ) -> Result<u64, NetErrorCode> {
        self.udp_check_send(self_.0);
        Ok(UDP_SEND_BATCH)
    }

    /// Returns how many datagrams were sent (see `udp_send`).
    fn wasi_sockets_udp_method_outgoing_datagram_stream_send(
        &mut self,
        self_: OutgoingDatagramStream,
        datagrams: Vec<OutgoingDatagram>,
    ) -> Result<u64, NetErrorCode> {
        let datagrams = datagrams
            .into_iter()
            .map(|d| (d.data, d.remote_address.map(socket_addr)))
            .collect();
        Ok(self.udp_send(self_.0, datagrams)?)
    }

    /// Always ready: `send` never waits.
    fn wasi_sockets_udp_method_outgoing_datagram_stream_subscribe(
        &mut self,
        _self_: OutgoingDatagramStream,
    ) -> Pollable {
        self.subscribe(None)
    }

    /// Resolves at once (see `resolve_name`); the stream holds the result.
    fn wasi_sockets_ip_name_lookup_resolve_addresses(
        &mut self,
        _network: Network,
        name: String,
    ) -> Result<ResolveAddressStream, NetErrorCode> {
        let found = self.resolve_name(&name)?;
        Ok(ResolveAddressStream(self.resolutions.insert(found) as u32))
    }

    /// The next address, `None` once they are all read; never `would-block`.
    fn wasi_sockets_ip_name_lookup_method_resolve_address_stream_resolve_next_address(
        &mut self,
        self_: ResolveAddressStream,
    ) -> Result<Option<IpAddress>, NetErrorCode> {
        let stream = self
            .resolutions
            .get_mut(self_.0 as usize)
            .expect("unknown or dropped resolve-address-stream");
        Ok(stream.pop_front().map(ip_address))
    }

    /// Always ready: the addresses are known when `resolve-addresses` returns.
    fn wasi_sockets_ip_name_lookup_method_resolve_address_stream_subscribe(
        &mut self,
        _self_: ResolveAddressStream,
    ) -> Pollable {
        self.subscribe(None)
    }

    fn wasi_sockets_ip_name_lookup_resource_drop_resolve_address_stream(
        &mut self,
        handle: ResolveAddressStream,
    ) {
        self.resolutions.remove(handle.0 as usize);
    }
}
