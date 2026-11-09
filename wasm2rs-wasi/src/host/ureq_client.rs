//! [`UreqClient`]: an [`HttpClient`] over `ureq` (feature `http-client`). No `ureq` type is part
//! of the public API.

use crate::{HttpClient, HttpClientError, HttpRequest, HttpResponse};
use std::time::Duration;

/// Executes `wasi:http` requests with `ureq`.
///
/// Redirects are not followed: a 3xx is the guest's to see, as with `wasmtime-wasi-http`. 4xx and
/// 5xx are responses, not errors; only a request that got no response (connection, name lookup,
/// timeout, malformed message) is an [`HttpClientError`]. The body is neither decompressed nor
/// otherwise rewritten. `https` works through `ureq`'s default TLS (rustls with the bundled
/// webpki roots).
///
/// `request-options`: `connect-timeout` bounds the connection and `first-byte-timeout` the wait
/// for the response head. `ureq` has no timeout per read, so `between-bytes-timeout` is applied
/// as a limit on the whole body (from the head to its end), which is stricter than the WIT's.
#[derive(Debug, Clone)]
pub struct UreqClient {
    agent: ureq::Agent,
}

impl UreqClient {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .build();
        UreqClient {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::new()
    }
}

fn nanos(timeout: Option<u64>) -> Option<Duration> {
    timeout.map(Duration::from_nanos)
}

fn client_error(error: impl std::fmt::Display) -> HttpClientError {
    HttpClientError::new(error.to_string())
}

impl HttpClient for UreqClient {
    fn send(&mut self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        let mut builder = ureq::http::Request::builder()
            .method(request.method.as_str())
            .uri(request.url.as_str());
        for (name, value) in &request.headers {
            let value = ureq::http::HeaderValue::from_bytes(value).map_err(client_error)?;
            builder = builder.header(name.as_str(), value);
        }
        let http_request = builder.body(request.body).map_err(client_error)?;
        let http_request = self
            .agent
            .configure_request(http_request)
            .timeout_connect(nanos(request.connect_timeout))
            .timeout_recv_response(nanos(request.first_byte_timeout))
            .timeout_recv_body(nanos(request.between_bytes_timeout))
            .build();
        let response = self.agent.run(http_request).map_err(client_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec()))
            .collect();
        Ok(HttpResponse {
            status,
            headers,
            body: Box::new(response.into_body().into_reader()),
        })
    }
}
