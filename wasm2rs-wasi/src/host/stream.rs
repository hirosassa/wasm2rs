//! `wasi:io/streams`: stdin, stdio, byte-stream reads and writes, and poll waits.

use super::*;

/// The bytes the stdin thread has read and not yet handed to the guest.
#[derive(Default)]
pub(super) struct StdinBuf {
    pub(super) bytes: VecDeque<u8>,
    pub(super) eof: bool,
    /// Why the input ended, when a read failed instead of reaching the end; taken when reported.
    pub(super) error: Option<String>,
}

pub(super) type SharedStdin = Arc<(Mutex<StdinBuf>, Condvar)>;

/// What a pollable waits for.
#[derive(Clone, Copy)]
pub(super) enum Wait {
    /// Always ready.
    Ready,
    /// Ready at the instant.
    Deadline(Instant),
    /// Ready when stdin has data or is at its end.
    Stdin,
    /// Ready when the listening socket in the slot has a connection to accept (or is not
    /// listening).
    TcpAccept(SlotRef),
    /// Ready when the stream in `tcp_in` at the slot has data, is at its end or failed.
    TcpReadable(SlotRef),
    /// Ready when the stream in `tcp_out` at the slot has room in its send queue (after it
    /// was drained as far as the socket takes) or failed.
    TcpWritable(SlotRef),
    /// Ready when the stream in `udp_in` at the slot has a datagram it would return (or failed).
    UdpReadable(SlotRef),
    /// Ready when the future in `http_futures` at the slot has its result.
    HttpResponse(SlotRef),
    /// Ready when the body in `http_bodies` at the slot has bytes or its end to read.
    HttpReadable(SlotRef),
}

impl Wait {
    pub(super) fn is_ready(self, now: Instant, stdin: &StdinBuf, host: &WasiHost) -> bool {
        match self {
            Wait::Ready => true,
            Wait::Deadline(d) => d <= now,
            Wait::Stdin => !stdin.bytes.is_empty() || stdin.eof,
            Wait::TcpAccept(r) => host.accept_ready(r),
            Wait::TcpReadable(r) => host.readable(r),
            Wait::TcpWritable(r) => host.tcp_writable(r),
            Wait::UdpReadable(r) => host.udp_readable(r),
            Wait::HttpResponse(r) => host.http_future_ready(r),
            Wait::HttpReadable(r) => host.http_body_readable(r),
        }
    }

    /// Whether readiness can only be found by asking the socket again.
    pub(super) fn polls_socket(self) -> bool {
        matches!(
            self,
            Wait::TcpAccept(_)
                | Wait::TcpReadable(_)
                | Wait::TcpWritable(_)
                | Wait::UdpReadable(_)
                | Wait::HttpResponse(_)
                | Wait::HttpReadable(_)
        )
    }

    pub(super) fn deadline(self) -> Option<Instant> {
        match self {
            Wait::Deadline(d) => Some(d),
            _ => None,
        }
    }
}

/// The first handle of the streams of TCP connections: the slot in `tcp_in` or `tcp_out`
/// is added to it (file stream handles stay far below).
pub(super) const TCP_STREAM: usize = 1 << 30;

/// The first handle of the streams of incoming bodies (`wasi:http`): the slot in `http_bodies`
/// is added to it. The range lies between the file streams and `TCP_STREAM`.
pub(super) const HTTP_STREAM: usize = 1 << 29;

/// The first handle of the streams of outgoing bodies: the slot in `http_out_bodies` is added to
/// it. Input streams take the range below it.
pub(super) const HTTP_OUT_STREAM: usize = HTTP_STREAM + (1 << 28);

/// The slot in `http_bodies` of a stream handle, `None` for any other stream.
pub(super) fn http_slot(stream: u32) -> Option<usize> {
    let slot = (stream as usize).checked_sub(HTTP_STREAM)?;
    (slot < HTTP_OUT_STREAM - HTTP_STREAM).then_some(slot)
}

