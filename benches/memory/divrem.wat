;; `run(n)` steps (n >= 1) that divide an LCG value by a data-dependent odd
;; divisor (1..=65535, so never zero and never -1), unsigned and signed, and fold
;; quotient and remainder into a checksum. The divisor is opaque to the optimizer,
;; so the zero / overflow checks of `typed_traps` cannot be folded away. The memory
;; is unused: it only makes the module stateful so the harness has an `Instance`.
(module
  (memory 1)
  (func (export "run") (param $n i32) (result i32)
    (local $x i32) (local $d i32) (local $acc i32) (local $i i32)
    (local.set $x (i32.const 12345))
    (loop $step
      (local.set $x
        (i32.add (i32.mul (local.get $x) (i32.const 1664525)) (i32.const 1013904223)))
      (local.set $d (i32.or (i32.shr_u (local.get $x) (i32.const 16)) (i32.const 1)))
      (local.set $acc
        (i32.xor (local.get $acc)
          (i32.add (i32.div_u (local.get $x) (local.get $d))
                   (i32.rem_u (local.get $x) (local.get $d)))))
      (local.set $acc
        (i32.xor (local.get $acc)
          (i32.add (i32.div_s (local.get $x) (local.get $d))
                   (i32.rem_s (local.get $x) (local.get $d)))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $step (i32.lt_u (local.get $i) (local.get $n))))
    (local.get $acc)))
