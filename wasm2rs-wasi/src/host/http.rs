//! The state and validation behind the `wasi:http/types` resources `fields`, `request-options`,
//! `outgoing-request`, `incoming-request` and `outgoing-response`, and the threads behind
//! `future-incoming-response` and `incoming-body`, and the buffer behind `outgoing-body`.

use super::NetworkPolicy;
use super::sources::{HttpClient, HttpClientError, HttpRequest, HttpResponse};
use crate::wasi_http_types::{ErrorCode, HeaderError, Method, Scheme};
use std::collections::VecDeque;
use std::io::{ErrorKind, Read};
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// Header names the host does not let a guest set (the same as wasmtime's).
const FORBIDDEN: [&str; 10] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "transfer-encoding",
    "upgrade",
    "http2-settings",
    "host",
];

/// A header field: its name is matched case-insensitively and keeps the spelling it was
/// first given.
#[derive(Debug, Clone)]
struct Field {
    lower: String,
    name: String,
    values: Vec<Vec<u8>>,
}

/// A `fields` resource. Fields of a request the guest built are immutable.
#[derive(Debug, Clone, Default)]
pub(super) struct HttpFields {
    fields: Vec<Field>,
    immutable: bool,
}

/// A `tchar` (RFC 9110 token) is allowed in a method and a header name.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn is_header_value(v: &[u8]) -> bool {
    v.iter().all(|&b| b == b'\t' || (b >= 0x20 && b != 0x7f))
}

fn check_name(name: &str) -> Result<String, HeaderError> {
    if !is_token(name) {
        return Err(HeaderError::InvalidSyntax);
    }
    let lower = name.to_ascii_lowercase();
    if FORBIDDEN.contains(&lower.as_str()) {
        return Err(HeaderError::Forbidden);
    }
    Ok(lower)
}

impl HttpFields {
    /// Fields from a list, validated as `append` would.
    pub(super) fn from_list(entries: Vec<(String, Vec<u8>)>) -> Result<Self, HeaderError> {
        let mut fields = HttpFields::default();
        for (name, value) in entries {
            fields.append(name, value)?;
        }
        Ok(fields)
    }

    /// A copy; `immutable` marks the view of a request's headers.
    pub(super) fn copy(&self, immutable: bool) -> Self {
        HttpFields {
            fields: self.fields.clone(),
            immutable,
        }
    }

    /// The fields of a received message, immutable. Nothing is validated: a peer may send
    /// names and values a guest could not set (`transfer-encoding`, ...).
    pub(super) fn received(entries: Vec<(String, Vec<u8>)>) -> Self {
        let mut fields = HttpFields::default();
        for (name, value) in entries {
            let lower = name.to_ascii_lowercase();
            match fields.fields.iter_mut().find(|f| f.lower == lower) {
                Some(field) => field.values.push(value),
                None => fields.fields.push(Field {
                    lower,
                    name,
                    values: vec![value],
                }),
            }
        }
        fields.freeze()
    }

    pub(super) fn freeze(mut self) -> Self {
        self.immutable = true;
        self
    }

    fn mutable(&self) -> Result<(), HeaderError> {
        if self.immutable {
            Err(HeaderError::Immutable)
        } else {
            Ok(())
        }
    }

    pub(super) fn get(&self, name: &str) -> Vec<Vec<u8>> {
        let lower = name.to_ascii_lowercase();
        self.fields
            .iter()
            .find(|f| f.lower == lower)
            .map(|f| f.values.clone())
            .unwrap_or_default()
    }

    pub(super) fn has(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        self.fields.iter().any(|f| f.lower == lower)
    }

    pub(super) fn set(&mut self, name: String, values: Vec<Vec<u8>>) -> Result<(), HeaderError> {
        self.mutable()?;
        let lower = check_name(&name)?;
        if !values.iter().all(|v| is_header_value(v)) {
            return Err(HeaderError::InvalidSyntax);
        }
        let field = Field {
            lower,
            name,
            values,
        };
        match self.fields.iter_mut().find(|f| f.lower == field.lower) {
            Some(_) if field.values.is_empty() => self.fields.retain(|f| f.lower != field.lower),
            Some(slot) => *slot = field,
            None if field.values.is_empty() => {}
            None => self.fields.push(field),
        }
        Ok(())
    }

