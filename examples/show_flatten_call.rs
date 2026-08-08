fn main() {
    // Deeply nest a call inside `depth` blocks so the flattener has to thread a
    // call across the nested control flow. Building the wat programmatically keeps
    // the parentheses balanced.
    let depth = 44;
    let opens = "(block ".repeat(depth);
    let closes = ")".repeat(depth);
    let wat = format!(
        r#"
(module
  (func $f (param i32) (result i32) (local.get 0))
  (func (export "main") (result i32)
    (local $i i32)
    (local.set $i (i32.const 0))
    {opens}
      (local.set $i (call $f (local.get $i)))
    {closes}
    (local.get $i)
  )
)
    "#
    );
    let wasm = wat::parse_str(&wat).expect("valid wat");
    let source = wasm2rs::transpile(&wasm).expect("transpile ok");
    println!("{}", source);
}
