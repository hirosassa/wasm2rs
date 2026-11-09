//! `wasm2rs-wasi/src/generated.rs` is the checked-in output of the generator for the official
//! WASI 0.2.0 WIT (`tests/wasi_p2_wit/`); this test regenerates it and fails on any
//! difference. After a generator change, refresh the file with
//! `WASM2RS_BLESS=1 cargo test --test it wasi_generated`.

use std::path::Path;

use wasm2rs::{WitNaming, WitOptions, wit_host_trait_with_options};

const GENERATED: &str = "wasm2rs-wasi/src/generated.rs";

/// The component of every WASI 0.2.0 interface (`world all` of `tests/wasi_p2_wit`).
pub fn wasi_p2_component() -> Vec<u8> {
    use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
    use wit_parser::{ManglingAndAbi, Resolve};
    let mut resolve = Resolve::default();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wasi_p2_wit");
    for package in [
        "io",
        "clocks",
        "random",
        "filesystem",
        "sockets",
        "cli",
        "http",
    ] {
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

/// The library's trait and types: qualified names, every method trapping by default.
pub fn generate() -> String {
    let options = WitOptions {
        naming: WitNaming::Qualified,
        default_trap: true,
        ..WitOptions::default()
    };
    wit_host_trait_with_options(&wasi_p2_component(), &options).unwrap()
}

#[test]
fn the_checked_in_trait_is_what_the_generator_produces() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(GENERATED);
    let fresh = generate();
    if std::env::var_os("WASM2RS_BLESS").is_some() {
        std::fs::write(&path, &fresh).unwrap();
        return;
    }
    let checked_in = std::fs::read_to_string(&path).unwrap();
    assert!(
        checked_in == fresh,
        "{GENERATED} is stale; run `WASM2RS_BLESS=1 cargo test --test it wasi_generated`"
    );
}
