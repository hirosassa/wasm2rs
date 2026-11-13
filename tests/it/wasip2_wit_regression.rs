//! Regression test against the official WASI 0.2.0 WIT (`tests/wasi_p2_wit/`, see its
//! README for the source and version).
//!
//! The library's `Host` trait (`wasm2rs-wasi/src/generated.rs`, kept fresh by
//! `wasi_generated.rs`) declares every method of `world all { include wasi:cli/imports@0.2.0;
//! import wasi:http/types@0.2.0; import wasi:http/outgoing-handler@0.2.0; }`, each with a
//! trapping default. The signatures cannot drift from the trait (the host is compiled
//! against it); what this test pins is that `WasiHost` overrides every method, `wasi:http`
//! included (see `docs/wasi-http-design.md`): a method left at the trapping default fails the
//! test.

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
fn the_trait_declares_every_wasi_0_2_0_method_and_the_overlay() {
    // 199 methods in the 29 interfaces of the official 0.2.0 WIT, plus `exit-with-code` of
    // `tests/wasi_p2_wit/overlay/`
    assert_eq!(methods(&generate()).len(), 200);
}

#[test]
fn the_host_overrides_every_method() {
    let declared = methods(&generate());
    let implemented = methods(IMPL_HOST);
    let unknown: Vec<_> = implemented.difference(&declared).collect();
    assert!(
        unknown.is_empty(),
        "WasiHost implements methods the trait does not declare: {unknown:#?}"
    );
    let missing: Vec<_> = declared.difference(&implemented).collect();
    assert!(
        missing.is_empty(),
        "methods left at the trapping default (implement them): {missing:#?}"
    );
}