/// The slot in `http_out_bodies` of a stream handle, `None` for any other stream.
pub(super) fn http_out_slot(stream: u32) -> Option<usize> {
    let slot = (stream as usize).checked_sub(HTTP_OUT_STREAM)?;
    (slot < TCP_STREAM - HTTP_OUT_STREAM).then_some(slot)
}

/// The slot in `tcp_in` or `tcp_out` of a stream handle, `None` for any other stream.
pub(super) fn tcp_slot(stream: u32) -> Option<usize> {
    (stream as usize).checked_sub(TCP_STREAM)
}

impl WasiHost {
    /// `last-operation-failed` for a failure of the last operation on a stream: a fresh
    /// `error` handle (one per call, live until the guest drops it) that carries `message`.
    /// The caller reports it once: the stream is `closed` from then on.
    pub(super) fn last_operation_failed(&mut self, message: impl std::fmt::Display) -> StreamError {
        self.last_operation_failed_with(message, None, None)
    }

    /// `last_operation_failed`, remembering the filesystem or HTTP `error-code` behind the failure.
    pub(super) fn last_operation_failed_with(
        &mut self,
        message: impl std::fmt::Display,
        fs: Option<Fs>,
        http: Option<HttpErrorCode>,
    ) -> StreamError {
        let entry = ErrorEntry {
            message: message.to_string(),
            fs,
            http,
        };
        StreamError::LastOperationFailed(Error(self.errors.insert(entry) as u32))
    }

    /// `last_operation_failed` for an I/O failure of `stream`: only a file stream's carries a
    /// filesystem `error-code`; a connection's is not a filesystem error. (The streams of http
    /// bodies report their failures elsewhere and never get here.)
    pub(super) fn stream_failed(&mut self, stream: u32, e: &std::io::Error) -> StreamError {
        let is_file = tcp_slot(stream).is_none() && file_index(stream).is_some();
        let fs = is_file.then(|| io_error_code(std::io::Error::from(e.kind())));
        self.last_operation_failed_with(e, fs, None)
    }

    /// The first read failure of the input stream `stream` is `last-operation-failed`; the
    /// stream is `closed` after it (see `read_stream`).
    pub(super) fn fail_input(&mut self, stream: u32, e: &std::io::Error) -> StreamError {
        self.failed_in.insert(stream);
        self.stream_failed(stream, e)
    }

    /// `fail_input` for the output stream `stream` that is not a connection.
    pub(super) fn fail_output(&mut self, stream: u32, e: &std::io::Error) -> StreamError {
        self.failed_out.insert(stream);
        self.stream_failed(stream, e)
    }

    /// Why the output stream in `tcp_out` that `r` refers to cannot be written: `last-operation-failed`
    /// the first time after its socket failed, `closed` after that and when it was dropped.
    pub(super) fn tcp_out_error(&mut self, slot: usize) -> StreamError {
        match self.tcp_out.get(slot).and_then(TcpOut::take_failure) {
            Some(why) => self.last_operation_failed(why),
            None => StreamError::Closed,
        }
    }

    /// `Ok` while the connection in `tcp_out` at `slot` has not failed, else its error (see
    /// `tcp_out_error`).
    pub(super) fn tcp_status(&mut self, slot: usize) -> Result<(), StreamError> {
        match self.tcp_out.get(slot) {
            Some(conn) if !conn.failed() => Ok(()),
            _ => Err(self.tcp_out_error(slot)),
        }
    }
}

