//! The memory-heavy workloads in `benches/memory/` (used by
//! `examples/bench_memory.rs`) compute the same checksum with and without
//! `unsafe_memory`, and that checksum matches a plain-Rust reference — so the
//! benchmark compares equal work.

use std::process::Command;

use crate::common;
use wasm2rs::{TranspileOptions, transpile_with_options};

const HASH: &str = include_str!("../../benches/memory/hash.wat");
const MATMUL: &str = include_str!("../../benches/memory/matmul.wat");
const RANDOM: &str = include_str!("../../benches/memory/random.wat");
const ATOMICS: &str = include_str!("../../benches/memory/atomics.wat");
const ATOMICS_SHARED: &str = include_str!("../../benches/memory/atomics_shared.wat");

/// Build `wat` with the given memory mode and return what `run(n)` prints.
fn run_checksum(name: &str, wat: &str, unsafe_memory: bool, n: u32) -> (String, String) {
    let wasm = wat::parse_str(wat).unwrap();
    let opts = TranspileOptions::default()
        .with_named_exports(true)
        .with_unsafe_memory(unsafe_memory);
    let generated = transpile_with_options(&wasm, &opts).unwrap();
    let program = format!(
        "{generated}\nfn main() {{ let mut m = Instance::new(); println!(\"{{}}\", m.run({n})); }}\n"
    );
    let bin = common::build(name, &program);
    let out = Command::new(bin).output().unwrap();
    assert!(out.status.success(), "{name} failed");
    (
        String::from_utf8(out.stdout).unwrap().trim().to_string(),
        generated,
    )
}

/// Both memory modes agree with `expected`, and really differ in codegen.
fn check(name: &str, wat: &str, n: u32, expected: i32) {
    let (safe, safe_src) = run_checksum(&format!("{name}_safe"), wat, false, n);
    let (fast, fast_src) = run_checksum(&format!("{name}_unsafe"), wat, true, n);
    assert_eq!(safe, expected.to_string(), "{name} safe");
    assert_eq!(fast, expected.to_string(), "{name} unsafe");
    assert!(!safe_src.contains("get_unchecked"), "{name}");
    assert!(fast_src.contains("get_unchecked"), "{name}");
}

#[test]
fn hash_workload_matches_reference() {
    let mem: Vec<u8> = (0..65536u32)
        .map(|i| i.wrapping_mul(31).to_le_bytes()[0])
        .collect();
    let mut h: u32 = 0x811C_9DC5;
    for _ in 0..3 {
        for b in &mem {
            h = (h ^ u32::from(*b)).wrapping_mul(16_777_619);
        }
    }
    check("bench_hash", HASH, 3, h.cast_signed());
}

#[test]
fn matmul_workload_matches_reference() {
    let a: Vec<u32> = (0..4096u32).map(|i| i % 7 + 1).collect();
    let b: Vec<u32> = (0..4096u32).map(|i| i % 5 + 1).collect();
    let mut sum: u32 = 0;
    for _ in 0..2 {
        for i in 0..64 {
            for j in 0..64 {
                let mut acc: u32 = 0;
                for k in 0..64 {
                    acc = acc.wrapping_add(a[i * 64 + k].wrapping_mul(b[k * 64 + j]));
                }
                sum = sum.wrapping_add(acc);
            }
        }
    }
    check("bench_matmul", MATMUL, 2, sum.cast_signed());
}

#[test]
fn random_workload_matches_reference() {
    let mut mem = vec![0u32; 16384];
    let (mut x, mut acc) = (12345u32, 0u32);
    for _ in 0..100_000 {
        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let a = ((x >> 8) & 65532) as usize / 4;
        let v = mem[a];
        mem[a] = v.wrapping_add(x);
        acc ^= v;
    }
    check("bench_random", RANDOM, 100_000, acc.cast_signed());
}

/// Plain-Rust model of `atomics.wat`.
fn atomics_reference(n: u32) -> i32 {
    let mut mem = vec![0u32; 16384];
    let mut acc = 0u32;
    for i in 0..n {
        let a = (i.wrapping_shl(2) & 65532) as usize / 4;
        mem[a] = i.wrapping_mul(2_654_435_761) ^ acc;
        acc = acc.wrapping_mul(31).wrapping_add(mem[a]);
        let old = mem[a];
        mem[a] = old.wrapping_add(acc);
        acc ^= old.wrapping_add(i);
        // cmpxchg: the expected value is the current one, so it always swaps.
        let prev = mem[a];
        mem[a] = acc;
        acc = acc.wrapping_add(prev);
    }
    acc.cast_signed()
}

#[test]
fn atomics_workload_matches_reference() {
    // 20_000 steps wrap the 16384-word memory, so the loop revisits addresses.
    let expected = atomics_reference(20_000);
    assert_ne!(
        expected, 0,
        "a degenerate checksum would hide a broken loop"
    );
    check("bench_atomics", ATOMICS, 20_000, expected);
}

#[test]
fn shared_atomics_workload_matches_reference() {
    // On a shared memory every atomic goes through `SharedMemory`, so `unsafe_memory`
    // has nothing to remove; only the checksum is compared.
    let expected = atomics_reference(20_000).to_string();
    let (safe, _) = run_checksum("bench_atomics_shared_safe", ATOMICS_SHARED, false, 20_000);
    let (fast, _) = run_checksum("bench_atomics_shared_unsafe", ATOMICS_SHARED, true, 20_000);
    assert_eq!(safe, expected, "shared safe");
    assert_eq!(fast, expected, "shared unsafe");
}

#[test]
fn shared_atomics_workload_differs_from_unshared_only_in_memory() {
    let body = |wat: &str| -> String {
        wat.lines()
            .skip_while(|l| *l != "(module")
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        body(ATOMICS_SHARED),
        body(ATOMICS).replace("(memory 1)", "(memory 1 1 shared)")
    );
}
