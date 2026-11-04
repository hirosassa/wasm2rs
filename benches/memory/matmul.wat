;; 64x64 i32 matrix product C = A * B, `run(n)` times (n >= 1). A at 0, B at
;; 16384, C at 32768. Two loads per inner step. Returns the sum of all C.
(module
  (memory 1)
  (func (export "run") (param $n i32) (result i32)
    (local $i i32) (local $j i32) (local $k i32) (local $r i32)
    (local $acc i32) (local $sum i32)
    (loop $init
      (i32.store (i32.shl (local.get $i) (i32.const 2))
        (i32.add (i32.rem_u (local.get $i) (i32.const 7)) (i32.const 1)))
      (i32.store offset=16384 (i32.shl (local.get $i) (i32.const 2))
        (i32.add (i32.rem_u (local.get $i) (i32.const 5)) (i32.const 1)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $init (i32.lt_u (local.get $i) (i32.const 4096))))
    (loop $rounds
      (local.set $i (i32.const 0))
      (loop $li
        (local.set $j (i32.const 0))
        (loop $lj
          (local.set $acc (i32.const 0))
          (local.set $k (i32.const 0))
          (loop $lk
            (local.set $acc
              (i32.add (local.get $acc)
                (i32.mul
                  (i32.load
                    (i32.shl
                      (i32.add (i32.shl (local.get $i) (i32.const 6)) (local.get $k))
                      (i32.const 2)))
                  (i32.load offset=16384
                    (i32.shl
                      (i32.add (i32.shl (local.get $k) (i32.const 6)) (local.get $j))
                      (i32.const 2))))))
            (local.set $k (i32.add (local.get $k) (i32.const 1)))
            (br_if $lk (i32.lt_u (local.get $k) (i32.const 64))))
          (i32.store offset=32768
            (i32.shl
              (i32.add (i32.shl (local.get $i) (i32.const 6)) (local.get $j))
              (i32.const 2))
            (local.get $acc))
          (local.set $sum (i32.add (local.get $sum) (local.get $acc)))
          (local.set $j (i32.add (local.get $j) (i32.const 1)))
          (br_if $lj (i32.lt_u (local.get $j) (i32.const 64))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $li (i32.lt_u (local.get $i) (i32.const 64))))
      (local.set $r (i32.add (local.get $r) (i32.const 1)))
      (br_if $rounds (i32.lt_u (local.get $r) (local.get $n))))
    (local.get $sum)))
