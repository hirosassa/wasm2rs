;; GC-heavy: builds a complete binary tree of structs (depth 16, 131071 nodes),
;; sums it, and drops it, `n` times. Dominated by struct.new / struct.get and
;; pointer chasing, so it exposes allocation count and indirection cost.
(module
  (type $node (struct (field i32) (field (ref null $node)) (field (ref null $node))))
  (func $build (param $d i32) (param $v i32) (result (ref null $node))
    (if (result (ref null $node)) (i32.eqz (local.get $d))
      (then (struct.new $node (local.get $v) (ref.null $node) (ref.null $node)))
      (else
        (struct.new $node (local.get $v)
          (call $build (i32.sub (local.get $d) (i32.const 1)) (i32.add (local.get $v) (i32.const 1)))
          (call $build (i32.sub (local.get $d) (i32.const 1)) (i32.add (local.get $v) (i32.const 2)))))))
  (func $sum (param $t (ref null $node)) (result i32)
    (if (result i32) (ref.is_null (local.get $t))
      (then (i32.const 0))
      (else
        (i32.add (struct.get $node 0 (local.get $t))
          (i32.add
            (call $sum (struct.get $node 1 (local.get $t)))
            (call $sum (struct.get $node 2 (local.get $t))))))))
  (func (export "run") (param $n i32) (result i32)
    (local $r i32) (local $s i32)
    (loop $rounds
      (local.set $s (i32.add (local.get $s) (call $sum (call $build (i32.const 16) (local.get $r)))))
      (local.set $r (i32.add (local.get $r) (i32.const 1)))
      (br_if $rounds (i32.lt_u (local.get $r) (local.get $n))))
    (local.get $s)))