impl WasiHost {
    /// Reads at most 4096 bytes of a stream without waiting: the one read path of `read`,
    /// `blocking-read`, `skip`, `blocking-skip`, `splice` and `blocking-splice`.
    pub(super) fn read_stream(&mut self, stream: u32, len: u64) -> Result<Vec<u8>, StreamError> {
        let mut buf = vec![0u8; len.min(4096) as usize];
        if stream == 0 {
            self.start_stdin();
            let stdin = Arc::clone(&self.stdin);
            let mut shared = stdin.0.lock().unwrap();
            // a zero length consumes nothing: ok while the input is open, like a read with data left
            let n = std::io::Read::read(&mut shared.bytes, &mut buf).unwrap_or(0); // a `VecDeque` never fails
            if n == 0 && shared.bytes.is_empty() && shared.eof {
                // the end of the input: `closed`, unless a read failed: that is reported once,
                // after the bytes before it, and the input is `closed` from then on
                return Err(match shared.error.take() {
                    Some(why) => self.last_operation_failed(why),
                    None => StreamError::Closed,
                });
            }
            buf.truncate(n);
            return Ok(buf);
        }
        if self.failed_in.contains(&stream) {
            // a failure was reported: the stream is closed, a zero-length read included
            return Err(StreamError::Closed);
        }
        if let Some(slot) = http_slot(stream) {
            let body = self
                .http_bodies
                .get(slot)
                .expect("read from an unknown or dropped stream")
                .shared();
            return match body.read(buf.len()) {
                http::BodyRead::Data(data) => Ok(data),
                http::BodyRead::Closed => Err(StreamError::Closed),
                http::BodyRead::Failed(kind, why) => {
                    self.failed_in.insert(stream);
                    let code = http::body_error_code(kind, &why);
                    Err(self.last_operation_failed_with(why, None, Some(code)))
                }
            };
        }
        if let Some(slot) = tcp_slot(stream) {
            // never blocks: an empty list means "nothing yet"
            let conn = self
                .tcp_in
                .get_mut(slot)
                .expect("read from an unknown or dropped stream");
            if buf.is_empty() {
                // a zero length succeeds only while the connection is open: peek, consume nothing
                return match conn.peek(&mut [0u8; 1]) {
                    Ok(0) => Err(StreamError::Closed),
                    Ok(_) => Ok(buf),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(buf),
                    Err(e) => Err(self.fail_input(stream, &e)),
                };
            }
            return match std::io::Read::read(conn, &mut buf) {
                Ok(0) => Err(StreamError::Closed),
                Ok(n) => {
                    buf.truncate(n);
                    Ok(buf)
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(Vec::new()),
                Err(e) => Err(self.fail_input(stream, &e)),
            };
        }
        let Some(index) = file_index(stream) else {
            return Err(StreamError::Closed);
        };
        let file = self
            .files
            .get_mut(index)
            .expect("read from an unknown or dropped stream");
        if buf.is_empty() {
            // a zero length succeeds only before the end of the file: look, consume nothing
            let at_end = file
                .file
                .metadata()
                .map_or(true, |meta| file.pos >= meta.len());
            return if at_end {
                Err(StreamError::Closed)
            } else {
                Ok(buf)
            };
        }
        match file.read(&mut buf) {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Ok(buf)
            }
            Ok(_) => Err(StreamError::Closed), // the end of the file
            Err(e) => Err(self.fail_input(stream, &e)),
        }
    }

    /// `read_stream`, but waits (as the stream's `subscribe` would) while an open stream has
    /// nothing yet: stdin and connections; a file never waits. A zero length waits, too, until
    /// the stream is readable (data or its end), then answers like `read_stream(.., 0)`: ok
    /// and empty while open, `closed` at the end.
    pub(super) fn read_stream_waiting(
        &mut self,
        stream: u32,
        len: u64,
    ) -> Result<Vec<u8>, StreamError> {
        loop {
            let data = self.read_stream(stream, len)?;
            if !data.is_empty() {
                return Ok(data);
            }
            let wait = match (tcp_slot(stream), http_slot(stream)) {
                (Some(slot), _) => Wait::TcpReadable(self.tcp_in.slot_ref(slot)),
                (None, Some(slot)) => Wait::HttpReadable(self.http_bodies.slot_ref(slot)),
                (None, None) if stream == 0 => Wait::Stdin,
                (None, None) => return Ok(data),
            };
            self.wait_any(&[wait]);
            if len == 0 {
                return self.read_stream(stream, 0);
            }
        }
    }

