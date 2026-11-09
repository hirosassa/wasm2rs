//! The state and validation behind the `wasi:http/types` resources `fields`, `request-options`
//! and `outgoing-request`.

use crate::wasi_http_types::{HeaderError, Method, Scheme};

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

/// An `outgoing-request` resource. Its body is not implemented yet (H3).
#[derive(Debug, Clone)]
pub(super) struct HttpOutgoingRequest {
    pub(super) method: Method,
    pub(super) path_with_query: Option<String>,
    pub(super) scheme: Option<Scheme>,
    pub(super) authority: Option<String>,
    pub(super) headers: HttpFields,
}

impl HttpOutgoingRequest {
    pub(super) fn new(headers: HttpFields) -> Self {
        HttpOutgoingRequest {
            method: Method::Get,
            path_with_query: None,
            scheme: None,
            authority: None,
            headers: headers.freeze(),
        }
    }
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

/// Visible ASCII without the delimiters that end an authority.
pub(super) fn valid_authority(authority: &str) -> bool {
    !authority.is_empty()
        && authority
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !matches!(b, b'/' | b'?' | b'#'))
}