    pub(super) fn delete(&mut self, name: &str) -> Result<(), HeaderError> {
        self.mutable()?;
        let lower = check_name(name)?;
        self.fields.retain(|f| f.lower != lower);
        Ok(())
    }

    pub(super) fn append(&mut self, name: String, value: Vec<u8>) -> Result<(), HeaderError> {
        self.mutable()?;
        let lower = check_name(&name)?;
        if !is_header_value(&value) {
            return Err(HeaderError::InvalidSyntax);
        }
        match self.fields.iter_mut().find(|f| f.lower == lower) {
            Some(field) => field.values.push(value),
            None => self.fields.push(Field {
                lower,
                name,
                values: vec![value],
            }),
        }
        Ok(())
    }

    pub(super) fn entries(&self) -> Vec<(String, Vec<u8>)> {
        self.fields
            .iter()
            .flat_map(|f| f.values.iter().map(|v| (f.name.clone(), v.clone())))
            .collect()
    }
}

/// A `request-options` resource: the timeouts in nanoseconds.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct HttpOptions {
    pub(super) connect: Option<u64>,
    pub(super) first_byte: Option<u64>,
    pub(super) between_bytes: Option<u64>,
}

/// An `outgoing-request` resource. `body` is set once the guest asked for it.
#[derive(Clone)]
pub(super) struct HttpOutgoingRequest {
    pub(super) method: Method,
    pub(super) path_with_query: Option<String>,
    pub(super) scheme: Option<Scheme>,
    pub(super) authority: Option<String>,
    pub(super) headers: HttpFields,
    pub(super) body: Option<OutBodyHandle>,
}

impl HttpOutgoingRequest {
    pub(super) fn new(headers: HttpFields) -> Self {
        HttpOutgoingRequest {
            method: Method::Get,
            path_with_query: None,
            scheme: None,
            authority: None,
            headers: headers.freeze(),
            body: None,
        }
    }
}

/// What an embedder gives `WasiHost::new_incoming_request`: the parts of a request it received,
/// for the guest's `incoming-handler#handle`.
#[derive(Debug, Clone)]
pub struct IncomingRequestParts {
    pub method: Method,
    pub path_with_query: Option<String>,
    pub scheme: Option<Scheme>,
    pub authority: Option<String>,
    /// Header names and values, in order; a name may repeat.
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

/// The response a guest set with `response-outparam.set`, as `WasiHost::take_response` returns
/// it: the guest's handles are gone, so this is plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedResponse {
    pub status: u16,
    /// Header names and values, in order; a name may repeat.
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

/// An `incoming-request` resource. Its headers are immutable; the body can be consumed once.
pub(super) struct HttpIncomingRequest {
    pub(super) method: Method,
    pub(super) path_with_query: Option<String>,
    pub(super) scheme: Option<Scheme>,
    pub(super) authority: Option<String>,
    pub(super) headers: HttpFields,
    pub(super) body: Option<Vec<u8>>,
}

impl HttpIncomingRequest {
    pub(super) fn new(parts: IncomingRequestParts) -> Self {
        HttpIncomingRequest {
            method: parts.method,
            path_with_query: parts.path_with_query,
            scheme: parts.scheme,
            authority: parts.authority,
            headers: HttpFields::received(parts.headers),
            body: Some(parts.body),
        }
    }
}

/// An `outgoing-response` resource. `body` is set once the guest asked for it.
pub(super) struct HttpOutgoingResponse {
    pub(super) status: u16,
    pub(super) headers: HttpFields,
    pub(super) body: Option<OutBodyHandle>,
}

impl HttpOutgoingResponse {
    pub(super) fn new(headers: HttpFields) -> Self {
        HttpOutgoingResponse {
            status: 200,
            headers: headers.freeze(),
            body: None,
        }
    }

