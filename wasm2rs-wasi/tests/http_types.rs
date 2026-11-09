//! `wasi:http/types` fields, request-options and outgoing-request (H1), driven through the
//! `Host` trait directly.

use wasm2rs_wasi::wasi_http_types::{HeaderError, Method, Scheme};
use wasm2rs_wasi::{
    Host, HttpClient, HttpClientError, HttpRequest, HttpResponse, WasiHost, WasiHostBuilder,
};

fn host() -> WasiHost {
    let builder: WasiHostBuilder = WasiHost::builder();
    #[cfg(not(feature = "os-random"))]
    let builder = builder.random(wasm2rs_wasi::InsecureRandom::new(0));
    builder.build().unwrap()
}

fn v(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[test]
fn names_are_case_insensitive_and_hold_several_values() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_method_fields_append(f, "Accept".into(), v("a"))
        .unwrap();
    h.wasi_http_types_method_fields_append(f, "accept".into(), v("b"))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_fields_get(f, "ACCEPT".into()),
        vec![v("a"), v("b")]
    );
    assert!(h.wasi_http_types_method_fields_has(f, "aCcEpT".into()));
    assert!(!h.wasi_http_types_method_fields_has(f, "x-other".into()));
    assert!(
        h.wasi_http_types_method_fields_get(f, "x-other".into())
            .is_empty()
    );
}

#[test]
fn set_replaces_append_adds_and_delete_removes_every_value() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_method_fields_append(f, "x".into(), v("1"))
        .unwrap();
    h.wasi_http_types_method_fields_append(f, "x".into(), v("2"))
        .unwrap();
    h.wasi_http_types_method_fields_set(f, "X".into(), vec![v("3")])
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_fields_get(f, "x".into()),
        vec![v("3")]
    );
    h.wasi_http_types_method_fields_append(f, "x".into(), v("4"))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_fields_get(f, "x".into()),
        vec![v("3"), v("4")]
    );
    h.wasi_http_types_method_fields_delete(f, "x".into())
        .unwrap();
    assert!(!h.wasi_http_types_method_fields_has(f, "x".into()));
    // deleting an absent name is fine; setting an empty list removes the name
    h.wasi_http_types_method_fields_delete(f, "x".into())
        .unwrap();
    h.wasi_http_types_method_fields_set(f, "y".into(), vec![v("1")])
        .unwrap();
    h.wasi_http_types_method_fields_set(f, "y".into(), vec![])
        .unwrap();
    assert!(!h.wasi_http_types_method_fields_has(f, "y".into()));
}

#[test]
fn entries_list_every_value_in_insertion_order() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_method_fields_append(f, "B".into(), v("1"))
        .unwrap();
    h.wasi_http_types_method_fields_append(f, "a".into(), v("2"))
        .unwrap();
    h.wasi_http_types_method_fields_append(f, "b".into(), v("3"))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_fields_entries(f),
        vec![
            ("B".into(), v("1")),
            ("B".into(), v("3")),
            ("a".into(), v("2"))
        ]
    );
}

#[test]
fn from_list_builds_fields_and_rejects_bad_entries() {
    let mut h = host();
    let f = h
        .wasi_http_types_static_fields_from_list(vec![
            ("a".into(), v("1")),
            ("A".into(), v("2")),
            ("b".into(), v("3")),
        ])
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_fields_get(f, "a".into()),
        vec![v("1"), v("2")]
    );
    assert_eq!(
        h.wasi_http_types_static_fields_from_list(vec![("host".into(), v("x"))]),
        Err(HeaderError::Forbidden)
    );
    assert_eq!(
        h.wasi_http_types_static_fields_from_list(vec![("bad name".into(), v("x"))]),
        Err(HeaderError::InvalidSyntax)
    );
}

#[test]
fn forbidden_headers_are_refused_by_set_append_and_delete() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "proxy-connection",
        "te",
        "transfer-encoding",
        "upgrade",
        "http2-settings",
        "Host",
    ] {
        assert_eq!(
            h.wasi_http_types_method_fields_append(f, name.into(), v("x")),
            Err(HeaderError::Forbidden),
            "{name}"
        );
        assert_eq!(
            h.wasi_http_types_method_fields_set(f, name.into(), vec![v("x")]),
            Err(HeaderError::Forbidden),
            "{name}"
        );
        assert_eq!(
            h.wasi_http_types_method_fields_delete(f, name.into()),
            Err(HeaderError::Forbidden),
            "{name}"
        );
    }
}

