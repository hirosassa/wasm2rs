//! `wasi:http/incoming-handler#handle` on the typed host (H5): a component of a `proxy`-like
//! world (the official WIT, `incoming-handler` exported, `types` imported) is called through
//! the glue's `Exports` with a request the embedder minted and a `response-outparam`; the
//! response the guest sets comes back through `take_response`.

use crate::wasip2_typed_host::{assert_succeeded, run_exporting_program};

/// The parts of `wasi:io` that `types` and the guests use (as the official WIT declares them).
const IO: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface streams {
  use error.{error};
  variant stream-error {
    last-operation-failed(error),
    closed,
  }
  resource input-stream {
    blocking-read: func(len: u64) -> result<list<u8>, stream-error>;
  }
  resource output-stream {
    write: func(contents: list<u8>) -> result<_, stream-error>;
  }
}
"#;

/// The incoming side of `wasi:http` with the official signatures: the glue is generated from
/// the component and must match the library's `Host`, but the core module of a test imports
/// only what it calls, so the interfaces declare no more than the guests use.
const HTTP: &str = r#"
package wasi:http@0.2.0;
interface types {
  use wasi:io/streams@0.2.0.{input-stream, output-stream};
  variant method { get, head, post, put, delete, connect, options, trace, patch, other(string) }
  variant scheme { HTTP, HTTPS, other(string) }
  variant error-code {
    DNS-timeout,
    DNS-error(DNS-error-payload),
    destination-not-found,
    destination-unavailable,
    destination-IP-prohibited,
    destination-IP-unroutable,
    connection-refused,
    connection-terminated,
    connection-timeout,
    connection-read-timeout,
    connection-write-timeout,
    connection-limit-reached,
    TLS-protocol-error,
    TLS-certificate-error,
    TLS-alert-received(TLS-alert-received-payload),
    HTTP-request-denied,
    HTTP-request-length-required,
    HTTP-request-body-size(option<u64>),
    HTTP-request-method-invalid,
    HTTP-request-URI-invalid,
    HTTP-request-URI-too-long,
    HTTP-request-header-section-size(option<u32>),
    HTTP-request-header-size(option<field-size-payload>),
    HTTP-request-trailer-section-size(option<u32>),
    HTTP-request-trailer-size(field-size-payload),
    HTTP-response-incomplete,
    HTTP-response-header-section-size(option<u32>),
    HTTP-response-header-size(field-size-payload),
    HTTP-response-body-size(option<u64>),
    HTTP-response-trailer-section-size(option<u32>),
    HTTP-response-trailer-size(field-size-payload),
    HTTP-response-transfer-coding(option<string>),
    HTTP-response-content-coding(option<string>),
    HTTP-response-timeout,
    HTTP-upgrade-failed,
    HTTP-protocol-error,
    loop-detected,
    configuration-error,
    internal-error(option<string>)
  }
  record DNS-error-payload { rcode: option<string>, info-code: option<u16> }
  record TLS-alert-received-payload { alert-id: option<u8>, alert-message: option<string> }
  record field-size-payload { field-name: option<string>, field-size: option<u32> }
  variant header-error { invalid-syntax, forbidden, immutable }
  type field-key = string;
  type field-value = list<u8>;
  resource fields {
    constructor();
    get: func(name: field-key) -> list<field-value>;
    append: func(name: field-key, value: field-value) -> result<_, header-error>;
  }
  type headers = fields;
  type trailers = fields;
  resource incoming-request {
    method: func() -> method;
    path-with-query: func() -> option<string>;
    scheme: func() -> option<scheme>;
    authority: func() -> option<string>;
    headers: func() -> headers;
    consume: func() -> result<incoming-body>;
  }
  resource response-outparam {
    set: static func(param: response-outparam, response: result<outgoing-response, error-code>);
  }
  type status-code = u16;
  resource incoming-body {
    %stream: func() -> result<input-stream>;
  }
  resource outgoing-response {
    constructor(headers: headers);
    set-status-code: func(status-code: status-code) -> result;
    body: func() -> result<outgoing-body>;
  }
  resource outgoing-body {
    write: func() -> result<output-stream>;
    finish: static func(this: outgoing-body, trailers: option<trailers>) -> result<_, error-code>;
  }
}
interface incoming-handler {
  use types.{incoming-request, response-outparam};
  handle: func(request: incoming-request, response-out: response-outparam);
}
"#;

const WORLD: &str = r#"
package test:t;
world p {
  import wasi:http/types@0.2.0;
  export wasi:http/incoming-handler@0.2.0;
}
"#;