    /// The response as plain data. A body the guest never finished (it may still be writing it
    /// after `handle` returned) cannot be completed any more, so the response fails.
    pub(super) fn serve(self) -> Result<ServedResponse, ErrorCode> {
        let body = match self.body {
            Some(body) => body.take_finished()?,
            None => Vec::new(),
        };
        Ok(ServedResponse {
            status: self.status,
            headers: self.headers.entries(),
            body,
        })
    }
}

/// Whether `status` is accepted as a status code: `100..=999`, as wasmtime does (a client may
/// send a three-digit code beyond the registered `100..=599`).
pub(super) fn valid_status(status: u16) -> bool {
    (100..=999).contains(&status)
}

/// Whether `method` is a token (the named methods always are).
pub(super) fn valid_method(method: &Method) -> bool {
    match method {
        Method::Other(m) => is_token(m),
        _ => true,
    }
}

/// A scheme is `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`.
pub(super) fn valid_scheme(scheme: &Scheme) -> bool {
    match scheme {
        Scheme::Other(s) => {
            s.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        }
        _ => true,
    }
}

/// Visible ASCII only, without the `#` that would start a fragment.
pub(super) fn valid_path_with_query(path: &str) -> bool {
    !path.is_empty()
        && path
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'#')
}

/// Visible ASCII without the delimiters that end an authority (a backslash ends it for
/// clients that follow the WHATWG URL standard, so it would make the host and the client
/// disagree about the destination).
pub(super) fn valid_authority(authority: &str) -> bool {
    !authority.is_empty()
        && authority
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !matches!(b, b'/' | b'?' | b'#' | b'\\'))
}

/// Locks `m`; a thread that panicked while holding it leaves data that is still usable here.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The host part of an authority (`userinfo@host:port`, a bracketed IPv6 address included).
fn authority_host(authority: &str) -> &str {
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if let Some(rest) = host_port.strip_prefix('[') {
        return rest.split_once(']').map_or(rest, |(ip, _)| ip);
    }
    host_port.rsplit_once(':').map_or(host_port, |(h, _)| h)
}

/// Whether `policy` lets a request go to `authority`. Only the guest's own URL is judged: a
/// redirect the client follows is the client's business. Names other than `localhost` are not
/// resolved here, so under `Loopback` they are refused.
pub(super) fn destination_allowed(policy: NetworkPolicy, authority: &str) -> bool {
    match policy {
        NetworkPolicy::None => false,
        NetworkPolicy::External => true,
        NetworkPolicy::Loopback => {
            if authority.contains('\\') {
                return false; // clients disagree about where the authority ends
            }
            let host = authority_host(authority);
            match host.parse::<IpAddr>() {
                Ok(ip) => ip.is_loopback(),
                Err(_) => host.trim_end_matches('.').eq_ignore_ascii_case("localhost"),
            }
        }
    }
}

/// The method as the client sends it.
pub(super) fn method_name(method: &Method) -> String {
    match method {
        Method::Get => "GET".into(),
        Method::Head => "HEAD".into(),
        Method::Post => "POST".into(),
        Method::Put => "PUT".into(),
        Method::Delete => "DELETE".into(),
        Method::Connect => "CONNECT".into(),
        Method::Options => "OPTIONS".into(),
        Method::Trace => "TRACE".into(),
        Method::Patch => "PATCH".into(),
        Method::Other(m) => m.clone(),
    }
}

pub(super) fn scheme_name(scheme: &Scheme) -> String {
    match scheme {
        Scheme::HTTP => "http".into(),
        Scheme::HTTPS => "https".into(),
        Scheme::Other(s) => s.clone(),
    }
}

/// What the thread of a `future-incoming-response` fills.
struct FutureShared {
    result: Mutex<Option<Result<HttpResponse, ErrorCode>>>,
    /// The guest dropped the future: a late response is dropped, not kept.
    cancelled: AtomicBool,
}

/// A `future-incoming-response`: a thread runs the client's `send` and leaves the result in a
/// shared slot, which `get` takes.
pub(super) struct HttpFuture {
    shared: Arc<FutureShared>,
    taken: bool,
}

