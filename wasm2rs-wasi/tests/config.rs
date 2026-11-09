//! The configuration surface of `WasiHost`: driven through the `Host` trait directly, so no
//! guest program is compiled.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use wasm2rs_wasi::wasi_clocks_wall_clock::Datetime;
use wasm2rs_wasi::wasi_filesystem_types::{
    Descriptor, DescriptorFlags, ErrorCode as Fs, OpenFlags, PathFlags,
};
use wasm2rs_wasi::wasi_sockets_network::{
    ErrorCode as Net, IpSocketAddress, Ipv4SocketAddress, Network,
};
use wasm2rs_wasi::{
    Access, Clock, ConfigError, Host, InsecureRandom, NetworkPolicy, RandomError, RandomSource,
    WasiHost, WasiHostBuilder,
};

/// The builder, with a random source when the feature that provides a default is off.
fn builder() -> WasiHostBuilder {
    let builder = WasiHost::builder();
    if cfg!(feature = "os-random") {
        builder
    } else {
        builder.random(Fixed(0))
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("wasm2rs-wasi-config-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn arguments_environment_and_cwd_are_set_by_the_builder() {
    let mut host = builder()
        .arguments(vec!["prog".into(), "-v".into()])
        .environment(vec![("K".into(), "V".into())])
        .initial_cwd(Some("/work".into()))
        .build()
        .unwrap();
    assert_eq!(host.wasi_cli_environment_get_arguments(), ["prog", "-v"]);
    assert_eq!(
        host.wasi_cli_environment_get_environment(),
        [("K".to_string(), "V".to_string())]
    );
    assert_eq!(
        host.wasi_cli_environment_initial_cwd().as_deref(),
        Some("/work")
    );
}

#[test]
fn defaults_have_no_preopen_and_no_cwd() {
    let mut host = builder().build().unwrap();
    assert!(host.wasi_filesystem_preopens_get_directories().is_empty());
    assert_eq!(host.wasi_cli_environment_initial_cwd(), None);
}

#[test]
fn the_default_cwd_is_the_preopen_named_slash() {
    let dir = scratch("cwd");
    let mut host = builder()
        .preopen("/", &dir, Access::ReadOnly)
        .build()
        .unwrap();
    assert_eq!(
        host.wasi_cli_environment_initial_cwd().as_deref(),
        Some("/")
    );
    let mut host = builder()
        .preopen("data", &dir, Access::ReadOnly)
        .build()
        .unwrap();
    assert_eq!(host.wasi_cli_environment_initial_cwd(), None);
    let mut host = builder()
        .preopen("/", &dir, Access::ReadOnly)
        .initial_cwd(None)
        .build()
        .unwrap();
    assert_eq!(host.wasi_cli_environment_initial_cwd(), None);
}

#[test]
fn a_missing_preopen_directory_is_an_error_not_a_panic() {
    let missing = scratch("missing").join("nope");
    let err = builder()
        .preopen("/", &missing, Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert!(matches!(err, ConfigError::Preopen { .. }), "{err}");
}

#[test]
fn a_file_is_not_a_preopen_directory() {
    let dir = scratch("file");
    std::fs::write(dir.join("f"), b"x").unwrap();
    let err = builder()
        .preopen("/", dir.join("f"), Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert!(matches!(err, ConfigError::NotADirectory(_)), "{err}");
}

#[test]
fn duplicate_and_empty_guest_names_are_errors() {
    let dir = scratch("dup");
    let err = builder()
        .preopen("a", &dir, Access::ReadOnly)
        .preopen("a", &dir, Access::ReadWrite)
        .build()
        .err()
        .unwrap();
    assert!(
        matches!(err, ConfigError::DuplicatePreopen(ref n) if n == "a"),
        "{err}"
    );
    let err = builder()
        .preopen("", &dir, Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert!(matches!(err, ConfigError::EmptyPreopenName), "{err}");
}

#[test]
fn several_preopens_have_their_own_name_root_and_access() {
    let ro = scratch("ro");
    let rw = scratch("rw");
    std::fs::write(ro.join("only-in-ro"), b"r").unwrap();
    let mut host = builder()
        .preopen("ro", &ro, Access::ReadOnly)
        .preopen("rw", &rw, Access::ReadWrite)
        .build()
        .unwrap();
    assert_eq!(
        host.wasi_filesystem_preopens_get_directories(),
        [
            (Descriptor(0), "ro".to_string()),
            (Descriptor(1), "rw".to_string())
        ]
    );
    // access is per preopen
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_create_directory_at(Descriptor(0), "d".into()),
        Err(Fs::ReadOnly)
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_create_directory_at(Descriptor(1), "d".into()),
        Ok(())
    );
    assert!(rw.join("d").is_dir() && !ro.join("d").exists());
    // a descriptor opened below a writable preopen inherits its access
    let sub = host
        .wasi_filesystem_types_method_descriptor_open_at(
            Descriptor(1),
            PathFlags::SYMLINK_FOLLOW,
            "d".into(),
            OpenFlags::DIRECTORY,
            DescriptorFlags::READ,
        )
        .unwrap();
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_create_directory_at(sub, "e".into()),
        Ok(())
    );
    // each root confines its own descriptors
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_open_at(
            Descriptor(0),
            PathFlags::SYMLINK_FOLLOW,
            "../rw".into(),
            OpenFlags::DIRECTORY,
            DescriptorFlags::READ,
        ),
        Err(Fs::NotPermitted)
    );
    // no preopen is ever freed
    host.wasi_filesystem_types_resource_drop_descriptor(Descriptor(0));
    host.wasi_filesystem_types_resource_drop_descriptor(Descriptor(1));
    assert!(
        host.wasi_filesystem_types_method_descriptor_get_flags(Descriptor(0))
            .is_ok()
    );
    assert!(
        host.wasi_filesystem_types_method_descriptor_get_flags(Descriptor(1))
            .is_ok()
    );
    // a file opened under the preopens gets a slot after them
    assert_eq!(sub, Descriptor(2));
}

#[test]
fn rename_across_preopens_needs_both_writable() {
    let ro = scratch("xro");
    let rw = scratch("xrw");
    std::fs::write(rw.join("f"), b"x").unwrap();
    let mut host = builder()
        .preopen("ro", &ro, Access::ReadOnly)
        .preopen("rw", &rw, Access::ReadWrite)
        .build()
        .unwrap();
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_rename_at(
            Descriptor(1),
            "f".into(),
            Descriptor(0),
            "g".into()
        ),
        Err(Fs::ReadOnly)
    );
    assert!(rw.join("f").exists());
}

fn loopback(port: u16) -> IpSocketAddress {
    IpSocketAddress::Ipv4(Ipv4SocketAddress {
        port,
        address: (127, 0, 0, 1),
    })
}

fn outside(port: u16) -> IpSocketAddress {
    IpSocketAddress::Ipv4(Ipv4SocketAddress {
        port,
        address: (192, 0, 2, 1),
    })
}

fn bind(host: &mut WasiHost, addr: IpSocketAddress) -> Result<(), Net> {
    use wasm2rs_wasi::wasi_sockets_network::IpAddressFamily;
    let socket = host
        .wasi_sockets_tcp_create_socket_create_tcp_socket(IpAddressFamily::Ipv4)
        .unwrap();
    host.wasi_sockets_tcp_method_tcp_socket_start_bind(socket, Network(0), addr)
}

#[test]
fn network_policy_decides_which_addresses_are_allowed() {
    let mut none = builder().network(NetworkPolicy::None).build().unwrap();
    assert_eq!(bind(&mut none, loopback(0)), Err(Net::AccessDenied));
    assert_eq!(bind(&mut none, outside(0)), Err(Net::AccessDenied));

    let mut lo = builder().network(NetworkPolicy::Loopback).build().unwrap();
    assert_eq!(bind(&mut lo, loopback(0)), Ok(()));
    assert_eq!(bind(&mut lo, outside(0)), Err(Net::AccessDenied));

    // the default is loopback only
    let mut default = builder().build().unwrap();
    assert_eq!(bind(&mut default, outside(0)), Err(Net::AccessDenied));

    // external passes the policy check; 192.0.2.1 is not ours, so the OS refuses later
    let mut ext = builder().network(NetworkPolicy::External).build().unwrap();
    assert_ne!(bind(&mut ext, outside(0)), Err(Net::AccessDenied));
}

#[test]
fn name_lookup_outside_localhost_needs_external() {
    for policy in [NetworkPolicy::None, NetworkPolicy::Loopback] {
        let mut host = builder().network(policy).build().unwrap();
        assert_eq!(
            host.wasi_sockets_ip_name_lookup_resolve_addresses(
                Network(0),
                "example.invalid".into()
            )
            .err(),
            Some(Net::PermanentResolverFailure)
        );
    }
}

#[test]
fn the_legacy_constructors_are_the_builder() {
    let dir = scratch("legacy");
    let mut host = WasiHost::new(vec![("A".into(), "B".into())])
        .with_arguments(vec!["x".into()])
        .with_writable_preopen(dir.clone())
        .with_external_network();
    assert_eq!(host.wasi_cli_environment_get_arguments(), ["x"]);
    assert_eq!(
        host.wasi_filesystem_preopens_get_directories(),
        [(Descriptor(0), "/".to_string())]
    );
    assert_eq!(
        host.wasi_cli_environment_initial_cwd().as_deref(),
        Some("/")
    );
    assert_eq!(
        host.wasi_filesystem_types_method_descriptor_create_directory_at(Descriptor(0), "d".into()),
        Ok(())
    );
    assert_ne!(bind(&mut host, outside(0)), Err(Net::AccessDenied));
}

#[test]
#[should_panic(expected = "preopen: directory does not exist")]
fn the_legacy_with_preopen_still_panics_on_a_missing_directory() {
    let missing = scratch("legacy-missing").join("nope");
    let _ = WasiHost::new(Vec::new()).with_preopen(missing);
}

/// A clock that never moves.
struct FrozenClock;

impl Clock for FrozenClock {
    fn monotonic_now(&self) -> u64 {
        5_000
    }
    fn monotonic_resolution(&self) -> u64 {
        7
    }
    fn wall_now(&self) -> Datetime {
        Datetime {
            seconds: 1_000,
            nanoseconds: 2,
        }
    }
    fn wall_resolution(&self) -> Datetime {
        Datetime {
            seconds: 0,
            nanoseconds: 3,
        }
    }
}

#[test]
fn an_injected_clock_gives_now_and_resolution() {
    let mut host = builder()
        .clock(FrozenClock)
        .random(Fixed(0))
        .build()
        .unwrap();
    assert_eq!(host.wasi_clocks_monotonic_clock_now(), 5_000);
    assert_eq!(host.wasi_clocks_monotonic_clock_resolution(), 7);
    assert_eq!(
        host.wasi_clocks_wall_clock_now(),
        Datetime {
            seconds: 1_000,
            nanoseconds: 2
        }
    );
    assert_eq!(
        host.wasi_clocks_wall_clock_resolution(),
        Datetime {
            seconds: 0,
            nanoseconds: 3
        }
    );
}

#[test]
fn waits_stay_in_real_time_whatever_the_clock_says() {
    let mut host = builder()
        .clock(FrozenClock)
        .random(Fixed(0))
        .build()
        .unwrap();
    // an instant at or before the clock's now is ready at once
    let past = host.wasi_clocks_monotonic_clock_subscribe_instant(5_000);
    assert!(host.wasi_io_poll_method_pollable_ready(past));
    // the frozen clock never reaches this one, and the wait is not shortened by it
    let later = host.wasi_clocks_monotonic_clock_subscribe_instant(5_000 + 3_600_000_000_000);
    assert!(!host.wasi_io_poll_method_pollable_ready(later));
}

#[test]
fn the_default_clock_is_the_system_one() {
    let mut host = builder().random(Fixed(0)).build().unwrap();
    let a = host.wasi_clocks_monotonic_clock_now();
    let b = host.wasi_clocks_monotonic_clock_now();
    assert!(b >= a);
    assert!(host.wasi_clocks_wall_clock_now().seconds > 1_600_000_000);
    assert_eq!(host.wasi_clocks_monotonic_clock_resolution(), 1);
}

/// Every secure byte is the same value; the insecure generator is a constant.
struct Fixed(u8);

impl RandomSource for Fixed {
    fn fill_random(&mut self, buf: &mut [u8]) -> Result<(), RandomError> {
        buf.fill(self.0);
        Ok(())
    }
    fn insecure_u64(&mut self) -> u64 {
        42
    }
    fn insecure_seed(&mut self) -> (u64, u64) {
        (1, 2)
    }
}

#[test]
fn an_injected_random_source_is_what_the_guest_sees() {
    let mut host = builder().random(Fixed(0xAB)).build().unwrap();
    assert_eq!(host.wasi_random_random_get_random_bytes(3), [0xAB; 3]);
    assert_eq!(
        host.wasi_random_random_get_random_u64(),
        0xABAB_ABAB_ABAB_ABAB
    );
    assert_eq!(host.wasi_random_insecure_get_insecure_random_u64(), 42);
    assert_eq!(host.wasi_random_insecure_seed_insecure_seed(), (1, 2));
    assert_eq!(
        host.wasi_random_insecure_get_insecure_random_bytes(10),
        [42u64.to_le_bytes().as_slice(), &[42, 0]].concat()
    );
}

#[test]
#[should_panic(expected = "random bytes: length over 64 MiB")]
fn the_random_length_limit_still_traps() {
    let mut host = builder().random(Fixed(0)).build().unwrap();
    host.wasi_random_random_get_random_bytes((64 << 20) + 1);
}

struct Failing;

impl RandomSource for Failing {
    fn fill_random(&mut self, _: &mut [u8]) -> Result<(), RandomError> {
        Err(RandomError::new("entropy pool unavailable"))
    }
    fn insecure_u64(&mut self) -> u64 {
        0
    }
    fn insecure_seed(&mut self) -> (u64, u64) {
        (0, 0)
    }
}

#[test]
#[should_panic(expected = "the random source failed: entropy pool unavailable")]
fn a_failing_secure_source_traps_instead_of_returning_weak_bytes() {
    let mut host = builder().random(Failing).build().unwrap();
    host.wasi_random_random_get_random_u64();
}

#[test]
fn the_insecure_generator_is_deterministic_per_seed() {
    let mut a = InsecureRandom::new(7);
    let mut b = InsecureRandom::new(7);
    assert_eq!(a.next_u64(), b.next_u64());
    assert_ne!(a.next_u64(), InsecureRandom::new(8).next_u64());
}

#[cfg(feature = "os-random")]
#[test]
fn the_default_random_source_is_the_os_one() {
    let mut host = builder().build().unwrap();
    let a = host.wasi_random_random_get_random_bytes(32);
    let b = host.wasi_random_random_get_random_bytes(32);
    assert_eq!((a.len(), b.len()), (32, 32));
    assert_ne!(a, b);
    assert_ne!(a, [0; 32]);
    assert!(host.wasi_random_random_get_random_bytes(0).is_empty());
}

#[cfg(not(feature = "os-random"))]
#[test]
fn without_os_random_a_source_is_required() {
    let err = WasiHost::builder().build().err().unwrap();
    assert!(matches!(err, ConfigError::MissingRandomSource), "{err}");
    assert!(WasiHost::builder().random(Fixed(0)).build().is_ok());
}

/// A writer that keeps what it was given in `pending` until it is flushed into `done`.
#[derive(Clone, Default)]
struct Sink {
    pending: Arc<Mutex<Vec<u8>>>,
    done: Arc<Mutex<Vec<u8>>>,
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let mut pending = self.pending.lock().unwrap();
        self.done.lock().unwrap().append(&mut pending);
        Ok(())
    }
}

