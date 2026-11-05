//! Generating function bodies on several threads must not change a byte of the
//! output: the pieces are joined in function-index order, so a parallel run is
//! indistinguishable from a serial one — including which error surfaces first.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test code"
)]

use wasm2rs::transpile_with_options;
use wasm2rs::{SourceFile, SplitOptions, TranspileOptions, transpile_split_with_options};

/// A module of `n` functions whose bodies differ (so each pulls in different
/// helpers and the output has no repeated structure to hide a reordering).
fn many_functions(n: usize, stateful: bool) -> Vec<u8> {
    let mut wat = String::from("(module\n");
    if stateful {
        wat.push_str("  (memory 1)\n");
    }
    for i in 0..n {
        let body = match (stateful, i % 3) {
            (true, 0) => format!("(i32.load offset={i} (local.get 0))"),
            (true, 1) => format!("(i32.add (i32.load8_u (local.get 0)) (i32.const {i}))"),
            _ => format!("(i32.mul (local.get 0) (i32.const {i}))"),
        };
        wat.push_str(&format!(
            "  (func (export \"f{i}\") (param i32) (result i32) {body})\n"
        ));
    }
    wat.push(')');
    wat::parse_str(&wat).expect("valid wat")
}

fn with_jobs(jobs: usize) -> TranspileOptions {
    TranspileOptions {
        jobs,
        ..TranspileOptions::default()
    }
}

fn split_files(wasm: &[u8], per_file: usize, jobs: usize) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let split = SplitOptions {
        funcs_per_file: per_file,
        max_bytes_per_file: 0,
    };
    transpile_split_with_options(
        wasm,
        &split,
        &with_jobs(jobs),
        |SourceFile { name, code }| {
            files.push((name, code));
            Ok(())
        },
    )
    .expect("transpile ok");
    files
}

#[test]
fn single_file_output_is_identical_for_any_job_count() {
    for stateful in [false, true] {
        let wasm = many_functions(40, stateful);
        let serial = transpile_with_options(&wasm, &with_jobs(1)).unwrap();
        for jobs in [2, 4, 16] {
            let parallel = transpile_with_options(&wasm, &with_jobs(jobs)).unwrap();
            assert!(
                serial == parallel,
                "jobs={jobs} stateful={stateful} changed the single-file output"
            );
        }
    }
}

#[test]
fn split_output_is_identical_for_any_job_count() {
    for stateful in [false, true] {
        let wasm = many_functions(40, stateful);
        let serial = split_files(&wasm, 7, 1);
        assert!(serial.len() > 2, "expected a multi-chunk split");
        for jobs in [2, 4, 16] {
            let parallel = split_files(&wasm, 7, jobs);
            assert!(
                serial == parallel,
                "jobs={jobs} stateful={stateful} changed the split output"
            );
        }
    }
}

#[test]
fn a_failing_function_reports_the_same_error_for_any_job_count() {
    // Function 30 is the only one the transpiler rejects.
    let mut wat = String::from("(module (tag $e)\n");
    for i in 0..40 {
        if i == 30 {
            wat.push_str("(func (result i32) (ref.is_null (ref.null noexn)))\n");
        } else {
            wat.push_str(&format!("(func (result i32) (i32.const {i}))\n"));
        }
    }
    wat.push(')');
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let serial = transpile_with_options(&wasm, &with_jobs(1)).unwrap_err();
    for jobs in [2, 8] {
        let parallel = transpile_with_options(&wasm, &with_jobs(jobs)).unwrap_err();
        assert_eq!(serial.to_string(), parallel.to_string(), "jobs={jobs}");
    }
}

#[test]
fn a_panicking_sink_unwinds_instead_of_hanging_the_workers() {
    // Far more functions than the look-ahead window of 2 jobs, one per file, so
    // the first flush (and its panic) happens while the workers are throttled.
    let wasm = many_functions(400, false);
    let split = SplitOptions {
        funcs_per_file: 1,
        max_bytes_per_file: 0,
    };
    let outcome = std::panic::catch_unwind(|| {
        let _ =
            transpile_split_with_options(&wasm, &split, &with_jobs(2), |_| panic!("sink failed"));
    });
    assert!(outcome.is_err(), "the sink's panic must reach the caller");
}
