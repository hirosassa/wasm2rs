//! `cargo run --release -- [first_seed] [count] [--profile NAME]`: differential-fuzz
//! `count` modules (default 100 from seed 0) under a feature profile (`base`,
//! `memory64`, `multi-memory`, `multi-table`; default `base`). Findings are written to
//! `findings/seed-N.wasm`, or `findings/<profile>-seed-N.wasm` for a non-base profile.

use std::sync::atomic::{AtomicU64, Ordering};

use wasm2rs_fuzz_diff::{Profile, Verdict, differential, generate_with};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let profile: Profile = match args.iter().position(|a| a == "--profile") {
        Some(i) => {
            let name = args.get(i + 1).expect("--profile needs a name").clone();
            args.drain(i..=i + 1);
            name.parse().unwrap_or_else(|e| panic!("{e}"))
        }
        None => Profile::Base,
    };
    let stem = match profile {
        Profile::Base => String::new(),
        p => format!("{}-", p.name()),
    };
    let mut args = args.into_iter();
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
                    let Some(wasm) = generate_with(seed, profile) else {
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
                        let save = std::fs::create_dir_all("findings")
                            .and_then(|()| {
                                std::fs::write(format!("findings/{stem}seed-{seed}.wasm"), &wasm)
                            })
                            .and_then(|()| {
                                std::fs::write(
                                    format!("findings/{stem}seed-{seed}.txt"),
                                    format!("{verdict:#?}"),
                                )
                            });
                        if let Err(e) = save {
                            eprintln!("seed {seed}: cannot save the reproducer: {e}");
                        }
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