impl Sink {
    fn flushed(&self) -> Vec<u8> {
        self.done.lock().unwrap().clone()
    }
    fn unflushed(&self) -> Vec<u8> {
        self.pending.lock().unwrap().clone()
    }
}

/// A reader that gives `data`, then fails with `error` if set, else ends.
struct Script {
    data: std::io::Cursor<Vec<u8>>,
    error: bool,
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.data.read(buf)? {
            0 if self.error => Err(std::io::Error::other("the reader broke")),
            n => Ok(n),
        }
    }
}

fn bytes(data: &[u8]) -> Script {
    Script {
        data: std::io::Cursor::new(data.to_vec()),
        error: false,
    }
}

#[test]
fn stdout_and_stderr_go_to_the_given_writers() {
    let (out, err) = (Sink::default(), Sink::default());
    let mut host = builder()
        .stdout(out.clone())
        .stderr(err.clone())
        .build()
        .unwrap();
    let o = host.wasi_cli_stdout_get_stdout();
    let e = host.wasi_cli_stderr_get_stderr();
    host.wasi_io_streams_method_output_stream_blocking_write_and_flush(o, b"to out".to_vec())
        .unwrap();
    host.wasi_io_streams_method_output_stream_blocking_write_and_flush(e, b"to err".to_vec())
        .unwrap();
    assert_eq!(out.flushed(), b"to out");
    assert_eq!(err.flushed(), b"to err");
}

