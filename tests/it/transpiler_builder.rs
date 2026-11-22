//! `Transpiler` / `ComponentTranspiler` are what the `transpile*` function family wraps. These
//! tests pin the wiring (each axis reaches the output, a wrapper passes the right flags); that
//! the bytes are unchanged from before the builders existed was checked once against the
//! pre-refactor CLI output, and the golden tests cover the generator itself.

use wasm2rs::{
    ComponentOptions, ComponentTranspiler, FileContent, OutputFile, SplitOptions, TranspileError,
    TranspileOptions, Transpiler,
};

use crate::wasip2_run_example::{CORE, WORLD, component_of};

/// Three functions (so `funcs_per_file: 1` splits) and one 5 KiB data segment (so side files
/// have something to move out).
fn wasm() -> Vec<u8> {
    let data: String = (0..5000)
        .map(|i| format!("\\{:02x}", i % 251 + 1))
        .collect();
    wat::parse_str(format!(
        r#"(module
          (memory (export "mem") 1)
          (data (i32.const 0) "{data}")
          (func (export "a") (result i32) i32.const 1)
          (func (export "b") (result i32) i32.const 2)
          (func (export "c") (result i32) i32.const 3))"#
    ))
    .expect("valid wat")
}

fn split() -> SplitOptions {
    SplitOptions {
        funcs_per_file: 1,
        max_bytes_per_file: 0,
    }
}

fn unsafe_opts() -> TranspileOptions {
    TranspileOptions::default().with_unsafe_memory(true)
}

fn collect(
    run: impl FnOnce(&mut dyn FnMut(OutputFile) -> Result<(), TranspileError>),
) -> Vec<(String, FileContent)> {
    let mut files = Vec::new();
    run(&mut |f| {
        files.push((f.name, f.content));
        Ok(())
    });
    files
}

#[test]
fn a_default_builder_is_transpile() {
    let wasm = wasm();
    assert_eq!(
        Transpiler::default().transpile(&wasm).unwrap(),
        wasm2rs::transpile(&wasm).unwrap()
    );
}

#[test]
fn options_reach_the_single_file_output() {
    let wasm = wasm();
    let opts = unsafe_opts();
    assert_eq!(
        Transpiler::new(opts.clone()).transpile(&wasm).unwrap(),
        wasm2rs::transpile_with_options(&wasm, &opts).unwrap()
    );
}

#[test]
fn pieces_join_to_the_single_file_output() {
    let wasm = wasm();
    let opts = unsafe_opts();
    let pieces = Transpiler::new(opts.clone())
        .transpile_pieces(&wasm)
        .unwrap();
    assert_eq!(
        pieces.into_string(),
        wasm2rs::transpile_pieces_with_options(&wasm, &opts)
            .unwrap()
            .into_string()
    );
}

#[test]
fn split_matches_transpile_split_with_options() {
    let wasm = wasm();
    let opts = unsafe_opts();
    let new = collect(|sink| {
        Transpiler::new(opts.clone())
            .with_split(split())
            .run(&wasm, sink)
            .unwrap();
    });
    let mut old = Vec::new();
    wasm2rs::transpile_split_with_options(&wasm, &split(), &opts, |f| {
        old.push((f.name, FileContent::Text(f.code)));
        Ok(())
    })
    .unwrap();
    assert!(old.len() > 1, "the module must actually split");
    assert_eq!(new, old);
}

#[test]
fn split_with_data_files_matches_transpile_split_files_with_options() {
    let wasm = wasm();
    let opts = TranspileOptions::default();
    let new = collect(|sink| {
        Transpiler::new(opts.clone())
            .with_split(split())
            .with_data_files(true)
            .run(&wasm, sink)
            .unwrap();
    });
    let old = collect(|sink| {
        wasm2rs::transpile_split_files_with_options(&wasm, &split(), &opts, sink).unwrap();
    });
    assert!(
        new.iter().any(|(_, c)| matches!(c, FileContent::Bytes(_))),
        "a side file must be emitted"
    );
    assert_eq!(new, old);
}

#[test]
fn a_single_file_run_is_the_transpile_output() {
    let wasm = wasm();
    let files = collect(|sink| {
        Transpiler::default().run(&wasm, sink).unwrap();
    });
    assert_eq!(files.len(), 1);
    assert_eq!(
        files[0],
        (
            "lib.rs".to_string(),
            FileContent::Text(wasm2rs::transpile(&wasm).unwrap())
        )
    );
}

#[test]
fn single_file_output_rejects_axes_it_cannot_honour() {
    let wasm = wasm();
    let split_err = Transpiler::default()
        .with_split(split())
        .transpile(&wasm)
        .unwrap_err();
    assert!(matches!(split_err, TranspileError::InvalidOptions(_)));
    let files_err = Transpiler::default()
        .with_data_files(true)
        .transpile_pieces(&wasm)
        .unwrap_err();
    assert!(matches!(files_err, TranspileError::InvalidOptions(_)));
}

#[test]
fn a_sink_error_stops_the_run() {
    let wasm = wasm();
    let err = Transpiler::default()
        .with_split(split())
        .run(&wasm, |_| Err(TranspileError::Internal("stop".into())))
        .unwrap_err();
    assert!(matches!(err, TranspileError::Internal(m) if m == "stop"));
}

// --- components ---

#[test]
fn a_default_component_builder_is_transpile_component() {
    let component = component_of(WORLD, CORE);
    let options = ComponentOptions::default();
    assert_eq!(
        ComponentTranspiler::new(options.clone())
            .transpile(&component)
            .unwrap(),
        wasm2rs::transpile_component(&component, &options).unwrap()
    );
    let (code, exports) = ComponentTranspiler::new(options.clone())
        .transpile_with_exports(&component)
        .unwrap();
    let (old_code, old_exports) =
        wasm2rs::transpile_component_with_exports(&component, &options).unwrap();
    assert_eq!(code, old_code);
    assert_eq!(format!("{exports:?}"), format!("{old_exports:?}"));
}

#[test]
fn component_split_matches_both_legacy_functions() {
    let component = component_of(WORLD, CORE);
    let options = ComponentOptions::default();
    for data_files in [false, true] {
        let new = collect(|sink| {
            ComponentTranspiler::new(options.clone())
                .with_split(split())
                .with_data_files(data_files)
                .run(&component, sink)
                .unwrap();
        });
        let mut old = Vec::new();
        if data_files {
            wasm2rs::transpile_component_split_files(&component, &options, &split(), |f| {
                old.push((f.name, f.content));
                Ok(())
            })
            .unwrap();
        } else {
            wasm2rs::transpile_component_split(&component, &options, &split(), |f| {
                old.push((f.name, FileContent::Text(f.code)));
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(new, old, "data_files = {data_files}");
    }
}