    /// Whether an output stream can still be written (not dropped or unknown): `splice`
    /// asks before it reads, so a dead output does not consume the source's bytes.
    pub(super) fn output_is_open(&self, stream: u32) -> bool {
        if let Some(slot) = tcp_slot(stream) {
            self.tcp_out.get(slot).is_some()
        } else if let Some(slot) = http_out_slot(stream) {
            self.http_out_bodies.get(slot).is_some()
        } else if let Some(index) = file_index(stream) {
            self.outputs.get(index).is_some()
        } else {
            true // stdout and stderr
        }
    }

    /// How many bytes `splice` may read for `stream`: `len`, for a connection at most its
    /// `check-write` permit. With `wait` the queue is emptied first (waiting for the peer).
    pub(super) fn splice_budget(
        &mut self,
        stream: u32,
        len: u64,
        wait: bool,
    ) -> Result<u64, StreamError> {
        let Some(slot) = tcp_slot(stream) else {
            return Ok(len);
        };
        let conn = self.tcp_out.get(slot).ok_or(StreamError::Closed)?;
        if wait {
            conn.flush_waiting();
        }
        match conn.permit() {
            Some(permit) => Ok(len.min(permit)),
            None => Err(self.tcp_out_error(slot)),
        }
    }

    /// Traps if a non-blocking write of `len` bytes to a connection is more than `check-write`
    /// permits now (its send queue's room, up to 4096); for a connection that failed or is gone it
    /// answers like `check-write` (`last-operation-failed` once, then `closed`).
    /// The other streams are not checked here.
    pub(super) fn assert_within_permit(
        &mut self,
        stream: u32,
        len: u64,
        op: &str,
    ) -> Result<(), StreamError> {
        if let Some(slot) = tcp_slot(stream) {
            let permit = self
                .tcp_permit(slot)
                .ok_or_else(|| self.tcp_out_error(slot))?;
            assert!(len <= permit, "{op}: more than check-write allows");
        }
        Ok(())
    }

    /// Writes all of `contents` to a stream: the one write path of `write`,
    /// `blocking-write-and-flush`, `write-zeroes`, `blocking-write-zeroes-and-flush`,
    /// `splice` and `blocking-splice`. A failed write is `last-operation-failed` once (the first
    /// time the failure shows), then the stream is `closed`. A connection's bytes go
    /// through its send queue (see `TcpOut`), so their order is kept: with `wait` false
    /// (`write`, and `splice` / `blocking-splice`, which read within the permit) they are queued
    /// and sent as far as the socket takes them; with `wait` the queue is then emptied, waiting
    /// for the peer to read.
    pub(super) fn write_stream(
        &mut self,
        stream: u32,
        contents: &[u8],
        wait: bool,
    ) -> Result<(), StreamError> {
        let written = if let Some(slot) = tcp_slot(stream) {
            let conn = self.tcp_out.get(slot).ok_or(StreamError::Closed)?;
            conn.push(contents);
            if wait {
                conn.flush_waiting();
            }
            return self.tcp_status(slot);
        } else if self.failed_out.contains(&stream) {
            return Err(StreamError::Closed); // a failure was reported: the stream is closed
        } else if let Some(slot) = http_out_slot(stream) {
            return self.http_out_write(stream, slot, contents);
        } else {
            match (stream, file_index(stream)) {
                (_, Some(index)) => match self.outputs.get_mut(index) {
                    Some(file) => file.write_all(contents),
                    None => Err(std::io::ErrorKind::NotFound.into()),
                },
                (_, None) => self.with_stdio(stream, |w| w.write_all(contents)),
            }
        };
        written.map_err(|e| match e.kind() {
            // a dropped file output (see above) is gone, not failed
            std::io::ErrorKind::NotFound => StreamError::Closed,
            _ => self.fail_output(stream, &e),
        })
    }

