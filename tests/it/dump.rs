//! `wasm2rs::dump` and the `--dump` flag: a summary of a module that does not
//! generate code, and does not need the module to be transpilable.

use std::process::Command;

fn summary(wat: &str) -> String {
    let wasm = wat::parse_str(wat).expect("valid wat");
    wasm2rs::dump(&wasm).expect("dump ok").to_string()
}

#[test]
fn counts_functions_split_into_imported_and_defined() {
    let text = summary(
        r#"(module
            (import "env" "log" (func (param i32)))
            (func (export "a") (result i32) i32.const 1)
            (func (result i32) i32.const 2))"#,
    );
    assert!(
        text.contains("functions: 3 (1 imported, 2 defined)"),
        "{text}"
    );
}

#[test]
fn classifies_each_import_by_how_it_would_be_resolved() {
    let text = summary(
        r#"(module
            (import "wasi_snapshot_preview1" "fd_write"
              (func (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_write"
              (func (param i32) (result i32)))
            (import "env" "log" (func (param i32)))
            (memory 1))"#,
    );
    assert!(
        text.contains(
            "wasi_snapshot_preview1.fd_write (i32, i32, i32, i32) -> (i32) [native WASI]"
        ),
        "{text}"
    );
    // A mistyped WASI import is left to the host, exactly as codegen does.
    assert!(
        text.contains("wasi_snapshot_preview1.fd_write (i32) -> (i32) [host trait]"),
        "{text}"
    );
    assert!(text.contains("env.log (i32) -> () [host trait]"), "{text}");
    assert!(text.contains("host trait: required"), "{text}");
}

#[test]
fn a_module_with_only_native_wasi_imports_needs_no_host_trait() {
    let text = summary(
        r#"(module
            (import "wasi_snapshot_preview1" "proc_exit" (func (param i32)))
            (memory 1))"#,
    );
    assert!(text.contains("host trait: not required"), "{text}");
}

