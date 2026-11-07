//! One component that imports both `wasi:filesystem` and `wasi:sockets`: each declares an
//! `error-code` enum, so the generated trait names them `ErrorCode` and `ErrorCode2` in an
//! order that follows the import order (the world and the WIT packages agree). The typed host must serve both, in either order.

use crate::wasip2_typed_host::{assert_succeeded, component_from_wit, run_program};

const FILESYSTEM: &str = r#"
package wasi:filesystem@0.2.0;
interface types {
  flags descriptor-flags {
    read, write, file-integrity-sync, data-integrity-sync, requested-write-sync, mutate-directory
  }
  flags path-flags { symlink-follow }
  flags open-flags { create, directory, exclusive, truncate }
  enum error-code {
    access, would-block, already, bad-descriptor, busy, deadlock, quota, exist, file-too-large,
    illegal-byte-sequence, in-progress, interrupted, invalid, io, is-directory, loop,
    too-many-links, message-size, name-too-long, no-device, no-entry, no-lock,
    insufficient-memory, insufficient-space, not-directory, not-empty, not-recoverable,
    unsupported, no-tty, no-such-device, overflow, not-permitted, pipe, read-only,
    invalid-seek, text-file-busy, cross-device
  }
  resource descriptor {
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
  }
}
interface preopens {
  use types.{descriptor};
  get-directories: func() -> list<tuple<descriptor, string>>;
}
"#;

const SOCKETS: &str = r#"
package wasi:sockets@0.2.0;
interface network {
  resource network;
  enum error-code {
    unknown, access-denied, not-supported, invalid-argument, out-of-memory, timeout,
    concurrency-conflict, not-in-progress, would-block, invalid-state, new-socket-limit,
    address-not-bindable, address-in-use, remote-unreachable, connection-refused,
    connection-reset, connection-aborted, datagram-too-large, name-unresolvable,
    temporary-resolver-failure, permanent-resolver-failure
  }
  enum ip-address-family { ipv4, ipv6 }
}
interface tcp {
  use network.{error-code};
  resource tcp-socket {
    start-listen: func() -> result<_, error-code>;
  }
}
interface tcp-create-socket {
  use network.{error-code, ip-address-family};
  use tcp.{tcp-socket};
  create-tcp-socket: func(address-family: ip-address-family) -> result<tcp-socket, error-code>;
}
"#;

const FILESYSTEM_FIRST: &str = r#"
package test:t;
world w {
  import wasi:filesystem/preopens@0.2.0;
  import wasi:filesystem/types@0.2.0;
  import wasi:sockets/network@0.2.0;
  import wasi:sockets/tcp@0.2.0;
  import wasi:sockets/tcp-create-socket@0.2.0;
}
"#;

const SOCKETS_FIRST: &str = r#"
package test:t;
world w {
  import wasi:sockets/network@0.2.0;
  import wasi:sockets/tcp@0.2.0;
  import wasi:sockets/tcp-create-socket@0.2.0;
  import wasi:filesystem/preopens@0.2.0;
  import wasi:filesystem/types@0.2.0;
}
"#;

const CORE: &str = r#"
(module
  (import "wasi:filesystem/types@0.2.0" "[method]descriptor.open-at" (func $open (param i32 i32 i32 i32 i32 i32 i32)))
  (import "wasi:filesystem/types@0.2.0" "[resource-drop]descriptor" (func $drop_fd (param i32)))
  (import "wasi:filesystem/preopens@0.2.0" "get-directories" (func $dirs (param i32)))
  (import "wasi:sockets/network@0.2.0" "[resource-drop]network" (func (param i32)))
  (import "wasi:sockets/tcp@0.2.0" "[resource-drop]tcp-socket" (func $drop_sock (param i32)))
  (import "wasi:sockets/tcp@0.2.0" "[method]tcp-socket.start-listen" (func $listen (param i32 i32)))
  (import "wasi:sockets/tcp-create-socket@0.2.0" "create-tcp-socket" (func $create (param i32 i32)))
  (memory (export "memory") 1)
  (global $brk (mut i32) (i32.const 1024))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (local $p i32)
    (local.set $p (i32.and (i32.add (global.get $brk) (i32.sub (local.get 2) (i32.const 1)))
                           (i32.sub (i32.const 0) (local.get 2))))
    (global.set $brk (i32.add (local.get $p) (local.get 3)))
    (local.get $p))
  (data (i32.const 300) "hello.txt")
  (data (i32.const 330) "missing")

  ;; open-at from the preopen: 0, or 100 + the filesystem error code
  (func (export "fs_open") (param $p i32) (param $n i32) (result i32) (local $fd i32)
    (call $dirs (i32.const 100))
    (local.set $fd (i32.load (i32.load (i32.const 100))))
    (call $open (local.get $fd) (i32.const 1) (local.get $p) (local.get $n)
                (i32.const 0) (i32.const 1) (i32.const 200))
    (if (result i32) (i32.load8_u (i32.const 200))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 204))))
      (else (call $drop_fd (i32.load (i32.const 204))) (i32.const 0))))

  ;; create-tcp-socket(ipv4): the handle, or -1
  (func (export "sock_create") (result i32)
    (call $create (i32.const 0) (i32.const 240))
    (if (result i32) (i32.load8_u (i32.const 240))
      (then (i32.const -1))
      (else (i32.load (i32.const 244)))))
  ;; start-listen: 0, or 100 + the sockets error code
  (func (export "sock_listen") (param $h i32) (result i32)
    (call $listen (local.get $h) (i32.const 260))
    (if (result i32) (i32.load8_u (i32.const 260))
      (then (i32.add (i32.const 100) (i32.load8_u (i32.const 261))))
      (else (i32.const 0)))))
"#;

/// Runs the guest on a component built from `world`; the import order (`deps` and `world`
/// agree) decides which of the two `error-code` enums gets the `2` suffix. `label` keeps the
/// scratch directories of parallel tests apart.
fn run(label: &str, deps: &[&str], world: &str) -> std::process::Output {
    let root =
        std::env::temp_dir().join(format!("wasm2rs_typed_both_{}_{label}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), "hello").unwrap();
    run_program(
        &component_from_wit(deps, world, "w"),
        CORE,
        &format!(
            "TypedHost::new(Vec::new()).with_preopen(std::path::PathBuf::from({:?}))",
            root.to_str().unwrap()
        ),
        "// filesystem: success, then no-entry 20
         assert_eq!(inst.fs_open(300, 9), 0);
         assert_eq!(inst.fs_open(330, 7), 100 + 20);
         // sockets: success, then invalid-state 9 (listening before bind)
         let s = inst.sock_create();
         assert!(s >= 0, \"{s}\");
         assert_eq!(inst.sock_listen(s), 100 + 9);",
    )
}

#[test]
fn a_component_importing_filesystem_then_sockets_runs() {
    assert_succeeded(&run("fs_first", &[FILESYSTEM, SOCKETS], FILESYSTEM_FIRST));
}

#[test]
fn a_component_importing_sockets_then_filesystem_runs() {
    assert_succeeded(&run("sockets_first", &[SOCKETS, FILESYSTEM], SOCKETS_FIRST));
}
