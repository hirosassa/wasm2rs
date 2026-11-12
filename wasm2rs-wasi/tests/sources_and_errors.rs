//! The injectable sources (clock, randomness) with their defaults, the text of the
//! configuration errors, and how a failed file stream is reported to the guest.

mod common;

use common::{builder, scratch, traps};
use std::error::Error as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wasm2rs_wasi::wasi_clocks_wall_clock::Datetime;
use wasm2rs_wasi::wasi_io_error::Error as IoError;
use wasm2rs_wasi::{
    Access, Clock, ConfigError, Host, HttpClientError, InsecureRandom, RandomError, RandomSource,
    SystemClock, WasiHost,
};

// ---- configuration errors ---------------------------------------------------------------

#[test]
fn each_config_error_says_what_is_wrong_and_only_a_failed_resolve_has_a_source() {
    let missing = scratch("missing").join("nope");
    let err = builder()
        .preopen("/", &missing, Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert!(matches!(err, ConfigError::Preopen { .. }));
    let text = err.to_string();
    assert!(
        text.starts_with(&format!("preopen {}: ", missing.display())),
        "{text}"
    );
    let source = err.source().expect("the I/O error is the source");
    assert!(text.ends_with(&source.to_string()), "{text}");

    let file = scratch("file").join("f");
    std::fs::write(&file, b"x").unwrap();
    let err = builder()
        .preopen("/", &file, Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        format!("preopen {}: not a directory", file.display())
    );
    assert!(err.source().is_none());

    let dir = scratch("dir");
    let err = builder()
        .preopen("a", &dir, Access::ReadOnly)
        .preopen("a", &dir, Access::ReadWrite)
        .build()
        .err()
        .unwrap();
    assert_eq!(err.to_string(), "two preopens are named \"a\"");
    assert!(err.source().is_none());

    let err = builder()
        .preopen("", &dir, Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert_eq!(err.to_string(), "a preopen has an empty guest name");

    // the message of this error is part of what users see
    assert_eq!(
        ConfigError::MissingRandomSource.to_string(),
        "no random source: give one to the builder or enable the `os-random` feature"
    );
}

#[test]
fn preopen_checks_run_in_order_name_then_duplicate_then_directory() {
    // the order of the checks: empty name, duplicate, then the directory
    let dir = scratch("order");
    let gone = dir.join("gone");
    let err = builder()
        .preopen("", &gone, Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert!(matches!(err, ConfigError::EmptyPreopenName), "{err}");
    let err = builder()
        .preopen("x", &dir, Access::ReadOnly)
        .preopen("x", &gone, Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert!(
        matches!(err, ConfigError::DuplicatePreopen(ref n) if n == "x"),
        "{err}"
    );
}

#[test]
fn descriptors_follow_preopen_call_order_and_no_slash_preopen_means_no_cwd() {
    let a = scratch("order-a");
    let b = scratch("order-b");
    std::fs::write(a.join("in-a"), b"1").unwrap();
    std::fs::write(b.join("in-b"), b"2").unwrap();
    let mut host = builder()
        .preopen("second", &b, Access::ReadOnly)
        .preopen("first", &a, Access::ReadWrite)
        .build()
        .unwrap();
    let dirs = host.wasi_filesystem_preopens_get_directories();
    assert_eq!(
        dirs.iter()
            .map(|(d, n)| (d.0, n.as_str()))
            .collect::<Vec<_>>(),
        [(0, "second"), (1, "first")]
    );
    // with no preopen named "/" there is no initial directory unless asked for
    assert_eq!(host.wasi_cli_environment_initial_cwd(), None);
}

// ---- clocks and randomness --------------------------------------------------------------

struct Minimal;
impl Clock for Minimal {
    fn monotonic_now(&self) -> u64 {
        9
    }
    fn wall_now(&self) -> Datetime {
        Datetime {
            seconds: 1,
            nanoseconds: 2,
        }
    }
}

#[test]
fn a_clock_that_gives_only_the_required_methods_has_one_nanosecond_resolution() {
    let mut host = builder().clock(Minimal).build().unwrap();
    assert_eq!(host.wasi_clocks_monotonic_clock_now(), 9);
    assert_eq!(host.wasi_clocks_monotonic_clock_resolution(), 1);
    assert_eq!(
        host.wasi_clocks_wall_clock_resolution(),
        Datetime {
            seconds: 0,
            nanoseconds: 1
        }
    );
}

#[test]
fn the_system_clock_counts_from_its_creation_and_the_wall_clock_is_the_systems() {
    let clock = SystemClock::default();
    let first = clock.monotonic_now();
    std::thread::sleep(Duration::from_millis(5));
    let second = clock.monotonic_now();
    assert!(second >= first + 4_000_000, "{first} -> {second}");
    // counted from creation, not from the machine's boot
    assert!(first < 60_000_000_000, "{first}");
    assert_eq!(clock.monotonic_resolution(), 1);

    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let wall = clock.wall_now();
    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!((before..=after).contains(&wall.seconds), "{wall:?}");
    assert!(wall.nanoseconds < 1_000_000_000);
}

#[test]
fn the_insecure_generator_is_splitmix64() {
    // the published reference outputs of splitmix64 from seed 0
    let mut g = InsecureRandom::new(0);
    assert_eq!(g.next_u64(), 0xE220_A839_7B1D_CDAF);
    assert_eq!(g.next_u64(), 0x6E78_9E6A_A1B9_65F4);
    assert_eq!(g.next_u64(), 0x06C4_5D18_8009_454F);
    // a seed near the top of the range wraps instead of overflowing
    assert_eq!(InsecureRandom::new(u64::MAX).next_u64(), 0xE4D971771B652C20);
    // two OS-seeded generators do not start in the same place
    assert_ne!(
        InsecureRandom::from_os().next_u64(),
        InsecureRandom::from_os().next_u64()
    );
}

#[test]
fn random_error_and_http_client_error_show_their_message() {
    assert_eq!(RandomError::new("no entropy").to_string(), "no entropy");
    assert_eq!(HttpClientError::new("dns down").to_string(), "dns down");
    assert!(RandomError::new("x").source().is_none());
}

#[cfg(feature = "os-random")]
#[test]
fn the_os_source_fills_every_length_and_its_insecure_values_move() {
    use wasm2rs_wasi::OsRandom;
    let mut os = OsRandom::new();
    let mut buf = [0u8; 4096];
    os.fill_random(&mut buf).unwrap();
    assert!(buf.iter().any(|&b| b != 0));
    let mut odd = [0u8; 1031];
    os.fill_random(&mut odd).unwrap();
    assert!(odd.iter().any(|&b| b != 0));
    assert_ne!(os.insecure_u64(), os.insecure_u64());
    let seed = os.insecure_seed();
    assert_ne!(seed, os.insecure_seed());
    let mut empty: [u8; 0] = [];
    assert!(os.fill_random(&mut empty).is_ok());
}

#[cfg(feature = "os-random")]
#[test]
fn a_default_os_source_is_independently_seeded_and_fills_like_new() {
    use wasm2rs_wasi::OsRandom;
    let mut a = OsRandom::default();
    let mut b = OsRandom::default();
    // Each is seeded from the OS, so two defaults do not replay one sequence.
    assert_ne!(a.insecure_u64(), b.insecure_u64());
    let mut buf = [0u8; 256];
    a.fill_random(&mut buf).unwrap();
    assert!(buf.iter().any(|&x| x != 0));
}

#[test]
fn insecure_bytes_are_the_generators_words_in_order_and_zero_length_is_empty() {
    struct Counter(u64);
    impl RandomSource for Counter {
        fn fill_random(&mut self, buf: &mut [u8]) -> Result<(), RandomError> {
            buf.fill(1);
            Ok(())
        }
        fn insecure_u64(&mut self) -> u64 {
            self.0 += 1;
            self.0
        }
        fn insecure_seed(&mut self) -> (u64, u64) {
            (self.0, self.0)
        }
    }
    let mut host = WasiHost::builder().random(Counter(0)).build().unwrap();
    assert!(
        host.wasi_random_insecure_get_insecure_random_bytes(0)
            .is_empty()
    );
    // two words, the second cut after 3 bytes: the first word's bytes are not reused
    let bytes = host.wasi_random_insecure_get_insecure_random_bytes(11);
    assert_eq!(&bytes[..8], 1u64.to_le_bytes());
    assert_eq!(&bytes[8..], &2u64.to_le_bytes()[..3]);
    // secure bytes come from `fill_random`, not from the insecure counter
    assert_eq!(host.wasi_random_random_get_random_bytes(2), [1, 1]);
    assert_eq!(host.wasi_random_insecure_get_insecure_random_u64(), 3);
    // 64 MiB exactly is allowed, one byte more traps (for both generators)
    assert_eq!(
        host.wasi_random_random_get_random_bytes(64 << 20).len(),
        64 << 20
    );
    assert!(traps(
        || host.wasi_random_random_get_random_bytes((64 << 20) + 1)
    ));
    assert!(traps(
        || host.wasi_random_insecure_get_insecure_random_bytes((64 << 20) + 1)
    ));
}

// ---- error handles ----------------------------------------------------------------------

#[test]
fn an_unknown_error_handle_has_no_message_and_no_code() {
    let mut host = builder().build().unwrap();
    assert_eq!(
        host.wasi_io_error_method_error_to_debug_string(IoError(12345)),
        ""
    );
    assert_eq!(
        host.wasi_filesystem_types_filesystem_error_code(IoError(12345)),
        None
    );
    // dropping what is not there is harmless
    host.wasi_io_error_resource_drop_error(IoError(12345));
}

/// A write that the OS refuses reaches the guest as `last-operation-failed` once, with the
/// filesystem `error-code` behind it, and the stream is `closed` afterwards. `/dev/full`
/// fails every write with ENOSPC.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_file_write_reports_a_filesystem_error_code_once_then_closes() {
    use wasm2rs_wasi::wasi_filesystem_types::{
        Descriptor, DescriptorFlags, ErrorCode as Fs, OpenFlags, PathFlags,
    };
    use wasm2rs_wasi::wasi_io_streams::StreamError;
    let mut host = builder()
        .preopen("/", "/dev", Access::ReadWrite)
        .build()
        .unwrap();
    let full = host
        .wasi_filesystem_types_method_descriptor_open_at(
            Descriptor(0),
            PathFlags::SYMLINK_FOLLOW,
            "full".into(),
            OpenFlags(0),
            DescriptorFlags::WRITE,
        )
        .unwrap();
    let out = host
        .wasi_filesystem_types_method_descriptor_write_via_stream(full, 0)
        .unwrap();
    let Err(StreamError::LastOperationFailed(err)) =
        host.wasi_io_streams_method_output_stream_write(out, b"x".to_vec())
    else {
        panic!("the first failure is last-operation-failed");
    };
    assert_eq!(
        host.wasi_filesystem_types_filesystem_error_code(err),
        Some(Fs::Io) // pins the current mapping of ENOSPC: relax if `insufficient-space` is added
    );
    assert!(
        !host
            .wasi_io_error_method_error_to_debug_string(err)
            .is_empty()
    );
    assert_eq!(
        host.wasi_io_streams_method_output_stream_write(out, b"y".to_vec()),
        Err(StreamError::Closed)
    );
    host.wasi_io_error_resource_drop_error(err);
}
