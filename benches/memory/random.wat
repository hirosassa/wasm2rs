;; `run(n)` read-modify-write steps (n >= 1) at LCG-chosen word addresses in a
;; 64 KiB memory. Unpredictable addresses, one load and one store per step.
;; Returns the XOR of the loaded values.
(module
  (memory 1)
  (func (export "run") (param $n i32) (result i32)
    (local $x i32) (local $a i32) (local $v i32) (local $acc i32) (local $i i32)
    (local.set $x (i32.const 12345))
    (loop $step
      (local.set $x
        (i32.add (i32.mul (local.get $x) (i32.const 1664525)) (i32.const 1013904223)))
      (local.set $a
        (i32.and (i32.shr_u (local.get $x) (i32.const 8)) (i32.const 65532)))
      (local.set $v (i32.load (local.get $a)))
      (i32.store (local.get $a) (i32.add (local.get $v) (local.get $x)))
      (local.set $acc (i32.xor (local.get $acc) (local.get $v)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $step (i32.lt_u (local.get $i) (local.get $n))))
    (local.get $acc)))
