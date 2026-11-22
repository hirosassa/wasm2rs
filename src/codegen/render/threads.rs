//! Thread-spawn methods (`wasi` `thread-spawn` and the Emscripten `pthread_create` shim).

use super::Layout;

/// `wasi::thread-spawn(start_arg) -> tid`: allocate a thread id (1..=0x1fff_ffff,
/// negative when exhausted or the thread cannot start), then run a fresh sibling
/// instance over the same shared memory on a new thread, entering it through
/// `wasi_thread_start(tid, start_arg)`. The child is detached. A trap in it ends
/// the whole process with a nonzero status, as the proposal requires. The
/// descriptor table is shared: a child sees the descriptors its siblings opened.
pub(super) fn thread_spawn_lines(layout: &Layout, typed_trap_type: bool) -> Vec<String> {
    spawn_thread_lines(
        layout,
        typed_trap_type,
        "fn wasi_thread_spawn(&mut self, a0: i32) -> i32 {",
        &[
            "let tid = self.wasi_tid.fetch_add(1, std::sync::atomic::Ordering::SeqCst).wrapping_add(1);",
            "if !(1..=0x1fff_ffff).contains(&tid) { return -1; }",
        ],
        &["t.wasi_thread_start(tid, a0);"],
        "\"wasi-threads: thread {tid} trapped; terminating the process\"",
        ("tid", "-1"),
    )
}

/// `env.__pthread_create_js(pthread_ptr, attr, start, arg)`: what an emscripten
/// JS host answers by starting a Worker. Run a fresh sibling instance over the
/// same shared memory on a new thread and replay the Worker's start-up: set up
/// its stack (when the module exports the stack functions), init the
/// thread with its pthread pointer, init its TLS, call the table entry `start`
/// with `arg`, then exit the thread with the result. The child is detached; a
/// trap in it ends the whole process. Returns 0, or EAGAIN (6) if no thread could
/// be started.
pub(super) fn pthread_create_js_lines(layout: &Layout, typed_trap_type: bool) -> Vec<String> {
    let mut run = Vec::new();
    if layout.emscripten_stack {
        // A Worker's `establishStackSpace`, which must come first: until then
        // the fresh instance's `__stack_pointer` is the module's initial one,
        // the parent's live stack, and any call would clobber it. `stack` and
        // `stack_size` are at offsets 48 and 52 of the wasm32 `struct pthread`
        // (as in emscripten's own JS).
        run.extend([
            "let pthread = u64::from(a0 as u32);",
            "let stack_high = t.memory.atomic_load(pthread + 48, 4) as u32;",
            "let stack_size = t.memory.atomic_load(pthread + 52, 4) as u32;",
            "t.emscripten_stack_set_limits(stack_high as i32, stack_high.wrapping_sub(stack_size) as i32);",
            "t._emscripten_stack_restore(stack_high as i32);",
        ]);
    }
    run.extend([
        "t._emscripten_thread_init(a0, 0, 0, 1, 0, 0);",
        "t._emscripten_tls_init();",
        "let r = t.emscripten_call_start(a2 as u32, a3);",
        "t._emscripten_thread_exit(r);",
    ]);
    spawn_thread_lines(
        layout,
        typed_trap_type,
        "fn emscripten_pthread_create_js(&mut self, a0: i32, a1: i32, a2: i32, a3: i32) -> i32 {",
        &[],
        &run,
        "\"emscripten pthread trapped; terminating the process\"",
        ("0", "6"),
    )
}

/// The shared skeleton of a native thread-starting import: `prelude` runs on the
/// parent, then a detached thread builds a sibling instance (`Self::with_memory`
/// over the shared memory, sharing the tid counter, descriptor table and
/// preopen), runs `run` and, if it unwinds, prints `trap_msg` and exits 134.
/// `ok_err` is the (success, failure) value the import returns. With
/// `typed_trap_type` (the output defines `Wasm2RsTrap`) a typed payload, which
/// bypasses the panic hook, has its `Display` printed ahead of `trap_msg`.
fn spawn_thread_lines(
    layout: &Layout,
    typed_trap_type: bool,
    header: &str,
    prelude: &[&str],
    run: &[&str],
    trap_msg: &str,
    ok_err: (&str, &str),
) -> Vec<String> {
    let mut lines = vec![header.to_string()];
    lines.extend(prelude.iter().map(|l| format!("    {l}")));
    lines.extend(
        [
            "    let mem = self.memory.clone();",
            "    let tids = self.wasi_tid.clone();",
        ]
        .map(str::to_string),
    );
    if layout.has_imports {
        lines.push("    let imports = self.imports.clone();".to_string());
    }
    if layout.wasi_files {
        lines.push("    let fds = self.wasi_fds.clone();".to_string());
    }
    if layout.wasi_preopen {
        lines.push("    let preopen = self.wasi_preopen.clone();".to_string());
    }
    lines.push("    let started = std::thread::Builder::new().spawn(move || {".to_string());
    lines.push(if layout.has_imports {
        "        let mut t = Self::with_memory(mem, imports);".to_string()
    } else {
        "        let mut t = Self::with_memory(mem);".to_string()
    });
    lines.push("        t.wasi_tid = tids;".to_string());
    if layout.wasi_files {
        lines.push("        t.wasi_fds = fds;".to_string());
    }
    if layout.wasi_preopen {
        lines.push("        t.wasi_preopen = preopen;".to_string());
    }
    lines.push("        let run = std::panic::AssertUnwindSafe(|| {".to_string());
    lines.extend(run.iter().map(|l| format!("            {l}")));
    lines.push("        });".to_string());
    if typed_trap_type {
        lines.extend(
            [
                "        if let Err(payload) = std::panic::catch_unwind(run) {",
                "            if let Some(trap) = payload.downcast_ref::<Wasm2RsTrap>() {",
                "                eprintln!(\"{trap}\");",
                "            }",
            ]
            .map(str::to_string),
        );
    } else {
        lines.push("        if std::panic::catch_unwind(run).is_err() {".to_string());
    }
    lines.push(format!("            eprintln!({trap_msg});"));
    lines.extend(
        [
            "            std::process::exit(134);",
            "        }",
            "    });",
        ]
        .map(str::to_string),
    );
    lines.push(format!(
        "    if started.is_ok() {{ {} }} else {{ {} }}",
        ok_err.0, ok_err.1
    ));
    lines.push("}".to_string());
    lines
}
