;; Same loop as `atomics.wat` on a `shared` memory (the threads proposal), where each
;; atomic access goes through `SharedMemory` instead of the plain load/store helpers.
(module
  (memory 1 1 shared)
  (global $base (mut i32) (i32.const 0))
  (func (export "run") (param $n i32) (result i32)
    (local $a i32) (local $i i32) (local $acc i32) (local $old i32) (local $t i32)
    (loop $step
      (local.set $a
        (i32.add
          (i32.and (i32.shl (local.get $i) (i32.const 2)) (i32.const 65532))
          (global.get $base)))
      (i32.atomic.store
        (local.get $a)
        (i32.xor (i32.mul (local.get $i) (i32.const 2654435761)) (local.get $acc)))
      (local.set $acc
        (i32.add
          (i32.mul (local.get $acc) (i32.const 31))
          (i32.atomic.load (local.get $a))))
      (local.set $t (local.get $acc))
      (local.set $old (i32.atomic.rmw.add (local.get $a) (local.get $t)))
      (local.set $acc (i32.xor (local.get $acc) (i32.add (local.get $old) (local.get $i))))
      (local.set $acc
        (i32.add
          (local.get $acc)
          (i32.atomic.rmw.cmpxchg
            (local.get $a)
            (i32.add (local.get $old) (local.get $t))
            (local.get $acc))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $step (i32.lt_u (local.get $i) (local.get $n))))
    (local.get $acc)))