#[test]
fn a_write_stays_buffered_until_the_guest_flushes() {
    let out = Sink::default();
    let mut host = builder().stdout(out.clone()).build().unwrap();
    let o = host.wasi_cli_stdout_get_stdout();
    host.wasi_io_streams_method_output_stream_write(o, b"abc".to_vec())
        .unwrap();
    assert_eq!(out.unflushed(), b"abc");
    host.wasi_io_streams_method_output_stream_blocking_flush(o)
        .unwrap();
    assert_eq!(out.flushed(), b"abc");
}

#[test]
fn dropping_the_host_flushes_stdout_and_stderr() {
    let (out, err) = (Sink::default(), Sink::default());
    let mut host = builder()
        .stdout(out.clone())
        .stderr(err.clone())
        .build()
        .unwrap();
    let (o, e) = (
        host.wasi_cli_stdout_get_stdout(),
        host.wasi_cli_stderr_get_stderr(),
    );
    host.wasi_io_streams_method_output_stream_write(o, b"late out".to_vec())
        .unwrap();
    host.wasi_io_streams_method_output_stream_write(e, b"late err".to_vec())
        .unwrap();
    drop(host);
    assert_eq!(out.flushed(), b"late out");
    assert_eq!(err.flushed(), b"late err");
}

#[test]
fn stdin_is_read_from_the_given_reader_and_ends_with_closed() {
    use wasm2rs_wasi::wasi_io_streams::StreamError;
    let mut host = builder().stdin(bytes(b"hello")).build().unwrap();
    let i = host.wasi_cli_stdin_get_stdin();
    let got = host
        .wasi_io_streams_method_input_stream_blocking_read(i, 16)
        .unwrap();
    assert_eq!(got, b"hello");
    let end = host.wasi_io_streams_method_input_stream_blocking_read(i, 16);
    assert!(matches!(end, Err(StreamError::Closed)));
}

