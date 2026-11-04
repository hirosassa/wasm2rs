;; FNV-1a-style hash over a 64 KiB buffer, `run(n)` rounds (n >= 1).
;; Dominated by byte loads. Returns the hash.
(module
  (memory 1)
  (func (export "run") (param $n i32) (result i32)
    (local $i i32) (local $h i32) (local $r i32)
    (loop $fill
      (i32.store8 (local.get $i) (i32.mul (local.get $i) (i32.const 31)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $fill (i32.lt_u (local.get $i) (i32.const 65536))))
    (local.set $h (i32.const -2128831035))
    (loop $rounds
      (local.set $i (i32.const 0))
      (loop $bytes
        (local.set $h
          (i32.mul
            (i32.xor (local.get $h) (i32.load8_u (local.get $i)))
            (i32.const 16777619)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $bytes (i32.lt_u (local.get $i) (i32.const 65536))))
      (local.set $r (i32.add (local.get $r) (i32.const 1)))
      (br_if $rounds (i32.lt_u (local.get $r) (local.get $n))))
    (local.get $h)))
