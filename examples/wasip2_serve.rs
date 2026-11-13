//! Serves a `wasm32-wasip2` component that exports `wasi:http/incoming-handler` (a `proxy`
//! world) over TCP, through the typed preview2 host of the `wasm2rs-wasi` crate.
//!
//! Usage: `cargo run --release --example wasip2_serve -- <component.wasm> [--listen ADDR] [--body-timeout SECS]`
//!
//! `ADDR` defaults to `127.0.0.1:8080` (loopback: bind elsewhere only on purpose). The program
//! is built like `wasip2_run`'s (see there), with the server of `wasip2_serve/server.rs`
//! appended instead of a `wasi:cli/run` call; once it listens it prints `listening on <addr>`
//! (`:0` picks a free port) and this process becomes it.
//!
//! One connection is one request, served one at a time, and the connection is closed after the
//! answer (`Connection: close`; no keep-alive, no concurrency). HTTP/1.x is minimal and lives
//! in the server, not in the library: a request line, headers and a `Content-Length` body. A
//! request with `Transfer-Encoding` is 501; a head over 64 KiB or a body over 1 MiB is 413; a
//! malformed request line, header, `Content-Length` or an unsupported version is 400; a client
//! that has not sent its whole request within 5 seconds is 408. The answer is read once the
//! guest's `handle` returned (no streaming): no response set is 500, an `error-code` set is 502
//! (its name in the body), and a guest trap (panic) is 500 too, after which the instance is
//! created anew and the server keeps going. Instance state otherwise lasts across requests.
//!
//! Exit codes: 2 when the build fails or the component does not lift
//! `wasi:http/incoming-handler#handle` directly, at any `0.2.x` patch version (or the address cannot be bound). The
//! build goes in `$WASIP2_WORK_DIR` (default: a temporary directory). Outgoing `wasi:http`
//! calls are not implemented by this program's host (no `HttpClient`) and the network stays
//! loopback-only, as in `wasip2_run`. See `docs/wasi-http-design.md`.

mod common;

const HANDLER_INTERFACE: &str = "wasi:http/incoming-handler@0.2.0";

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("wasip2_serve: {message}");
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| {
        fail("usage: wasip2_serve <component.wasm> [--listen ADDR] [--body-timeout SECS]")
    });
    let server_args: Vec<String> = args.collect();
    let dir = std::env::var_os("WASIP2_WORK_DIR").map_or_else(
        || std::env::temp_dir().join(format!("wasm2rs-wasip2-serve-{}", std::process::id())),
        Into::into,
    );
    std::fs::create_dir_all(&dir).expect("create work dir");

    let component = std::fs::read(&input).expect("read component");
    let (generated, interface) =
        common::transpile_exporting(&component, HANDLER_INTERFACE, "handle")
            .unwrap_or_else(|err| fail(err));
    // `server.rs` names the method by version; a toolchain may export a newer patch (`@0.2.12`).
    let version = interface
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .replace(['.', '-'], "_");
    let server = include_str!("wasip2_serve/server.rs").replace(
        "export_wasi_http_incoming_handler_0_2_0_handle",
        &format!("export_wasi_http_incoming_handler_{version}_handle"),
    );
    let source = format!("{generated}\n{server}\n");
    let Some(bin) = common::build_program(&dir, &source) else {
        fail(
            "rustc failed. If the errors above are E0407 (not a member of trait `Host`), the \
             component imports functions outside the WASI 0.2.0 surface that wasm2rs-wasi \
             implements.",
        );
    };
    let mut command = std::process::Command::new(&bin);
    command.args(&server_args);
    // Become the server, so that stopping this process stops it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = command.exec();
        fail(format!("cannot run the program: {err}"));
    }
    #[cfg(not(unix))]
    std::process::exit(command.status().expect("run program").code().unwrap_or(1));
}
