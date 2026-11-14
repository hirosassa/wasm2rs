//! Large data segments of a split output can live in `data_{n}.bin` side files that `lib.rs`
//! pulls in with `include_bytes!` (`transpile_split_files`). The behavioural oracle is the
//! inline output: the same module must compute the same thing either way.

use std::process::Command;

use wasm2rs::{FileContent, OutputFile, SplitOptions, TranspileOptions};

/// `n` non-zero bytes as a WAT string body (`\01\02...`), a pattern that is easy to spot-check.
fn pattern(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| u8::try_from(i % 251 + 1).expect("fits"))
        .collect()
}

fn wat_escape(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}

fn split_files(wat: &str, funcs_per_file: usize, topts: &TranspileOptions) -> Vec<OutputFile> {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let split = SplitOptions {
        funcs_per_file,
        max_bytes_per_file: 0,
    };
    let mut files = Vec::new();
    wasm2rs::transpile_split_files_with_options(&wasm, &split, topts, |f| {
        files.push(f);
        Ok(())
    })
    .expect("transpile ok");
    files
}

fn lib_rs(files: &[OutputFile]) -> &str {
    files
        .iter()
        .find_map(|f| match (&f.content, f.name.as_str()) {
            (FileContent::Text(code), "lib.rs") => Some(code.as_str()),
            _ => None,
        })
        .expect("lib.rs emitted")
}

fn side_file<'a>(files: &'a [OutputFile], name: &str) -> Option<&'a [u8]> {
    files.iter().find_map(|f| match &f.content {
        FileContent::Bytes(bytes) if f.name == name => Some(bytes.as_slice()),
        _ => None,
    })
}

fn side_names(files: &[OutputFile]) -> Vec<&str> {
    files
        .iter()
        .filter(|f| matches!(f.content, FileContent::Bytes(_)))
        .map(|f| f.name.as_str())
        .collect()
}

/// Write `files` (side files included) into a temp dir, append `main` to `lib.rs`, build with
/// `rustc -D warnings` and run it.
fn compile_run(name: &str, files: &[OutputFile], main_body: &str) {
    let dir = std::env::temp_dir().join(format!("wasm2rs_df_{name}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    for f in files {
        match &f.content {
            FileContent::Text(code) => {
                let mut code = code.clone();
                if f.name == "lib.rs" {
                    code.push_str(&format!("\nfn main() {{\n{main_body}\n}}\n"));
                }
                std::fs::write(dir.join(&f.name), code).expect("write source");
            }
            FileContent::Bytes(bytes) => std::fs::write(dir.join(&f.name), bytes).expect("write"),
        }
    }
    let bin = dir.join("gen");
    let out = Command::new("rustc")
        .current_dir(&dir)
        .args(["lib.rs", "--edition", "2024", "-D", "warnings", "-o"])
        .arg(&bin)
        .output()
        .expect("run rustc");
    assert!(
        out.status.success(),
        "crate failed to compile ({name}):\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        Command::new(&bin).status().expect("run").success(),
        "generated program assertions failed: {name}"
    );
}

/// A module with one active segment of `bytes` at `offset` and a `load8_u` export (`func0`),
/// plus a second function so two chunks exist.
fn active_wat(offset: u32, bytes: &[u8]) -> String {
    format!(
        r#"(module (memory 1) (data (i32.const {offset}) "{}")
             (func (param i32) (result i32) local.get 0 i32.load8_u)
             (func (result i32) i32.const 0))"#,
        wat_escape(bytes)
    )
}

const CHECK_PATTERN: &str = "let mut inst = Instance::new();\n\
    for i in [0usize, 1, 250, 251, 4095, 4999] {\n\
        assert_eq!(inst.func0(i as i32), (i % 251 + 1) as i32, \"byte {i}\");\n\
    }\n\
    assert_eq!(inst.func0(5000), 0);";