#[test]
fn invalid_names_and_values_are_invalid_syntax() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    for name in ["", "a b", "a:b", "a\n", "é"] {
        assert_eq!(
            h.wasi_http_types_method_fields_append(f, name.into(), v("x")),
            Err(HeaderError::InvalidSyntax),
            "{name:?}"
        );
    }
    for value in [v("a\r\nb"), v("a\0"), v("a\x7f")] {
        assert_eq!(
            h.wasi_http_types_method_fields_append(f, "x".into(), value.clone()),
            Err(HeaderError::InvalidSyntax),
            "{value:?}"
        );
    }
    // a failed set leaves the previous value alone
    h.wasi_http_types_method_fields_set(f, "x".into(), vec![v("ok")])
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_fields_set(f, "x".into(), vec![v("new"), v("bad\n")]),
        Err(HeaderError::InvalidSyntax)
    );
    assert_eq!(
        h.wasi_http_types_method_fields_get(f, "x".into()),
        vec![v("ok")]
    );
    // tabs and obs-text bytes are fine in a value
    h.wasi_http_types_method_fields_append(f, "x".into(), vec![b'a', b'\t', 0xe9])
        .unwrap();
}

#[test]
fn clone_is_an_independent_mutable_copy() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_method_fields_append(f, "x".into(), v("1"))
        .unwrap();
    let c = h.wasi_http_types_method_fields_clone(f);
    h.wasi_http_types_method_fields_append(c, "x".into(), v("2"))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_fields_get(f, "x".into()),
        vec![v("1")]
    );
    assert_eq!(
        h.wasi_http_types_method_fields_get(c, "x".into()),
        vec![v("1"), v("2")]
    );
    h.wasi_http_types_resource_drop_fields(f);
    assert_eq!(h.wasi_http_types_method_fields_get(c, "x".into()).len(), 2);
}

#[test]
fn request_options_round_trip_timeouts() {
    let mut h = host();
    let o = h.wasi_http_types_constructor_request_options();
    assert_eq!(
        h.wasi_http_types_method_request_options_connect_timeout(o),
        None
    );
    assert_eq!(
        h.wasi_http_types_method_request_options_first_byte_timeout(o),
        None
    );
    assert_eq!(
        h.wasi_http_types_method_request_options_between_bytes_timeout(o),
        None
    );
    h.wasi_http_types_method_request_options_set_connect_timeout(o, Some(1))
        .unwrap();
    h.wasi_http_types_method_request_options_set_first_byte_timeout(o, Some(2))
        .unwrap();
    h.wasi_http_types_method_request_options_set_between_bytes_timeout(o, Some(3))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_request_options_connect_timeout(o),
        Some(1)
    );
    assert_eq!(
        h.wasi_http_types_method_request_options_first_byte_timeout(o),
        Some(2)
    );
    assert_eq!(
        h.wasi_http_types_method_request_options_between_bytes_timeout(o),
        Some(3)
    );
    h.wasi_http_types_method_request_options_set_connect_timeout(o, None)
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_request_options_connect_timeout(o),
        None
    );
    h.wasi_http_types_resource_drop_request_options(o);
}

#[test]
fn an_outgoing_request_starts_with_get_and_no_target() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(f);
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_method(r),
        Method::Get
    );
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_path_with_query(r),
        None
    );
    assert_eq!(h.wasi_http_types_method_outgoing_request_scheme(r), None);
    assert_eq!(h.wasi_http_types_method_outgoing_request_authority(r), None);
}