impl HttpFuture {
    /// Starts `send` of `request` on a thread. The client is shared by all requests, so two
    /// requests in flight take turns inside `send` (the body is read outside it).
    ///
    /// With a `body`, the thread first waits for the guest to finish it (the guest may call
    /// `handle` before it writes the body); a body dropped or refused instead fails the future
    /// without running the client.
    pub(super) fn spawn(
        client: Arc<Mutex<Box<dyn HttpClient>>>,
        request: HttpRequest,
        body: Option<OutBodyHandle>,
    ) -> Self {
        let shared = Arc::new(FutureShared {
            result: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        });
        let slot = Arc::clone(&shared);
        std::thread::spawn(move || {
            let result = send_when_finished(&client, request, body);
            if !slot.cancelled.load(Ordering::SeqCst) {
                *lock(&slot.result) = Some(result);
            }
        });
        HttpFuture {
            shared,
            taken: false,
        }
    }

    /// Whether `get` has something to answer (also once the result was taken).
    pub(super) fn is_ready(&self) -> bool {
        self.taken || lock(&self.shared.result).is_some()
    }

    /// `get`: `None` while pending, the result once, then `Some(Err(()))`.
    pub(super) fn take(&mut self) -> Option<Result<Result<HttpResponse, ErrorCode>, ()>> {
        if self.taken {
            return Some(Err(()));
        }
        let result = lock(&self.shared.result).take()?;
        self.taken = true;
        Some(Ok(result))
    }

    pub(super) fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::SeqCst);
        // a response that arrived before the guest gave up
        drop(lock(&self.shared.result).take());
    }
}

/// Waits for `body` to be finished, then sends `request` with it.
fn send_when_finished(
    client: &Mutex<Box<dyn HttpClient>>,
    mut request: HttpRequest,
    body: Option<OutBodyHandle>,
) -> Result<HttpResponse, ErrorCode> {
    if let Some(body) = body {
        request.body = body.wait_finished()?;
    }
    // a client that panics must not leave the guest waiting for ever
    std::panic::catch_unwind(AssertUnwindSafe(|| lock(client).send(request)))
        .unwrap_or_else(|_| Err(HttpClientError::new("the http client panicked")))
        .map_err(|e| ErrorCode::InternalError(Some(e.to_string())))
}

/// An `incoming-response`: its body is read once `consume` has been called.
pub(super) struct HttpIncomingResponse {
    pub(super) status: u16,
    pub(super) headers: HttpFields,
    pub(super) body: Option<Box<dyn Read + Send>>,
}

impl HttpIncomingResponse {
    pub(super) fn new(response: HttpResponse) -> Self {
        HttpIncomingResponse {
            status: response.status,
            headers: HttpFields::received(response.headers),
            body: Some(response.body),
        }
    }
}

/// How an `outgoing-body` stands.
enum OutPhase {
    Open,
    Finished,
    /// Dropped before `finish`, refused (trailers) or over the limit.
    Failed(ErrorCode),
}

struct OutState {
    bytes: Vec<u8>,
    phase: OutPhase,
}

impl OutState {
    /// The failure of a failed body, which every later operation reports.
    fn check_not_failed(&self) -> Result<(), ErrorCode> {
        match &self.phase {
            OutPhase::Failed(code) => Err(code.clone()),
            OutPhase::Open | OutPhase::Finished => Ok(()),
        }
    }
}

struct OutBodyShared {
    state: Mutex<OutState>,
    changed: Condvar,
    limit: usize,
    /// The `error-code` of a body over the limit: request or response body size.
    too_large: fn(Option<u64>) -> ErrorCode,
}

/// The most a stream's `check-write` allows at once.
const WRITE_PERMIT: usize = 4096;

/// A reference to the buffer of an `outgoing-body`, for the output stream and the request
/// thread.
#[derive(Clone)]
pub(super) struct OutBodyHandle(Arc<OutBodyShared>);

