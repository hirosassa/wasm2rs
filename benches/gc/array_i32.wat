;; GC-heavy: one 1M-element (mut i32) array, filled and summed `n` times.
;; Dominated by array.get/array.set on a primitive element type, so it
;; exposes the per-element slot size. Returns the checksum.
(module
  (type $arr (array (mut i32)))
  (func (export "run") (param $n i32) (result i32)
    (local $a (ref null $arr)) (local $i i32) (local $r i32) (local $s i32)
    (local.set $a (array.new_default $arr (i32.const 1048576)))
    (loop $rounds
      (local.set $i (i32.const 0))
      (loop $fill
        (array.set $arr (local.get $a) (local.get $i)
          (i32.add (i32.mul (local.get $i) (i32.const 7)) (local.get $r)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $fill (i32.lt_u (local.get $i) (i32.const 1048576))))
      (local.set $i (i32.const 0))
      (loop $sum
        (local.set $s (i32.add (local.get $s) (array.get $arr (local.get $a) (local.get $i))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $sum (i32.lt_u (local.get $i) (i32.const 1048576))))
      (local.set $r (i32.add (local.get $r) (i32.const 1)))
      (br_if $rounds (i32.lt_u (local.get $r) (local.get $n))))
    (local.get $s)))