    /// Appends to the body behind an output stream; a body over the limit fails the stream
    /// (`last-operation-failed` once, then `closed`).
    pub(super) fn http_out_write(
        &mut self,
        stream: u32,
        slot: usize,
        contents: &[u8],
    ) -> Result<(), StreamError> {
        self.http_out_handle(slot)?
            .push(contents)
            .map_err(|code| self.fail_http_out(stream, code))
    }

    /// `check-write` of an output stream that is a request body.
    pub(super) fn http_out_permit(&mut self, stream: u32, slot: usize) -> Result<u64, StreamError> {
        if self.failed_out.contains(&stream) {
            return Err(StreamError::Closed);
        }
        self.http_out_handle(slot)?
            .permit()
            .map_err(|code| self.fail_http_out(stream, code))
    }

    /// The buffer behind an output stream; a body already finished or dropped is `closed`.
    pub(super) fn http_out_handle(&self, slot: usize) -> Result<http::OutBodyHandle, StreamError> {
        self.http_out_bodies
            .get(slot)
            .map(http::HttpOutgoingBody::handle)
            .ok_or(StreamError::Closed)
    }

    pub(super) fn fail_http_out(&mut self, stream: u32, code: HttpErrorCode) -> StreamError {
        self.failed_out.insert(stream);
        self.last_operation_failed_with(format!("{code:?}"), None, Some(code))
    }

    /// Runs `f` on stderr for stream 2 and on stdout otherwise: the given writer, else the
    /// process's.
    pub(super) fn with_stdio<T>(&mut self, stream: u32, f: impl FnOnce(&mut dyn Write) -> T) -> T {
        let given = if stream == 2 {
            &mut self.stderr
        } else {
            &mut self.stdout
        };
        match given {
            Some(w) => f(w.as_mut()),
            None if stream == 2 => f(&mut std::io::stderr()),
            None => f(&mut std::io::stdout()),
        }
    }

    /// Flushes stdout and stderr (an exit and a drop end with it); a failure has nobody left to
    /// tell, so it is dropped.
    pub(super) fn flush_stdio(&mut self) {
        self.with_stdio(1, |w| w.flush()).ok();
        self.with_stdio(2, |w| w.flush()).ok();
    }

    /// Flushes a stream; only stdout, stderr (when given) and a connection's send queue
    /// buffer anything. With `wait` (`blocking-flush`, and what a blocking write ends with)
    /// the queue is emptied; without it (`flush`) it is drained as far as the socket takes now
    /// and the flush stays pending.
    pub(super) fn flush_stream(&mut self, stream: u32, wait: bool) -> Result<(), StreamError> {
        if let Some(slot) = tcp_slot(stream) {
            let conn = self.tcp_out.get(slot).ok_or(StreamError::Closed)?;
            if wait {
                conn.flush_waiting();
            } else {
                conn.flush();
            }
            return self.tcp_status(slot);
        }
        if self.failed_out.contains(&stream) {
            return Err(StreamError::Closed); // a failure was reported: the stream is closed
        }
        if file_index(stream).is_some() {
            return Ok(()); // a file holds nothing
        }
        let flushed = match stream {
            1 | 2 => self.with_stdio(stream, |w| w.flush()),
            _ => Ok(()),
        };
        flushed.map_err(|e| self.fail_output(stream, &e))
    }
}

/// `base` plus `when` nanoseconds. A guest can ask for more than an `Instant` holds (it
/// depends on the platform), and adding that would panic; such a deadline is replaced by a
/// distant one that is representable (a century, or a year if even that is not), so it is
/// never reached in practice; only if not even a year fits is `base` itself returned.
pub(super) fn deadline_after(base: Instant, when: u64) -> Instant {
    const YEAR: u64 = 365 * 24 * 60 * 60;
    [
        Duration::from_nanos(when),
        Duration::from_secs(100 * YEAR),
        Duration::from_secs(YEAR),
    ]
    .into_iter()
    .find_map(|d| base.checked_add(d))
    .unwrap_or(base)
}
