//! Regression test against the official WASI 0.2.0 WIT (`tests/wasi_p2_wit/`, see its
//! README for the source and version).
//!
//! A component importing every interface of
//! `world all { include wasi:cli/imports@0.2.0; import wasi:http/types@0.2.0;
//! import wasi:http/outgoing-handler@0.2.0; }` is built with `wit-component`, and
//! `wit_host_trait` generates the complete `Host` trait for it. Each method of that trait
//! is compared, signature types included, with the `Host` impl of
//! `examples/wasip2_host/typed.rs`, after the host's `FsErrorCode`/`NetErrorCode` aliases are
//! replaced by the generated spelling of those enums (the `type_aliases` rule of `filter.rs`).
//!
//! `tests/wasi_p2_wit/allowlist.txt` records every known difference, one method per line:
//! `missing <fn>` (declared by the WIT, not implemented), `extra <fn>` (implemented, not in
//! 0.2.0) or `mismatch <fn>` (both, but the signatures differ). A difference that is not
//! listed fails the test, and so does a listed line that no longer describes a difference,
//! so implementing a method means deleting its line.

#![allow(
    dead_code,
    reason = "filter.rs is included whole; this test only uses `type_aliases`"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use wasm2rs::wit_host_trait;

include!("../../examples/wasip2_host/filter.rs");

const TYPED_HOST: &str = include_str!("../../examples/wasip2_host/typed.rs");
const ALLOWLIST: &str = include_str!("../wasi_p2_wit/allowlist.txt");

/// The packages in dependency order: each only imports the ones before it.
const PACKAGES: [&str; 7] = [
    "io",
    "clocks",
    "random",
    "filesystem",
    "sockets",
    "cli",
    "http",
];

fn component_of_all_world() -> Vec<u8> {
    use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
    use wit_parser::{ManglingAndAbi, Resolve};
    let mut resolve = Resolve::default();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wasi_p2_wit");
    for package in PACKAGES {
        resolve.push_dir(root.join(package)).unwrap();
    }
    let package = resolve
        .push_str(
            "all.wit",
            "package audit:all;
             world all {
               include wasi:cli/imports@0.2.0;
               import wasi:http/types@0.2.0;
               import wasi:http/outgoing-handler@0.2.0;
             }",
        )
        .unwrap();
    let world = resolve.select_world(&[package], Some("all")).unwrap();
    let mut module = dummy_module(&resolve, world, ManglingAndAbi::Standard32);
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false).unwrap();
    ComponentEncoder::default()
        .module(&module)
        .unwrap()
        .validate(true)
        .encode()
        .unwrap()
}

/// `fn wasi_*(..) -> ..` of every method in `source` (up to its `;` or `{`), by name, with
/// all whitespace, trailing commas and unused-parameter underscores removed so rustfmt's wrapping does not matter.
fn signatures(source: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    let mut rest = source;
    while let Some(at) = rest.find("fn wasi_") {
        let tail = &rest[at + "fn ".len()..];
        let end = tail.find([';', '{']).unwrap();
        let signature: String = tail[..end].split_whitespace().collect();
        // a parameter the host does not use is spelled `_name`; a type never starts with `_`
        let signature = signature
            .replace(",)", ")")
            .replace("(_", "(")
            .replace(",_", ",");
        let name = signature.split('(').next().unwrap().to_string();
        found.insert(name, signature);
        rest = &tail[end..];
    }
    found
}

/// The signatures of the generated `Host` trait.
fn generated_methods(generated: &str) -> BTreeMap<String, String> {
    let trait_start = generated.find("pub trait Host").unwrap();
    signatures(&generated[trait_start..])
}

/// The signatures of `impl Host for ..` in the typed host, with its `type_aliases` names
/// replaced by the generated enums they stand for.
fn typed_methods(generated: &str) -> BTreeMap<String, String> {
    let impl_start = TYPED_HOST.find("impl Host for ").unwrap();
    let mut methods = signatures(&TYPED_HOST[impl_start..]);
    for line in type_aliases(generated).lines() {
        let (alias, name) = line
            .strip_prefix("type ")
            .and_then(|rest| rest.strip_suffix(';'))
            .and_then(|rest| rest.split_once(" = "))
            .unwrap();
        for signature in methods.values_mut() {
            *signature = replace_word(signature, alias, name);
        }
    }
    methods
}

/// `text` with every whole-identifier occurrence of `word` replaced.
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(word) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + word.len()..].chars().next();
        let is_ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(&rest[..at]);
        out.push_str(if is_ident(before) || is_ident(after) {
            word
        } else {
            with
        });
        rest = &rest[at + word.len()..];
    }
    out.push_str(rest);
    out
}

/// The differences between the generated trait and the typed host, as allowlist lines.
fn differences(
    generated: &BTreeMap<String, String>,
    typed: &BTreeMap<String, String>,
) -> BTreeSet<String> {
    let mut diffs = BTreeSet::new();
    for (name, signature) in generated {
        match typed.get(name) {
            None => diffs.insert(format!("missing {name}")),
            Some(host) if host != signature => diffs.insert(format!("mismatch {name}")),
            Some(_) => false,
        };
    }
    for name in typed.keys().filter(|name| !generated.contains_key(*name)) {
        diffs.insert(format!("extra {name}"));
    }
    diffs
}

fn allowlist() -> BTreeSet<String> {
    ALLOWLIST
        .lines()
        .map(|line| line.split('#').next().unwrap().trim())
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_all_world_declares_every_wasi_0_2_0_interface() {
    let generated = wit_host_trait(&component_of_all_world()).unwrap();
    let methods = generated_methods(&generated);
    // 199 methods in the 29 interfaces of the official 0.2.0 WIT
    assert_eq!(methods.len(), 199, "the generated Host trait is complete");
}

#[test]
fn typed_host_matches_the_official_wit_up_to_the_allowlist() {
    let generated = wit_host_trait(&component_of_all_world()).unwrap();
    let actual = differences(&generated_methods(&generated), &typed_methods(&generated));
    let allowed = allowlist();
    let unlisted: Vec<_> = actual.difference(&allowed).collect();
    let stale: Vec<_> = allowed.difference(&actual).collect();
    assert!(
        unlisted.is_empty(),
        "differences from the official WIT that are not in tests/wasi_p2_wit/allowlist.txt \
         (implement them or list them): {unlisted:#?}"
    );
    assert!(
        stale.is_empty(),
        "allowlist lines that no longer describe a difference (delete them): {stale:#?}"
    );
}

#[test]
fn replacing_an_alias_leaves_longer_identifiers_alone() {
    assert_eq!(
        replace_word(
            "Result<(),FsErrorCode>|FsErrorCode2",
            "FsErrorCode",
            "ErrorCode2"
        ),
        "Result<(),ErrorCode2>|FsErrorCode2"
    );
}

#[test]
fn differences_classify_missing_extra_and_changed_signatures() {
    let methods = |items: &[(&str, &str)]| {
        items
            .iter()
            .map(|(n, s)| (n.to_string(), s.to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    let generated = methods(&[
        ("wasi_a", "wasi_a()"),
        ("wasi_b", "wasi_b(x:u8)"),
        ("wasi_c", "wasi_c()"),
    ]);
    let typed = methods(&[
        ("wasi_a", "wasi_a()"),
        ("wasi_b", "wasi_b(x:u16)"),
        ("wasi_d", "wasi_d()"),
    ]);
    let expected: BTreeSet<String> = ["mismatch wasi_b", "missing wasi_c", "extra wasi_d"]
        .map(String::from)
        .into();
    assert_eq!(differences(&generated, &typed), expected);
}