#[test]
fn a_large_active_segment_goes_to_a_side_file_and_runs() {
    let bytes = pattern(5000);
    let wat = active_wat(0, &bytes);
    let files = split_files(&wat, 1, &TranspileOptions::default());

    assert_eq!(side_file(&files, "data_0.bin"), Some(bytes.as_slice()));
    let lib = lib_rs(&files);
    assert!(
        lib.contains(r#"static DATA0: [u8; 5000] = *include_bytes!("data_0.bin");"#),
        "{lib}"
    );
    assert!(!lib.contains("*b\""), "no inline literal expected");
    compile_run("large_active", &files, CHECK_PATTERN);
}

#[test]
fn the_side_file_build_matches_the_inline_build() {
    let wat = active_wat(0, &pattern(5000));
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let split = SplitOptions {
        funcs_per_file: 1,
        max_bytes_per_file: 0,
    };
    let mut inline = Vec::new();
    wasm2rs::transpile_split(&wasm, &split, |f| {
        inline.push(f);
        Ok(())
    })
    .expect("transpile ok");

    let side = split_files(&wat, 1, &TranspileOptions::default());
    // The only difference is the `DATA0` definition: every chunk is the same, and the root
    // matches once that one line is set aside.
    let strip = |code: &str| match code.find("static DATA0") {
        // The inline literal may span lines; the definition ends at `";` or `");`.
        Some(start) => {
            let end = ["\";\n", "\");\n"]
                .iter()
                .filter_map(|m| code[start..].find(m).map(|e| start + e + m.len()))
                .min()
                .expect("definition ends");
            format!("{}{}", &code[..start], &code[end..])
        }
        None => code.to_string(),
    };
    for src in &inline {
        let counterpart = side
            .iter()
            .find_map(|f| match &f.content {
                FileContent::Text(code) if f.name == src.name => Some(code),
                _ => None,
            })
            .expect("same file set");
        assert_eq!(strip(&src.code), strip(counterpart), "{}", src.name);
    }
    let inline_lib = &inline
        .iter()
        .find(|f| f.name == "lib.rs")
        .expect("lib")
        .code;
    assert!(inline_lib.contains("static DATA0: [u8; 5000] = *b\""));
}

#[test]
fn the_threshold_is_four_kib_inclusive() {
    let below = split_files(
        &active_wat(0, &pattern(4095)),
        1,
        &TranspileOptions::default(),
    );
    assert!(side_names(&below).is_empty());
    assert!(lib_rs(&below).contains("static DATA0: [u8; 4095] = *b\""));

    let at = split_files(
        &active_wat(0, &pattern(4096)),
        1,
        &TranspileOptions::default(),
    );
    assert_eq!(side_names(&at), ["data_0.bin"]);
}

#[test]
fn only_segments_over_the_threshold_leave_the_source() {
    let wat = format!(
        r#"(module (memory 1)
             (data (i32.const 0) "small")
             (data (i32.const 8192) "{}")
             (func (param i32) (result i32) local.get 0 i32.load8_u)
             (func (result i32) i32.const 0))"#,
        wat_escape(&pattern(4096))
    );
    let files = split_files(&wat, 1, &TranspileOptions::default());
    assert_eq!(side_names(&files), ["data_1.bin"]);
    let lib = lib_rs(&files);
    assert!(
        lib.contains("static DATA0: [u8; 5] = *b\"small\";"),
        "{lib}"
    );
    compile_run(
        "mixed",
        &files,
        "let mut inst = Instance::new();\n\
         assert_eq!(inst.func0(0), b's' as i32);\n\
         assert_eq!(inst.func0(8192), 1);\n\
         assert_eq!(inst.func0(8192 + 4095), (4095 % 251 + 1) as i32);",
    );
}

#[test]
fn zero_edges_are_trimmed_before_the_side_file_is_written() {
    let mut bytes = vec![0u8; 3000];
    bytes.extend(pattern(5000));
    bytes.extend(vec![0u8; 3000]);
    let files = split_files(&active_wat(0, &bytes), 1, &TranspileOptions::default());
    // Only the 5000-byte core is stored, and the instance copies it to offset 3000.
    assert_eq!(
        side_file(&files, "data_0.bin"),
        Some(pattern(5000).as_slice())
    );
    compile_run(
        "trimmed",
        &files,
        "let mut inst = Instance::new();\n\
         assert_eq!(inst.func0(2999), 0);\n\
         assert_eq!(inst.func0(3000), 1);\n\
         assert_eq!(inst.func0(3000 + 4999), (4999 % 251 + 1) as i32);\n\
         assert_eq!(inst.func0(3000 + 5000), 0);",
    );
}

