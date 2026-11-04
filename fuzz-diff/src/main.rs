//! `cargo run --release -- [first_seed] [count]`: differential-fuzz `count` modules
//! (default 100 from seed 0). Findings are written to `findings/seed-N.wasm`.

use std::sync::atomic::{AtomicU64, Ordering};

use wasm2rs_fuzz_diff::{Verdict, differential, generate};

fn main() {
    let mut args = std::env::args().skip(1);
    let first: u64 = args.next().map_or(0, |a| a.parse().expect("first_seed"));
    let count: u64 = args.next().map_or(100, |a| a.parse().expect("count"));
    let next = AtomicU64::new(0);
    let counts = std::sync::Mutex::new(std::collections::BTreeMap::<&'static str, u64>::new());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= count {
                        return;
                    }
                    let seed = first + i;
                    let Some(wasm) = generate(seed) else {
                        continue;
                    };
                    let work = std::env::temp_dir()
                        .join(format!("wasm2rs-fuzz-{}-{seed}", std::process::id()));
                    let verdict = differential(&wasm, &work);
                    std::fs::remove_dir_all(&work).ok();
                    let kind = match &verdict {
                        Verdict::Match => "match",
                        Verdict::Skip(_) => "skip",
                        Verdict::Unsupported(_) => "unsupported",
                        Verdict::Mismatch { .. } => "MISMATCH",
                        Verdict::Broken(_) => "BROKEN",
                    };
                    *counts.lock().unwrap().entry(kind).or_default() += 1;
                    if matches!(kind, "MISMATCH" | "BROKEN" | "unsupported") {
                        std::fs::create_dir_all("findings").ok();
                        std::fs::write(format!("findings/seed-{seed}.wasm"), &wasm).ok();
                        std::fs::write(
                            format!("findings/seed-{seed}.txt"),
                            format!("{verdict:#?}"),
                        )
                        .ok();
                        eprintln!("seed {seed}: {kind}");
                    }
                }
            });
        }
    });
    for (kind, n) in counts.lock().unwrap().iter() {
        println!("{kind}: {n}");
    }
}
