//! The configuration surface of `WasiHost`: driven through the `Host` trait directly, so no
//! guest program is compiled.

use std::path::PathBuf;
use wasm2rs_wasi::wasi_filesystem_types::{
    Descriptor, DescriptorFlags, ErrorCode as Fs, OpenFlags, PathFlags,
};
use wasm2rs_wasi::wasi_sockets_network::{
    ErrorCode as Net, IpSocketAddress, Ipv4SocketAddress, Network,
};
use wasm2rs_wasi::{Access, ConfigError, Host, NetworkPolicy, WasiHost};

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("wasm2rs-wasi-config-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn arguments_environment_and_cwd_are_set_by_the_builder() {
    let mut host = WasiHost::builder()
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
    let mut host = WasiHost::builder().build().unwrap();
    assert!(host.wasi_filesystem_preopens_get_directories().is_empty());
    assert_eq!(host.wasi_cli_environment_initial_cwd(), None);
}

#[test]
fn the_default_cwd_is_the_preopen_named_slash() {
    let dir = scratch("cwd");
    let mut host = WasiHost::builder()
        .preopen("/", &dir, Access::ReadOnly)
        .build()
        .unwrap();
    assert_eq!(
        host.wasi_cli_environment_initial_cwd().as_deref(),
        Some("/")
    );
    let mut host = WasiHost::builder()
        .preopen("data", &dir, Access::ReadOnly)
        .build()
        .unwrap();
    assert_eq!(host.wasi_cli_environment_initial_cwd(), None);
    let mut host = WasiHost::builder()
        .preopen("/", &dir, Access::ReadOnly)
        .initial_cwd(None)
        .build()
        .unwrap();
    assert_eq!(host.wasi_cli_environment_initial_cwd(), None);
}

#[test]
fn a_missing_preopen_directory_is_an_error_not_a_panic() {
    let missing = scratch("missing").join("nope");
    let err = WasiHost::builder()
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
    let err = WasiHost::builder()
        .preopen("/", dir.join("f"), Access::ReadOnly)
        .build()
        .err()
        .unwrap();
    assert!(matches!(err, ConfigError::NotADirectory(_)), "{err}");
}

#[test]
fn duplicate_and_empty_guest_names_are_errors() {
    let dir = scratch("dup");
    let err = WasiHost::builder()
        .preopen("a", &dir, Access::ReadOnly)
        .preopen("a", &dir, Access::ReadWrite)
        .build()
        .err()
        .unwrap();
    assert!(
        matches!(err, ConfigError::DuplicatePreopen(ref n) if n == "a"),
        "{err}"
    );
    let err = WasiHost::builder()
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
    let mut host = WasiHost::builder()
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
    // a descriptor opened below a read-only preopen stays read-only
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
    let mut host = WasiHost::builder()
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
    let mut none = WasiHost::builder()
        .network(NetworkPolicy::None)
        .build()
        .unwrap();
    assert_eq!(bind(&mut none, loopback(0)), Err(Net::AccessDenied));
    assert_eq!(bind(&mut none, outside(0)), Err(Net::AccessDenied));

    let mut lo = WasiHost::builder()
        .network(NetworkPolicy::Loopback)
        .build()
        .unwrap();
    assert_eq!(bind(&mut lo, loopback(0)), Ok(()));
    assert_eq!(bind(&mut lo, outside(0)), Err(Net::AccessDenied));

    // the default is loopback only
    let mut default = WasiHost::builder().build().unwrap();
    assert_eq!(bind(&mut default, outside(0)), Err(Net::AccessDenied));

    // external passes the policy check; 192.0.2.1 is not ours, so the OS refuses later
    let mut ext = WasiHost::builder()
        .network(NetworkPolicy::External)
        .build()
        .unwrap();
    assert_ne!(bind(&mut ext, outside(0)), Err(Net::AccessDenied));
}

#[test]
fn name_lookup_outside_localhost_needs_external() {
    for policy in [NetworkPolicy::None, NetworkPolicy::Loopback] {
        let mut host = WasiHost::builder().network(policy).build().unwrap();
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
