//! Measures what bounds checks cost on memory-heavy workloads: each workload in
//! `benches/memory/` is transpiled three ways — default (Rust's built-in checks),
//! `typed_traps` (explicit checks raising a typed trap) and `unsafe_memory` (every
//! memory check dropped, the upper bound on what removing checks could gain) —
//! built with `rustc -O`, and timed in-process. Prints speed, plus the size of the
//! optimized object file of the generated module alone (the program's std is left
//! out, so the code the option adds is not drowned).
//!
//! Usage: `cargo run --release --example bench_memory [runs]` (default 7 runs,
//! the median is reported). Prints Markdown tables. With `BENCH_KEEP_DIR=<dir>` the
//! timing binaries are built into `<dir>` (as `<workload>_<mode>`) and kept, so
//! binaries from two commits can be timed against each other.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use wasm2rs::{TranspileOptions, transpile_with_options};

/// (name, wat, `run` argument sized for roughly a second of work). `i32`, like the
/// generated `run` parameter, so an oversized count fails here rather than in rustc.
const WORKLOADS: [(&str, &str, i32); 6] = [
    ("hash", include_str!("../benches/memory/hash.wat"), 15_000),
    (
        "matmul",
        include_str!("../benches/memory/matmul.wat"),
        12_000,
    ),
    (
        "random",
        include_str!("../benches/memory/random.wat"),
        800_000_000,
    ),
    (
        "divrem",
        include_str!("../benches/memory/divrem.wat"),
        150_000_000,
    ),
    (
        "atomics",
        include_str!("../benches/memory/atomics.wat"),
        900_000_000,
    ),
    (
        "atomics_shared",
        include_str!("../benches/memory/atomics_shared.wat"),
        300_000_000,
    ),
];

#[derive(Clone, Copy)]
enum Mode {
    Safe,
    Typed,
    Unsafe,
}

impl Mode {
    const ALL: [Mode; 3] = [Mode::Safe, Mode::Typed, Mode::Unsafe];

    fn name(self) -> &'static str {
        match self {
            Mode::Safe => "safe",
            Mode::Typed => "typed",
            Mode::Unsafe => "unsafe",
        }
    }
}

/// Build the timing binary and return it with the object size of the generated
/// module compiled alone as a library.
fn build(dir: &Path, name: &str, wat: &str, n: i32, mode: Mode) -> (std::path::PathBuf, u64) {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = TranspileOptions::default()
        .with_named_exports(true)
        .with_typed_traps(matches!(mode, Mode::Typed))
        .with_unsafe_memory(matches!(mode, Mode::Unsafe));
    let generated = transpile_with_options(&wasm, &opts).expect("transpile");
    // `drive` is never inlined and the instance is opaque to the optimizer: with
    // the memory's length a visible constant, LLVM proves every access in bounds
    // and the safe and unsafe builds compile to identical code.
    let program = format!(
        "{generated}\n#[inline(never)]\nfn drive(m: &mut Instance, n: i32) -> i32 {{ m.run(n) }}\n\
         fn main() {{\n\
         \x20   let mut m = std::hint::black_box(Instance::new());\n\
         \x20   let t = std::time::Instant::now();\n\
         \x20   let r = std::hint::black_box(drive(&mut m, std::hint::black_box({n})));\n\
         \x20   println!(\"{{}} {{}}\", t.elapsed().as_nanos(), r);\n\
         }}\n"
    );
    let mode = mode.name();
    let src = dir.join(format!("{name}_{mode}.rs"));
    let bin = dir.join(format!("{name}_{mode}"));
    std::fs::write(&src, program).expect("write source");
    let obj = dir.join(format!("{name}_{mode}.o"));
    let lib_src = dir.join(format!("{name}_{mode}_lib.rs"));
    std::fs::write(&lib_src, &generated).expect("write lib source");
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=3", "-A", "warnings"])
        .args(["--crate-type=lib", "--emit=obj"])
        .arg(&lib_src)
        .arg("-o")
        .arg(&obj)
        .status()
        .expect("run rustc");
    assert!(status.success(), "rustc (object) failed for {name} {mode}");
    let obj_size = std::fs::metadata(&obj).expect("object size").len();
    let status = Command::new("rustc")
        .args([
            "--edition",
            "2024",
            "-C",
            "opt-level=3",
            "-C",
            "strip=symbols",
            "-A",
            "warnings",
        ])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run rustc");
    assert!(status.success(), "rustc failed for {name} {mode}");
    (bin, obj_size)
}

/// Run `bin` once; returns (elapsed, checksum).
fn time(bin: &Path) -> (Duration, String) {
    let out = Command::new(bin).output().expect("run workload");
    assert!(out.status.success(), "workload failed");
    let text = String::from_utf8(out.stdout).expect("utf8");
    let (ns, sum) = text.trim().split_once(' ').expect("`ns checksum`");
    (
        Duration::from_nanos(ns.parse().expect("nanos")),
        sum.to_string(),
    )
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn main() {
    let runs: usize = std::env::args()
        .nth(1)
        .map_or(7, |a| a.parse().expect("runs"));
    let keep = std::env::var_os("BENCH_KEEP_DIR");
    let dir = keep.as_ref().map_or_else(
        || std::env::temp_dir().join(format!("wasm2rs-bench-memory-{}", std::process::id())),
        std::path::PathBuf::from,
    );
    std::fs::create_dir_all(&dir).expect("create dir");

    println!("| workload | safe (ms) | typed (ms) | unsafe (ms) | typed / safe | unsafe / safe |");
    println!("|---|---|---|---|---|---|");
    let mut sizes = Vec::new();
    for (name, wat, n) in WORKLOADS {
        let built = Mode::ALL.map(|mode| build(&dir, name, wat, n, mode));
        let bins = built.clone().map(|(bin, _)| bin);
        sizes.push((name, built.map(|(_, size)| size)));
        let mut times: [Vec<Duration>; 3] = Default::default();
        let mut sum: Option<String> = None;
        // Interleave the variants so thermal / frequency drift hits all equally.
        for _ in 0..runs {
            for (i, bin) in bins.iter().enumerate() {
                let (t, s) = time(bin);
                times[i].push(t);
                match &sum {
                    Some(first) => assert_eq!(first, &s, "{name}: checksums differ"),
                    None => sum = Some(s),
                }
            }
        }
        let [safe, typed, fast] = times.map(median);
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        println!(
            "| {name} | {:.1} | {:.1} | {:.1} | {:.3} | {:.3} |  <!-- checksum {} -->",
            ms(safe),
            ms(typed),
            ms(fast),
            ms(typed) / ms(safe),
            ms(fast) / ms(safe),
            sum.unwrap_or_default()
        );
    }
    println!();
    println!(
        "| workload | safe object (bytes) | typed object (bytes) | unsafe object (bytes) | typed / safe |"
    );
    println!("|---|---|---|---|---|");
    for (name, [safe, typed, fast]) in sizes {
        println!(
            "| {name} | {safe} | {typed} | {fast} | {:.3} |",
            typed as f64 / safe as f64
        );
    }
    if keep.is_none() {
        std::fs::remove_dir_all(&dir).ok();
    }
}