/// The component of `world p` around the core module `core`: an exported function's core
/// name comes from the module, so the dummy module of the imports-only tests cannot do.
fn proxy_component(core: &str) -> Vec<u8> {
    use wit_component::{ComponentEncoder, StringEncoding, embed_component_metadata};
    use wit_parser::Resolve;
    let mut resolve = Resolve::default();
    resolve.push_str("io.wit", IO).unwrap();
    resolve.push_str("http.wit", HTTP).unwrap();
    let package = resolve.push_str("test.wit", WORLD).unwrap();
    let world = resolve.select_world(&[package], Some("p")).unwrap();
    let mut module = wat::parse_str(core).unwrap();
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

/// Imports and a bump allocator shared by the guests.
const PRELUDE: &str = r#"
  (import "wasi:http/types@0.2.0" "[resource-drop]incoming-request" (func $drop_req (param i32)))
  (import "wasi:http/types@0.2.0" "[resource-drop]response-outparam" (func $drop_out (param i32)))
  (import "wasi:http/types@0.2.0" "[resource-drop]fields" (func $drop_fields (param i32)))
  (import "wasi:http/types@0.2.0" "[resource-drop]incoming-body" (func $drop_inbody (param i32)))
  (import "wasi:http/types@0.2.0" "[resource-drop]outgoing-body" (func $drop_outbody (param i32)))
  (import "wasi:http/types@0.2.0" "[resource-drop]outgoing-response" (func $drop_resp (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]input-stream" (func $drop_in (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func $drop_ostream (param i32)))
  (import "wasi:http/types@0.2.0" "[constructor]fields" (func $fields_new (result i32)))
  (import "wasi:http/types@0.2.0" "[method]fields.get" (func $fields_get (param i32 i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]fields.append" (func $append (param i32 i32 i32 i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]incoming-request.method" (func $method (param i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]incoming-request.path-with-query" (func $path (param i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]incoming-request.scheme" (func $scheme (param i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]incoming-request.authority" (func $authority (param i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]incoming-request.headers" (func $req_headers (param i32) (result i32)))
  (import "wasi:http/types@0.2.0" "[method]incoming-request.consume" (func $consume (param i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]incoming-body.stream" (func $in_stream (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]input-stream.blocking-read" (func $read (param i32 i64 i32)))
  (import "wasi:http/types@0.2.0" "[constructor]outgoing-response" (func $resp_new (param i32) (result i32)))
  (import "wasi:http/types@0.2.0" "[method]outgoing-response.set-status-code" (func $set_status (param i32 i32) (result i32)))
  (import "wasi:http/types@0.2.0" "[method]outgoing-response.body" (func $resp_body (param i32 i32)))
  (import "wasi:http/types@0.2.0" "[method]outgoing-body.write" (func $body_write (param i32 i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[static]outgoing-body.finish" (func $finish (param i32 i32 i32 i32)))
  (import "wasi:http/types@0.2.0" "[static]response-outparam.set"
    (func $set (param i32 i32 i32 i32 i64 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (global $bump (mut i32) (i32.const 4096))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $bump) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $bump (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 500) "x-path")
  (data (i32.const 510) "x-auth")
  (data (i32.const 520) "x-accept")
  (data (i32.const 530) "accept")
"#;

/// A guest module whose `handle` export has the body `handle`.
fn guest(handle: &str) -> String {
    format!(
        r#"(module
  {PRELUDE}
  (func (export "wasi:http/incoming-handler@0.2.0#handle") (param $req i32) (param $out i32)
    {handle}))"#
    )
}

/// The echo guest: the status is `200 + method case + 10 * scheme case`, the headers
/// `x-path`, `x-auth` and `x-accept` carry the request's path-with-query, authority and first
/// `accept`, and the body is the request's body.
fn echo_guest() -> String {
    guest(
        r#"(local $mtag i32) (local $stag i32) (local $rh i32) (local $h i32) (local $resp i32)
    (local $body i32) (local $ostream i32) (local $inbody i32) (local $istream i32)
    (call $method (local.get $req) (i32.const 100))
    (local.set $mtag (i32.load8_u (i32.const 100)))
    (call $scheme (local.get $req) (i32.const 112))
    (if (i32.load8_u (i32.const 112)) (then (local.set $stag (i32.load8_u (i32.const 116)))))
    (call $path (local.get $req) (i32.const 128))
    (call $authority (local.get $req) (i32.const 144))
    (local.set $rh (call $req_headers (local.get $req)))
    (call $fields_get (local.get $rh) (i32.const 530) (i32.const 6) (i32.const 160))
    (local.set $h (call $fields_new))
    (call $append (local.get $h) (i32.const 500) (i32.const 6)
      (i32.load (i32.const 132)) (i32.load (i32.const 136)) (i32.const 176))
    (call $append (local.get $h) (i32.const 510) (i32.const 6)
      (i32.load (i32.const 148)) (i32.load (i32.const 152)) (i32.const 176))
    (call $append (local.get $h) (i32.const 520) (i32.const 8)
      (i32.load (i32.load (i32.const 160)))
      (i32.load offset=4 (i32.load (i32.const 160))) (i32.const 176))
    (local.set $resp (call $resp_new (local.get $h)))
    (drop (call $set_status (local.get $resp)
      (i32.add (i32.const 200)
        (i32.add (local.get $mtag) (i32.mul (local.get $stag) (i32.const 10))))))
    (call $resp_body (local.get $resp) (i32.const 192))
    (local.set $body (i32.load (i32.const 196)))
    (call $body_write (local.get $body) (i32.const 208))
    (local.set $ostream (i32.load (i32.const 212)))
    (call $consume (local.get $req) (i32.const 224))
    (local.set $inbody (i32.load (i32.const 228)))
    (call $in_stream (local.get $inbody) (i32.const 240))
    (local.set $istream (i32.load (i32.const 244)))
    (call $read (local.get $istream) (i64.const 64) (i32.const 256))
    (if (i32.eqz (i32.load (i32.const 256)))
      (then (call $write (local.get $ostream)
        (i32.load (i32.const 260)) (i32.load (i32.const 264)) (i32.const 272))))
    (call $drop_in (local.get $istream))
    (call $drop_inbody (local.get $inbody))
    (call $drop_ostream (local.get $ostream))
    (call $finish (local.get $body) (i32.const 0) (i32.const 0) (i32.const 288))
    (call $drop_fields (local.get $rh))
    (call $drop_req (local.get $req))
    (call $set (local.get $out) (i32.const 0) (local.get $resp)
      (i32.const 0) (i64.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))"#,
    )
}

/// A guest that gives the request and the outparam back without answering.
fn silent_guest() -> String {
    guest(
        r#"(call $drop_req (local.get $req))
    (call $drop_out (local.get $out))"#,
    )
}

/// A guest that answers with `error-code::connection-refused` (case 6).
fn refusing_guest() -> String {
    guest(
        r#"(call $drop_req (local.get $req))
    (call $set (local.get $out) (i32.const 1) (i32.const 6)
      (i32.const 0) (i64.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0))"#,
    )
}

/// Runs `core`'s `handle` on the request of `MINT_POST`, then `checks` (Rust with `host!()`
/// and `out` in scope).
fn run_handle(core: &str, checks: &str) -> std::process::Output {
    let body = format!(
        "{MINT_POST}
    Exports::export_wasi_http_incoming_handler_0_2_0_handle(&mut inst, req, out);
{checks}"
    );
    run_exporting_program(
        &proxy_component(core),
        core,
        "WasiHost::new(Vec::new())",
        &body,
    )
}

/// The Rust that makes the request every test hands to `handle`.
const MINT_POST: &str = r#"
    macro_rules! host { () => { inst.imports.as_mut().unwrap().0 }; }
    use wasm2rs_wasi::wasi_http_types::{Method, Scheme};
    use wasm2rs_wasi::IncomingRequestParts;
    let req = host!().new_incoming_request(IncomingRequestParts {
        method: Method::Post,
        path_with_query: Some("/items?id=7".to_string()),
        scheme: Some(Scheme::HTTPS),
        authority: Some("example.com:8080".to_string()),
        headers: vec![("Accept".to_string(), b"text/plain".to_vec())],
        body: b"hello".to_vec(),
    });
    let out = host!().new_response_outparam();
"#;

#[test]
fn the_guest_sees_the_request_and_its_response_comes_back() {
    let checks = r#"
    let served = host!().take_response(out).unwrap().unwrap();
    // 200 + post (2) + https (1) * 10
    assert_eq!(served.status, 212);
    let header = |name: &str| {
        served.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    };
    assert_eq!(header("x-path"), Some(b"/items?id=7".to_vec()));
    assert_eq!(header("x-auth"), Some(b"example.com:8080".to_vec()));
    assert_eq!(header("x-accept"), Some(b"text/plain".to_vec()));
    assert_eq!(served.body, b"hello".to_vec());"#;
    assert_succeeded(&run_handle(&echo_guest(), checks));
}

#[test]
fn a_guest_that_does_not_answer_leaves_no_response() {
    let checks = "assert!(host!().take_response(out).is_none());";
    assert_succeeded(&run_handle(&silent_guest(), checks));
}

#[test]
fn an_error_the_guest_sets_is_returned() {
    let checks = r#"
    let answer = host!().take_response(out).unwrap();
    assert_eq!(
        answer.unwrap_err(),
        wasm2rs_wasi::wasi_http_types::ErrorCode::ConnectionRefused
    );"#;
    assert_succeeded(&run_handle(&refusing_guest(), checks));
}
