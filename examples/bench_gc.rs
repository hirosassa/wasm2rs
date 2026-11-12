//! Measures GC-object cost on the workloads in `benches/gc/`: each is
//! transpiled, built with `rustc -O`, and timed in-process; peak RSS comes from
//! `/usr/bin/time`. Usage: `cargo run --release --example bench_gc [runs]`.

use std::path::{Path, PathBuf};
use std::process::Command;

use wasm2rs::{TranspileOptions, transpile_with_options};

/// (name, wat, index of the exported `run` function, `run` argument). The
/// modules are stateless, so the generated code exposes free `funcN` functions.
const WORKLOADS: [(&str, &str, u32, i32); 2] = [
    (
        "array_i32",
        include_str!("../benches/gc/array_i32.wat"),
        0,
        60,
    ),
    ("tree", include_str!("../benches/gc/tree.wat"), 2, 60),
];

fn build(dir: &Path, name: &str, wat: &str, f: u32, n: i32) -> PathBuf {
    let wasm = wat::parse_str(wat).expect("valid wat");
    let opts = TranspileOptions::default().with_named_exports(true);
    let generated = transpile_with_options(&wasm, &opts).expect("transpile");
    let program = format!(
        "{generated}\n#[inline(never)]\nfn drive(n: i32) -> i32 {{ func{f}(n) }}\n\
         fn main() {{\n\
         \x20   let t = std::time::Instant::now();\n\
         \x20   let r = std::hint::black_box(drive(std::hint::black_box({n})));\n\
         \x20   println!(\"{{}} {{}}\", t.elapsed().as_nanos(), r);\n\
         }}\n"
    );
    let src = dir.join(format!("{name}.rs"));
    let bin = dir.join(name);
    std::fs::write(&src, program).expect("write source");
    let status = Command::new("rustc")
        .args(["--edition", "2024", "-C", "opt-level=3", "-A", "warnings"])
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("run rustc");
    assert!(status.success(), "rustc failed for {name}");
    bin
}

/// (elapsed ms, peak RSS MiB, checksum). macOS `time -l` reports bytes; Linux
/// `time -v` reports KiB.
fn run(bin: &Path) -> (f64, f64, String) {
    let (flag, div) = if cfg!(target_os = "macos") {
        ("-l", 1024.0 * 1024.0)
    } else {
        ("-v", 1024.0)
    };
    let out = Command::new("/usr/bin/time")
        .arg(flag)
        .arg(bin)
        .output()
        .expect("run workload");
    assert!(out.status.success(), "workload failed");
    let text = String::from_utf8(out.stdout).expect("utf8");
    let (ns, sum) = text.trim().split_once(' ').expect("`ns checksum`");
    let err = String::from_utf8_lossy(&out.stderr);
    let rss = err
        .lines()
        .find(|l| l.contains("maximum resident set size") || l.contains("Maximum resident"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.parse::<f64>().ok()))
        .unwrap_or(0.0);
    (
        ns.parse::<f64>().expect("nanos") / 1e6,
        rss / div,
        sum.to_string(),
    )
}

fn main() {
    let runs: usize = std::env::args()
        .nth(1)
        .map_or(5, |a| a.parse().expect("runs"));
    let dir = std::env::temp_dir().join(format!("wasm2rs-bench-gc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create dir");
    println!("| workload | time (ms, median) | peak RSS (MiB) | checksum |");
    println!("|---|---|---|---|");
    for (name, wat, f, n) in WORKLOADS {
        let bin = build(&dir, name, wat, f, n);
        let mut results: Vec<_> = (0..runs).map(|_| run(&bin)).collect();
        results.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (t, rss, sum) = &results[results.len() / 2];
        println!("| {name} | {t:.1} | {rss:.1} | {sum} |");
    }
    std::fs::remove_dir_all(&dir).ok();
}