impl OutBodyHandle {
    /// What `check-write` answers: the room left under the limit, up to 4096.
    pub(super) fn permit(&self) -> Result<u64, ErrorCode> {
        let state = lock(&self.0.state);
        state.check_not_failed()?;
        let room = self.0.limit.saturating_sub(state.bytes.len());
        // A full body still permits a write: 0 would make a guest that waits on the stream
        // and asks again spin for ever, while the write that passes the limit fails with
        // `HTTP-request-body-size`.
        Ok(if room == 0 {
            WRITE_PERMIT
        } else {
            room.min(WRITE_PERMIT)
        } as u64)
    }

    /// Appends `contents`; a body that would pass the limit fails for good instead.
    pub(super) fn push(&self, contents: &[u8]) -> Result<(), ErrorCode> {
        let mut state = lock(&self.0.state);
        state.check_not_failed()?;
        let size = state.bytes.len().saturating_add(contents.len());
        if size > self.0.limit {
            let code = (self.0.too_large)(Some(size as u64));
            self.settle(state, OutPhase::Failed(code.clone()));
            return Err(code);
        }
        state.bytes.extend_from_slice(contents);
        Ok(())
    }

    /// Closes the body for the request thread; a failed body stays failed.
    pub(super) fn finish(&self) -> Result<(), ErrorCode> {
        let state = lock(&self.0.state);
        state.check_not_failed()?;
        self.settle(state, OutPhase::Finished);
        Ok(())
    }

    /// Fails an open body (`finish` with trailers, a drop): the request will not run.
    pub(super) fn fail(&self, code: ErrorCode) {
        let state = lock(&self.0.state);
        if matches!(state.phase, OutPhase::Open) {
            self.settle(state, OutPhase::Failed(code));
        }
    }

    /// Moves the body to its final `phase` and wakes the request thread.
    fn settle(&self, mut state: MutexGuard<'_, OutState>, phase: OutPhase) {
        state.phase = phase;
        drop(state);
        self.0.changed.notify_all();
    }

    /// The bytes of a body that is finished already; a failed body's failure, and an error for
    /// one that is still open.
    pub(super) fn take_finished(&self) -> Result<Vec<u8>, ErrorCode> {
        let mut state = lock(&self.0.state);
        match &state.phase {
            OutPhase::Finished => Ok(std::mem::take(&mut state.bytes)),
            OutPhase::Failed(code) => Err(code.clone()),
            OutPhase::Open => Err(ErrorCode::InternalError(Some(
                "the response body was not finished".into(),
            ))),
        }
    }