#[test]
fn an_imported_global_or_memory_needs_the_host_trait() {
    let global = summary(r#"(module (import "env" "g" (global i32)))"#);
    assert!(global.contains("host trait: required"), "{global}");
    let memory = summary(r#"(module (import "env" "memory" (memory 1)))"#);
    assert!(memory.contains("host trait: required"), "{memory}");
    // A shared memory is a handle passed to `new`, not lent by the host.
    let shared = summary(r#"(module (import "env" "memory" (memory 1 1 shared)))"#);
    assert!(shared.contains("host trait: not required"), "{shared}");
}

#[test]
fn lists_exports_with_kind_and_index() {
    let text = summary(
        r#"(module
            (memory (export "mem") 1)
            (func (export "run") (result i32) i32.const 1))"#,
    );
    assert!(text.contains("run (func 0)"), "{text}");
    assert!(text.contains("mem (memory 0)"), "{text}");
}

#[test]
fn describes_memory_limits_and_sharing() {
    let text = summary(r#"(module (memory 1 4 shared))"#);
    assert!(text.contains("memory 0: min 1, max 4, shared"), "{text}");
    let text = summary(r#"(module (memory 2))"#);
    assert!(text.contains("memory 0: min 2, no max"), "{text}");
}

#[test]
fn reports_the_proposals_a_module_uses() {
    let text = summary(
        r#"(module
            (memory 1 1 shared)
            (func (export "a") (result i32)
              i32.const 0 i32.const 1 i32.atomic.rmw.add))"#,
    );
    assert!(text.contains("features: atomics"), "{text}");

    let simd = summary(
        r#"(module (func (result i32)
            i32.const 1 i32x4.splat i32x4.extract_lane 0))"#,
    );
    assert!(simd.contains("simd"), "{simd}");

    let tail = summary(
        r#"(module
            (func $f (result i32) i32.const 1)
            (func (result i32) return_call $f))"#,
    );
    assert!(tail.contains("tail-call"), "{tail}");

    let plain = summary(r#"(module (func (result i32) i32.const 1))"#);
    assert!(plain.contains("features: none"), "{plain}");
}

#[test]
fn a_module_the_transpiler_rejects_can_still_be_dumped() {
    // A shared 64-bit memory is unsupported by codegen, but is a valid binary.
    let wasm = wat::parse_str(r#"(module (memory i64 1 1 shared))"#).unwrap();
    assert!(wasm2rs::transpile(&wasm).is_err());
    let text = wasm2rs::dump(&wasm).unwrap().to_string();
    assert!(text.contains("memory 0: min 1, max 1, shared"), "{text}");
    assert!(wasm2rs::dump(b"not wasm").is_err());
}

#[test]
fn the_host_trait_verdict_matches_what_codegen_generates() {
    let modules = [
        r#"(module (import "wasi_snapshot_preview1" "sched_yield" (func (result i32))))"#,
        r#"(module (import "env" "f" (func)))"#,
        r#"(module (import "wasi_snapshot_preview1" "fd_close" (func (param i64) (result i32))))"#,
        r#"(module (import "env" "g" (global i32)))"#,
        r#"(module (import "env" "memory" (memory 1)))"#,
        r#"(module (import "env" "memory" (memory 1 1 shared)))"#,
        r#"(module (memory 1))"#,
        // An imported tag is not a host-trait item in codegen.
        r#"(module (import "env" "e" (tag (param i32))))"#,
        r#"(module (import "env" "t" (table 1 funcref)))"#,
    ];
    for wat in modules {
        let wasm = wat::parse_str(wat).unwrap();
        let generated = wasm2rs::transpile(&wasm).unwrap();
        let text = wasm2rs::dump(&wasm).unwrap().to_string();
        assert_eq!(
            text.contains("host trait: required"),
            generated.contains("trait Imports"),
            "{wat}\n{text}"
        );
    }
}

fn write_wasm(name: &str) -> std::path::PathBuf {
    let wasm = wat::parse_str(r#"(module (func (export "a") (result i32) i32.const 1))"#).unwrap();
    let dir = std::env::temp_dir().join(format!("wasm2rs_dump_{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("in.wasm");
    std::fs::write(&path, wasm).unwrap();
    path
}

#[test]
fn the_dump_flag_prints_the_summary_and_no_code() {
    let input = write_wasm("cli");
    let out = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg("--dump")
        .arg(&input)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("functions: 1 (0 imported, 1 defined)"),
        "{stdout}"
    );
    assert!(!stdout.contains("pub struct Instance"), "{stdout}");
}

#[test]
fn the_dump_flag_rejects_an_output_path() {
    let input = write_wasm("cli_output");
    let out = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
        .arg("--dump")
        .arg(&input)
        .arg("out.rs")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--dump"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Every flag that only changes the generated code: `--dump` generates none, so it
/// refuses them instead of silently ignoring them. (The defaults `--jobs=0` and
/// `--split-dispatch=0` change nothing and are accepted.)
#[test]
fn the_dump_flag_rejects_code_generation_flags() {
    let input = write_wasm("cli_flags");
    for flag in [
        "--unsafe-memory",
        "--named-exports",
        "--named-imports",
        "--import-memory",
        "--reentrant-imports",
        "--split-dispatch=4",
        "--jobs=2",
        "--entry-exports=a",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_wasm2rs"))
            .arg("--dump")
            .arg(flag)
            .arg(&input)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let name = flag.split('=').next().unwrap();
        assert!(!out.status.success(), "{flag} was accepted");
        assert!(
            stderr.contains("--dump") && stderr.contains(name),
            "{flag}: {stderr}"
        );
        assert!(out.stdout.is_empty(), "{flag} printed a summary");
    }
}

#[test]
fn an_imported_tag_is_listed_but_does_not_need_the_host_trait() {
    let text = summary(r#"(module (import "env" "e" (tag (param i32))))"#);
    assert!(text.contains("env.e tag [exception tag]"), "{text}");
    assert!(text.contains("tags: 1"), "{text}");
    assert!(text.contains("host trait: not required"), "{text}");
}

#[test]
fn describes_non_function_imports_and_every_export_kind() {
    let text = summary(
        r#"(module
            (import "env" "g" (global (mut i64)))
            (import "env" "t" (table 1 funcref))
            (import "env" "m" (memory 1))
            (tag $e (param i32))
            (global (export "glob") i32 (i32.const 0))
            (table (export "tab") 1 funcref)
            (export "tag" (tag $e))
            (export "mem" (memory 0)))"#,
    );
    assert!(text.contains("env.g global mut i64 [host trait]"), "{text}");
    assert!(text.contains("env.t table [host trait]"), "{text}");
    assert!(text.contains("env.m memory [host trait]"), "{text}");
    assert!(
        text.contains("memory 0: min 1, no max (imported)"),
        "{text}"
    );
    assert!(text.contains("globals: 2"), "{text}");
    assert!(text.contains("tables: 2"), "{text}");
    assert!(text.contains("glob (global 1)"), "{text}");
    assert!(text.contains("tab (table 1)"), "{text}");
    assert!(text.contains("tag (tag 0)"), "{text}");
}

#[test]
fn counts_segments() {
    let text = summary(
        r#"(module
            (memory 1)
            (table 2 funcref)
            (func $f)
            (data (i32.const 0) "ab")
            (data "cd")
            (elem (i32.const 0) $f))"#,
    );
    assert!(text.contains("data segments: 2"), "{text}");
    assert!(text.contains("element segments: 1"), "{text}");
}

#[test]
fn reports_exceptions_gc_multi_memory_and_wasi_threads() {
    let eh = summary(
        r#"(module (tag $e)
            (func (try_table (catch_all 0) (throw $e))))"#,
    );
    assert!(eh.contains("features: exceptions"), "{eh}");

    let gc = summary(
        r#"(module (type $s (struct (field i32)))
            (func (result (ref $s)) i32.const 1 struct.new $s))"#,
    );
    assert!(gc.contains("features: gc"), "{gc}");

    let multi = summary(r#"(module (memory 1) (memory 1))"#);
    assert!(multi.contains("features: multi-memory"), "{multi}");

    let threads = summary(
        r#"(module
            (import "wasi" "thread-spawn" (func (param i32) (result i32)))
            (memory 1 1 shared))"#,
    );
    assert!(threads.contains("features: wasi-threads"), "{threads}");
}

#[test]
fn reports_continuations() {
    let text = summary(
        r#"(module
            (type $ft (func))
            (type $ct (cont $ft))
            (func $f)
            (elem declare func $f)
            (func (result (ref $ct)) ref.func $f cont.new $ct))"#,
    );
    assert!(text.contains("continuations"), "{text}");
}

#[test]
fn compact_import_groups_are_listed_item_by_item_with_their_own_signatures() {
    // Encoding 1 (`item name type`) gives each item its own type; encoding 2
    // (`item name` ... then one shared type) shares one. Both expand to one line
    // per imported function, classified like single imports.
    let text = summary(
        r#"(module
            (import "env"
              (item "one" (func (param i32)))
              (item "two" (func (result i64))))
            (import "wasi_snapshot_preview1"
              (item "fd_write")
              (item "fd_read")
              (func (param i32 i32 i32 i32) (result i32))))"#,
    );
    assert!(
        text.contains("functions: 4 (4 imported, 0 defined)"),
        "{text}"
    );
    assert!(text.contains("env.one (i32) -> () [host trait]"), "{text}");
    assert!(text.contains("env.two () -> (i64) [host trait]"), "{text}");
    for name in ["fd_write", "fd_read"] {
        assert!(
            text.contains(&format!(
                "wasi_snapshot_preview1.{name} (i32, i32, i32, i32) -> (i32) [native WASI]"
            )),
            "{text}"
        );
    }
}