#[test]
fn outgoing_request_parts_round_trip() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(f);
    h.wasi_http_types_method_outgoing_request_set_method(r, Method::Other("PURGE".into()))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_method(r),
        Method::Other("PURGE".into())
    );
    h.wasi_http_types_method_outgoing_request_set_path_with_query(r, Some("/a?b=c".into()))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_path_with_query(r),
        Some("/a?b=c".into())
    );
    h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(Scheme::HTTPS))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_scheme(r),
        Some(Scheme::HTTPS)
    );
    h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(Scheme::Other("ftp".into())))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_scheme(r),
        Some(Scheme::Other("ftp".into()))
    );
    h.wasi_http_types_method_outgoing_request_set_authority(r, Some("example.com:8080".into()))
        .unwrap();
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_authority(r),
        Some("example.com:8080".into())
    );
    h.wasi_http_types_method_outgoing_request_set_authority(r, None)
        .unwrap();
    assert_eq!(h.wasi_http_types_method_outgoing_request_authority(r), None);
}

#[test]
fn outgoing_request_rejects_invalid_parts_and_keeps_the_old_value() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    let r = h.wasi_http_types_constructor_outgoing_request(f);
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_set_method(r, Method::Other("".into())),
        Err(())
    );
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_set_method(r, Method::Other("a b".into())),
        Err(())
    );
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_method(r),
        Method::Get
    );
    for p in ["", "a b", "/a#frag", "/a\n"] {
        assert_eq!(
            h.wasi_http_types_method_outgoing_request_set_path_with_query(r, Some(p.into())),
            Err(()),
            "{p:?}"
        );
    }
    for s in ["", "1http", "ht tp", "ht:tp"] {
        assert_eq!(
            h.wasi_http_types_method_outgoing_request_set_scheme(r, Some(Scheme::Other(s.into()))),
            Err(()),
            "{s:?}"
        );
    }
    for a in ["", "a/b", "a b", "a?b", "a#b"] {
        assert_eq!(
            h.wasi_http_types_method_outgoing_request_set_authority(r, Some(a.into())),
            Err(()),
            "{a:?}"
        );
    }
    assert_eq!(
        h.wasi_http_types_method_outgoing_request_path_with_query(r),
        None
    );
    assert_eq!(h.wasi_http_types_method_outgoing_request_scheme(r), None);
    assert_eq!(h.wasi_http_types_method_outgoing_request_authority(r), None);
}

#[test]
fn the_headers_of_an_outgoing_request_are_immutable() {
    let mut h = host();
    let f = h.wasi_http_types_constructor_fields();
    h.wasi_http_types_method_fields_append(f, "x".into(), v("1"))
        .unwrap();
    let r = h.wasi_http_types_constructor_outgoing_request(f);
    let hs = h.wasi_http_types_method_outgoing_request_headers(r);
    assert_eq!(
        h.wasi_http_types_method_fields_get(hs, "x".into()),
        vec![v("1")]
    );
    assert_eq!(
        h.wasi_http_types_method_fields_append(hs, "x".into(), v("2")),
        Err(HeaderError::Immutable)
    );
    assert_eq!(
        h.wasi_http_types_method_fields_set(hs, "x".into(), vec![]),
        Err(HeaderError::Immutable)
    );
    assert_eq!(
        h.wasi_http_types_method_fields_delete(hs, "x".into()),
        Err(HeaderError::Immutable)
    );
    // a clone of immutable fields is mutable again, and dropping the view keeps the request's
    let c = h.wasi_http_types_method_fields_clone(hs);
    h.wasi_http_types_method_fields_append(c, "x".into(), v("2"))
        .unwrap();
    h.wasi_http_types_resource_drop_fields(hs);
    let again = h.wasi_http_types_method_outgoing_request_headers(r);
    assert_eq!(
        h.wasi_http_types_method_fields_get(again, "x".into()),
        vec![v("1")]
    );
    h.wasi_http_types_resource_drop_outgoing_request(r);
}

struct Refusing;

impl HttpClient for Refusing {
    fn send(&mut self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        Err(HttpClientError::new("refused"))
    }
}

#[test]
fn the_builder_holds_an_http_client() {
    let builder = WasiHost::builder().http_client(Refusing);
    #[cfg(not(feature = "os-random"))]
    let builder = builder.random(wasm2rs_wasi::InsecureRandom::new(0));
    builder.build().unwrap();
}