    /// The bytes, once the guest finished the body; the failure if it never will.
    fn wait_finished(&self) -> Result<Vec<u8>, ErrorCode> {
        let mut state = lock(&self.0.state);
        loop {
            match &state.phase {
                OutPhase::Finished => return Ok(std::mem::take(&mut state.bytes)),
                OutPhase::Failed(code) => return Err(code.clone()),
                OutPhase::Open => {
                    state = self
                        .0
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }
}

/// An `outgoing-body`. Dropping it before `finish` fails the request or the response.
pub(super) struct HttpOutgoingBody {
    handle: OutBodyHandle,
    stream_taken: bool,
}

impl HttpOutgoingBody {
    pub(super) fn new(limit: usize, too_large: fn(Option<u64>) -> ErrorCode) -> Self {
        HttpOutgoingBody {
            handle: OutBodyHandle(Arc::new(OutBodyShared {
                state: Mutex::new(OutState {
                    bytes: Vec::new(),
                    phase: OutPhase::Open,
                }),
                changed: Condvar::new(),
                limit,
                too_large,
            })),
            stream_taken: false,
        }
    }

    pub(super) fn handle(&self) -> OutBodyHandle {
        self.handle.clone()
    }

    /// `write` can be called once.
    pub(super) fn take_stream(&mut self) -> bool {
        !std::mem::replace(&mut self.stream_taken, true)
    }
}

impl Drop for HttpOutgoingBody {
    fn drop(&mut self) {
        self.handle.fail(ErrorCode::InternalError(Some(
            "the body was dropped before finish".into(),
        )));
    }
}

/// How many bytes of a body the reader thread buffers ahead of the guest.
const BODY_BUFFER: usize = 1 << 20;

#[derive(Default)]
struct BodyBuf {
    bytes: VecDeque<u8>,
    eof: bool,
    /// Why the body ended early; taken when reported.
    error: Option<(ErrorKind, String)>,
}

struct BodyShared {
    state: Mutex<BodyBuf>,
    changed: Condvar,
    /// The guest let go of the body: the thread ends between reads.
    stop: AtomicBool,
}

/// What a read of a body found.
pub(super) enum BodyRead {
    /// Up to the length asked for, empty while nothing has arrived.
    Data(Vec<u8>),
    /// The body ended.
    Closed,
    /// The body failed (reported once; it is closed afterwards).
    Failed(ErrorKind, String),
}

/// An `incoming-body`: a thread reads the response's body into a buffer.
pub(super) struct HttpIncomingBody {
    shared: Arc<BodyShared>,
    stream_taken: bool,
}

impl HttpIncomingBody {
    pub(super) fn spawn(mut reader: Box<dyn Read + Send>) -> Self {
        let shared = Arc::new(BodyShared {
            state: Mutex::new(BodyBuf::default()),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let body = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                let mut state = lock(&body.state);
                while state.bytes.len() >= BODY_BUFFER && !body.stop.load(Ordering::SeqCst) {
                    state = body
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                drop(state);
                if body.stop.load(Ordering::SeqCst) {
                    return;
                }
                let read = reader.read(&mut chunk);
                if body.stop.load(Ordering::SeqCst) {
                    return;
                }
                let mut state = lock(&body.state);
                match read {
                    Ok(0) => state.eof = true,
                    Ok(n) => state.bytes.extend(chunk.iter().take(n)),
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        state.error = Some((e.kind(), e.to_string()));
                        state.eof = true;
                    }
                }
                let done = state.eof;
                drop(state);
                body.changed.notify_all();
                if done {
                    return;
                }
            }
        });
        HttpIncomingBody {
            shared,
            stream_taken: false,
        }
    }

    /// `stream` can be called once.
    pub(super) fn take_stream(&mut self) -> bool {
        !std::mem::replace(&mut self.stream_taken, true)
    }

    pub(super) fn shared(&self) -> BodyHandle {
        BodyHandle(Arc::clone(&self.shared))
    }

    /// Ends the reader thread (it notices between reads).
    fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // taking the lock orders the flag before the thread's next wait
        drop(lock(&self.shared.state));
        self.shared.changed.notify_all();
    }
}

/// Dropping a body (also with the host, for a guest that exits holding it) ends its thread.
impl Drop for HttpIncomingBody {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A reference to the buffer of a body, for reading it without borrowing the host.
pub(super) struct BodyHandle(Arc<BodyShared>);

impl BodyHandle {
    /// Whether a read would not wait: bytes are buffered or the body ended.
    pub(super) fn readable(&self) -> bool {
        let state = lock(&self.0.state);
        !state.bytes.is_empty() || state.eof
    }

    pub(super) fn read(&self, len: usize) -> BodyRead {
        let mut state = lock(&self.0.state);
        if state.bytes.is_empty() && state.eof {
            return match state.error.take() {
                Some((kind, why)) => BodyRead::Failed(kind, why),
                None => BodyRead::Closed,
            };
        }
        let n = len.min(state.bytes.len());
        let data = state.bytes.drain(..n).collect();
        drop(state);
        self.0.changed.notify_all(); // room for the thread
        BodyRead::Data(data)
    }
}

/// The `error-code` of a body that failed with `kind`.
pub(super) fn body_error_code(kind: ErrorKind, why: &str) -> ErrorCode {
    match kind {
        ErrorKind::TimedOut => ErrorCode::ConnectionReadTimeout,
        ErrorKind::UnexpectedEof => ErrorCode::HTTPResponseIncomplete,
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
            ErrorCode::ConnectionTerminated
        }
        _ => ErrorCode::InternalError(Some(why.to_string())),
    }
}
