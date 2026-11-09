//! Stream failures on the typed host are `last-operation-failed(error)` with a live `error`
//! resource; the end of a stream and a dropped one stay `closed`. A failed connection (a
//! reset) is the case here; the failed send queue is in `wasip2_typed_send_queue`, a failed
//! file read in `wasip2_typed_fs_edges`.

use crate::wasip2_typed_host::assert_succeeded;
#[cfg(target_os = "linux")]
use crate::wasip2_typed_host::{build_program, component_from_wit};
use crate::wasip2_typed_sockets::run_with_main;

/// Reads `h` until it answers with a stream-error: -1 for `last-operation-failed`, -2 for
/// `closed` (the reset may take a moment to show).
const FIRST_ERROR: &str = r#"
#[allow(dead_code)]
fn first_error(inst: &mut Instance<Glue<WasiHost>>, h: i32) -> i32 {
    for _ in 0..2000 {
        let got = inst.read(h, 4096);
        if got < 0 {
            return got;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!("read: no error");
}
"#;

fn run(body: &str) -> std::process::Output {
    run_with_main(&format!("{FIRST_ERROR}\n{body}"))
}

#[test]
fn reading_a_reset_connection_reports_last_operation_failed_once_and_a_hang_up_is_closed() {
    assert_succeeded(&run("let (s, port) = serve(&mut inst);
         let client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let (out, input) = (inst.s_out(), inst.s_in());
         // the client leaves data unread, so closing it resets the connection
         assert_eq!(inst.blocking_write_and_flush(out, 400, 4), 0);
         std::thread::sleep(std::time::Duration::from_millis(100));
         drop(client);
         // the first error is `last-operation-failed` (case 0: -1; `closed` is -2) with a live
         // error handle
         assert_eq!(first_error(&mut inst, input), -1);
         let error = inst.last_err();
         assert_ne!(error, -1);
         // after the report the stream is `closed`, for every read and skip
         assert_eq!(inst.read(input, 4096), -2);
         assert_eq!(inst.blocking_read(input, 4096), -2);
         assert_eq!(inst.read(input, 0), -2);
         assert_eq!(inst.skip(input, 1), -2);
         inst.drop_error(error);
         // an orderly hang-up is the end of the input: `closed` at once, no error handle
         let client = std::net::TcpStream::connect((\"127.0.0.1\", port)).unwrap();
         accept(&mut inst, s);
         let input = inst.s_in();
         drop(client);
         assert_eq!(first_error(&mut inst, input), -2);
         assert_eq!(inst.last_err(), error);"));
}

/// A guest that writes to stdout: `write` returns 0, or 100 + the stream-error case.
#[cfg(target_os = "linux")]
const STDOUT_IO: &str = r#"
package wasi:io@0.2.0;
interface error {
  resource error;
}
interface streams {
  use error.{error};
  variant stream-error { last-operation-failed(error), closed }
  resource output-stream {
    write: func(contents: list<u8>) -> result<_, stream-error>;
  }
}
"#;

#[cfg(target_os = "linux")]
#[test]
fn a_failing_stdout_write_is_last_operation_failed() {
    const CLI: &str = r#"
package wasi:cli@0.2.0;
interface stdout {
  use wasi:io/streams@0.2.0.{output-stream};
  get-stdout: func() -> output-stream;
}
"#;
    const WORLD_OUT: &str = r#"
package test:t;
world w {
  import wasi:io/streams@0.2.0;
  import wasi:cli/stdout@0.2.0;
}
"#;
    const CORE_OUT: &str = r#"
(module
  (import "wasi:io/error@0.2.0" "[resource-drop]error" (func $drop_error (param i32)))
  (import "wasi:io/streams@0.2.0" "[resource-drop]output-stream" (func (param i32)))
  (import "wasi:io/streams@0.2.0" "[method]output-stream.write" (func $write (param i32 i32 i32 i32)))
  (import "wasi:cli/stdout@0.2.0" "get-stdout" (func $stdout (result i32)))
  (memory (export "memory") 1)
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 1024))
  (data (i32.const 400) "full\n")
  (func (export "stdout_handle") (result i32) (call $stdout))
  (func (export "write") (param $h i32) (result i32)
    (call $write (local.get $h) (i32.const 400) (i32.const 5) (i32.const 180))
    (if (result i32) (i32.load8_u (i32.const 180))
      (then
        (if (i32.eqz (i32.load8_u (i32.const 184)))
          (then (call $drop_error (i32.load (i32.const 188)))))
        (i32.add (i32.const 100) (i32.load8_u (i32.const 184))))
      (else (i32.const 0)))))
"#;
    let program = build_program(
        &component_from_wit(&[STDOUT_IO, CLI], WORLD_OUT, "w"),
        CORE_OUT,
        "WasiHost::new(Vec::new())",
        "let out = inst.stdout_handle();
         // `/dev/full` takes no byte: ENOSPC (the line ends with a newline, so `std` writes it)
         assert_eq!(inst.write(out), 100);",
    );
    let out = std::process::Command::new(program)
        .stdout(
            std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert_succeeded(&out);
}
