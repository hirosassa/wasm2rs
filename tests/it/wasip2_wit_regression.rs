//! Regression test against the official WASI 0.2.0 WIT (`tests/wasi_p2_wit/`, see its
//! README for the source and version).
//!
//! The library's `Host` trait (`wasm2rs-wasi/src/generated.rs`, kept fresh by
//! `wasi_generated.rs`) declares every method of `world all { include wasi:cli/imports@0.2.0;
//! import wasi:http/types@0.2.0; import wasi:http/outgoing-handler@0.2.0; }`, each with a
//! trapping default. The signatures cannot drift from the trait (the host is compiled
//! against it); what this test pins is which methods `WasiHost` overrides: every method
//! except those of `wasi:http`, which are implemented phase by phase (see
//! `docs/wasi-http-design.md`) and keep the default trap until then. `IMPLEMENTED_HTTP` names
//! the ones done so far: a method that is neither implemented nor an `http` one fails the
//! test, and so does implementing an `http` one without updating the list here.

use std::collections::BTreeSet;

use crate::wasi_generated::generate;

const IMPL_HOST: &str = include_str!("../../wasm2rs-wasi/src/host/impl_host.rs");

/// The `wasi:http` methods implemented so far (H1: fields, request-options, the parts of
/// outgoing-request; H2: `handle` and the receive side; H3: the send side of bodies and
/// `future-trailers`).
const IMPLEMENTED_HTTP: [&str; 48] = [
    "wasi_http_types_constructor_fields",
    "wasi_http_types_static_fields_from_list",
    "wasi_http_types_method_fields_get",
    "wasi_http_types_method_fields_has",
    "wasi_http_types_method_fields_set",
    "wasi_http_types_method_fields_delete",
    "wasi_http_types_method_fields_append",
    "wasi_http_types_method_fields_entries",
    "wasi_http_types_method_fields_clone",
    "wasi_http_types_constructor_request_options",
    "wasi_http_types_method_request_options_connect_timeout",
    "wasi_http_types_method_request_options_set_connect_timeout",
    "wasi_http_types_method_request_options_first_byte_timeout",
    "wasi_http_types_method_request_options_set_first_byte_timeout",
    "wasi_http_types_method_request_options_between_bytes_timeout",
    "wasi_http_types_method_request_options_set_between_bytes_timeout",
    "wasi_http_types_constructor_outgoing_request",
    "wasi_http_types_method_outgoing_request_method",
    "wasi_http_types_method_outgoing_request_set_method",
    "wasi_http_types_method_outgoing_request_path_with_query",
    "wasi_http_types_method_outgoing_request_set_path_with_query",
    "wasi_http_types_method_outgoing_request_scheme",
    "wasi_http_types_method_outgoing_request_set_scheme",
    "wasi_http_types_method_outgoing_request_authority",
    "wasi_http_types_method_outgoing_request_set_authority",
    "wasi_http_types_method_outgoing_request_headers",
    "wasi_http_types_method_outgoing_request_body",
    "wasi_http_types_method_outgoing_body_write",
    "wasi_http_types_static_outgoing_body_finish",
    "wasi_http_types_method_future_trailers_subscribe",
    "wasi_http_types_method_future_trailers_get",
    "wasi_http_types_resource_drop_outgoing_body",
    "wasi_http_types_resource_drop_fields",
    "wasi_http_types_resource_drop_outgoing_request",
    "wasi_http_types_resource_drop_request_options",
    "wasi_http_outgoing_handler_handle",
    "wasi_http_types_method_future_incoming_response_subscribe",
    "wasi_http_types_method_future_incoming_response_get",
    "wasi_http_types_method_incoming_response_status",
    "wasi_http_types_method_incoming_response_headers",
    "wasi_http_types_method_incoming_response_consume",
    "wasi_http_types_method_incoming_body_stream",
    "wasi_http_types_static_incoming_body_finish",
    "wasi_http_types_http_error_code",
    "wasi_http_types_resource_drop_future_incoming_response",
    "wasi_http_types_resource_drop_incoming_response",
    "wasi_http_types_resource_drop_incoming_body",
    "wasi_http_types_resource_drop_future_trailers",
];

/// The names of the `fn wasi_*(&mut self ..` methods declared (or overridden) in `source`.
fn methods(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            let name = line.trim_start().strip_prefix("fn wasi_")?;
            let (name, _) = name.split_once('(')?;
            Some(format!("wasi_{name}"))
        })
        .collect()
}

#[test]
fn the_trait_declares_every_wasi_0_2_0_method() {
    // 199 methods in the 29 interfaces of the official 0.2.0 WIT
    assert_eq!(methods(&generate()).len(), 199);
}

#[test]
fn the_host_overrides_every_method_except_those_of_wasi_http() {
    let declared = methods(&generate());
    let implemented = methods(IMPL_HOST);
    let unknown: Vec<_> = implemented.difference(&declared).collect();
    assert!(
        unknown.is_empty(),
        "WasiHost implements methods the trait does not declare: {unknown:#?}"
    );
    let trapping: BTreeSet<_> = declared.difference(&implemented).cloned().collect();
    let http: BTreeSet<_> = declared
        .iter()
        .filter(|m| m.starts_with("wasi_http_"))
        .cloned()
        .collect();
    let missing: Vec<_> = trapping.difference(&http).collect();
    assert!(
        missing.is_empty(),
        "methods left at the trapping default (implement them): {missing:#?}"
    );
    let implemented_http: BTreeSet<_> = http.difference(&trapping).cloned().collect();
    let expected: BTreeSet<_> = IMPLEMENTED_HTTP.iter().map(|m| m.to_string()).collect();
    assert_eq!(
        implemented_http, expected,
        "the implemented wasi:http methods differ from IMPLEMENTED_HTTP (update this test)"
    );
}
