//! Regression test against the official WASI 0.2.0 WIT (`tests/wasi_p2_wit/`, see its
//! README for the source and version).
//!
//! The library's `Host` trait (`wasm2rs-wasi/src/generated.rs`, kept fresh by
//! `wasi_generated.rs`) declares every method of `world all { include wasi:cli/imports@0.2.0;
//! import wasi:http/types@0.2.0; import wasi:http/outgoing-handler@0.2.0; }`, each with a
//! trapping default. The signatures cannot drift from the trait (the host is compiled
//! against it); what this test pins is which methods `WasiHost` overrides: every method
//! except those of `wasi:http`, which keep the default trap until the export side exists. So
//! a method that is neither implemented nor on that list fails the test, and so does
//! implementing an `http` one without updating the list here.

use std::collections::BTreeSet;

use crate::wasi_generated::generate;

const IMPL_HOST: &str = include_str!("../../wasm2rs-wasi/src/host/impl_host.rs");

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
    let implemented_http: Vec<_> = http.difference(&trapping).collect();
    assert!(
        implemented_http.is_empty(),
        "wasi:http methods are now implemented (update this test): {implemented_http:#?}"
    );
}
