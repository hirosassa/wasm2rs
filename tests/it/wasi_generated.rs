//! `wasm2rs-wasi/src/generated.rs` is the checked-in output of the generator for the official
//! WASI 0.2.0 WIT (`tests/wasi_p2_wit/`) plus its overlay of later additions
//! (`tests/wasi_p2_wit/overlay/`); this test regenerates it and fails on any
//! difference. After a generator change, refresh the file with
//! `WASM2RS_BLESS=1 cargo test --test it wasi_generated`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use wasm2rs::{WitNaming, WitOptions, wit_host_trait_with_options};

const GENERATED: &str = "wasm2rs-wasi/src/generated.rs";

/// `package`'s directory under `tests/wasi_p2_wit` with the files of the same name under
/// `tests/wasi_p2_wit/overlay` laid over it (copied to a scratch directory, since
/// `Resolve::push_dir` reads a directory).
fn package_dir(root: &Path, package: &str) -> PathBuf {
    let overlay = root.join("overlay").join(package);
    if !overlay.is_dir() {
        return root.join(package);
    }
    // unique per call: tests run in parallel and each builds its own copy
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let merged = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "wasi_p2_wit-{package}-{}-{call}",
        std::process::id()
    ));
    std::fs::create_dir_all(&merged).unwrap();
    for dir in [root.join(package), overlay] {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "wit") {
                std::fs::copy(&path, merged.join(path.file_name().unwrap())).unwrap();
            }
        }
    }
    merged
}

/// The component of every WASI 0.2.0 interface and overlay addition (`world all` of
/// `tests/wasi_p2_wit`).
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
        resolve.push_dir(package_dir(&root, package)).unwrap();
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
