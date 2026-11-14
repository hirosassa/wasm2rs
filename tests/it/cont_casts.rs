//! `ref.test` / `ref.cast` / `br_on_cast` against a concrete continuation type are
//! invalid wasm ("cannot cast to a continuation type"), so validation rejects them
//! before code generation, which therefore has no lowering for them.

#[test]
fn a_cast_to_a_continuation_type_is_rejected_by_validation() {
    for body in [
        "(drop (ref.test (ref $ct) (local.get 0))) (i32.const 0)",
        "(drop (ref.cast (ref $ct) (local.get 0))) (i32.const 0)",
        "(block $b (result (ref $ct))
           (br_on_cast $b (ref null cont) (ref $ct) (local.get 0))
           drop
           (return (i32.const 0)))
         drop
         i32.const 1",
    ] {
        let wat = format!(
            r#"(module
                 (type $ft (func))
                 (type $ct (cont $ft))
                 (func (param (ref null cont)) (result i32) {body}))"#
        );
        let wasm = wat::parse_str(&wat).expect("wat should assemble");
        assert!(
            matches!(
                wasm2rs::transpile(&wasm),
                Err(wasm2rs::TranspileError::Parse(_))
            ),
            "not rejected: {body}"
        );
    }
}