#[test]
fn a_failing_stdin_reader_reports_the_error_after_its_bytes() {
    use wasm2rs_wasi::wasi_io_streams::StreamError;
    let reader = Script {
        data: std::io::Cursor::new(b"ab".to_vec()),
        error: true,
    };
    let mut host = builder().stdin(reader).build().unwrap();
    let i = host.wasi_cli_stdin_get_stdin();
    let got = host
        .wasi_io_streams_method_input_stream_blocking_read(i, 16)
        .unwrap();
    assert_eq!(got, b"ab");
    let failed = host.wasi_io_streams_method_input_stream_blocking_read(i, 16);
    assert!(matches!(failed, Err(StreamError::LastOperationFailed(_))));
}

#[test]
fn the_stdin_reader_starts_on_first_use() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Flag(Arc<AtomicBool>);
    impl Read for Flag {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.0.store(true, Ordering::SeqCst);
            Ok(0)
        }
    }
    let touched = Arc::new(AtomicBool::new(false));
    let mut host = builder().stdin(Flag(touched.clone())).build().unwrap();
    let i = host.wasi_cli_stdin_get_stdin();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!touched.load(Ordering::SeqCst), "read before first use");
    let _ = host.wasi_io_streams_method_input_stream_blocking_read(i, 1);
    assert!(touched.load(Ordering::SeqCst));
}

#[test]
fn a_failing_stdout_writer_is_a_stream_error_not_a_panic() {
    use wasm2rs_wasi::wasi_io_streams::StreamError;
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("sink broke"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut host = builder().stdout(Broken).build().unwrap();
    let o = host.wasi_cli_stdout_get_stdout();
    let r = host.wasi_io_streams_method_output_stream_blocking_write_and_flush(o, b"x".to_vec());
    assert!(matches!(r, Err(StreamError::LastOperationFailed(_))));
}