#[test]
fn a_segment_that_trims_to_nothing_emits_no_file() {
    let files = split_files(
        &active_wat(0, &vec![0u8; 6000]),
        1,
        &TranspileOptions::default(),
    );
    assert!(side_names(&files).is_empty());
    assert!(!lib_rs(&files).contains("DATA0"));
    compile_run(
        "all_zero",
        &files,
        "let mut inst = Instance::new();\nassert_eq!(inst.func0(100), 0);",
    );
}

#[test]
fn a_passive_segment_is_a_side_file_that_memory_init_reads() {
    let bytes = pattern(6000);
    let wat = format!(
        r#"(module (memory 1) (data $p "{}")
             (func (param i32) (result i32) local.get 0 i32.load8_u)
             (func (param i32 i32 i32) local.get 0 local.get 1 local.get 2 memory.init $p))"#,
        wat_escape(&bytes)
    );
    let files = split_files(&wat, 1, &TranspileOptions::default());
    assert_eq!(side_file(&files, "data_0.bin"), Some(bytes.as_slice()));
    compile_run(
        "passive",
        &files,
        "let mut inst = Instance::new();\n\
         assert_eq!(inst.func0(10), 0);\n\
         inst.func1(10, 5000, 1000);\n\
         assert_eq!(inst.func0(10), (5000 % 251 + 1) as i32);\n\
         assert_eq!(inst.func0(1009), (5999 % 251 + 1) as i32);\n\
         assert_eq!(inst.func0(1010), 0);",
    );
}

/// A host-owned memory is not zero-filled by us, so the zeros of its segment are data: the
/// side file must hold the whole segment.
#[test]
fn an_imported_memory_segment_keeps_its_zero_edges_in_the_side_file() {
    let mut bytes = vec![0u8; 100];
    bytes.extend(pattern(4000));
    let wat = format!(
        r#"(module (import "env" "memory" (memory 1)) (data (i32.const 0) "{}")
             (func (result i32) i32.const 0)
             (func (result i32) i32.const 1))"#,
        wat_escape(&bytes)
    );
    let topts = TranspileOptions::default().with_import_memory(true);
    let files = split_files(&wat, 1, &topts);
    assert_eq!(side_file(&files, "data_0.bin"), Some(bytes.as_slice()));
}

#[test]
fn a_shared_memory_segment_keeps_its_zero_edges_in_the_side_file() {
    let mut bytes = vec![0u8; 100];
    bytes.extend(pattern(4000));
    let wat = format!(
        r#"(module (memory 1 1 shared) (data (i32.const 0) "{}")
             (func (result i32) i32.const 0)
             (func (result i32) i32.const 1))"#,
        wat_escape(&bytes)
    );
    let files = split_files(&wat, 1, &TranspileOptions::default());
    assert_eq!(side_file(&files, "data_0.bin"), Some(bytes.as_slice()));
}

#[test]
fn a_single_file_split_still_uses_side_files() {
    let bytes = pattern(5000);
    let files = split_files(&active_wat(0, &bytes), 0, &TranspileOptions::default());
    assert_eq!(side_file(&files, "data_0.bin"), Some(bytes.as_slice()));
    assert!(lib_rs(&files).contains("include_bytes!(\"data_0.bin\")"));
    compile_run("single", &files, CHECK_PATTERN);
}

#[test]
fn the_inline_api_never_emits_a_side_file() {
    let wasm = wat::parse_str(active_wat(0, &pattern(5000))).expect("valid wat");
    let split = SplitOptions {
        funcs_per_file: 1,
        max_bytes_per_file: 0,
    };
    let mut names = Vec::new();
    wasm2rs::transpile_split(&wasm, &split, |f| {
        assert!(!f.code.contains("include_bytes!"));
        names.push(f.name);
        Ok(())
    })
    .expect("transpile ok");
    assert!(names.iter().all(|n| n.ends_with(".rs")));
}
