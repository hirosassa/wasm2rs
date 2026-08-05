//! A host embedding the generated `Instance` must be able to marshal bytes into
//! and out of the module's linear memory (e.g. to write an RPC request buffer
//! and read the response). This pins a public `memory()` accessor: the host
//! writes a value, the wasm reads it back, and the host observes a wasm store.

mod common;

#[test]
fn host_can_read_and_write_linear_memory() {
    let wat = r#"
        (module
          (memory 1)
          ;; func0: load an i32 from the given address
          (func (param i32) (result i32)
            local.get 0
            i32.load)
          ;; func1: store `val` at `addr`
          (func (param i32 i32)
            local.get 0
            local.get 1
            i32.store))
    "#;

    let main_body = r#"
        let mut inst = Instance::new();

        // Host writes 42 at offset 16 directly into linear memory.
        inst.memory()[16..20].copy_from_slice(&42i32.to_le_bytes());
        // wasm sees the host's write.
        assert_eq!(inst.func0(16), 42);

        // wasm stores 99 at offset 32.
        inst.func1(32, 99);
        // Host reads back the wasm store through the same accessor.
        assert_eq!(&inst.memory()[32..36], &99i32.to_le_bytes());
    "#;

    common::compile_run("host_memory_access", wat, main_body);
}
