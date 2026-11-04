(module
  (rec
    (type (;0;) (sub (struct (field (ref null 1)))))
    (type (;1;) (func (param (ref null 0) i32) (result i32)))
    (type (;2;) (sub final 0 (struct (field (ref null 1)) (field i32))))
  )
  (type (;3;) (func (param i32 i32) (result i32)))
  (export "fn0" (func $adder))
  (export "fn1" (func $add))
  (elem (;0;) declare func 2)
  (func $adder (;0;) (type 3) (param $n i32) (param $x i32) (result i32)
    (local $sum i32)
    local.get $x
    local.get $n
    i32.add
    local.set $sum
    local.get $sum
    return
  )
  (func $add (;1;) (type 3) (param $n i32) (param $x i32) (result i32)
    (local $add_n (ref null 0)) (local $r i32)
    ref.func 2
    local.get $n
    struct.new 2
    local.set $add_n
    local.get $add_n
    local.get $x
    local.get $add_n
    struct.get 0 0
    return_call_ref 1
  )
  (func (;2;) (type 1) (param (ref null 0) i32) (result i32)
    (local (ref null 2))
    local.get 0
    ref.cast (ref null 2)
    local.set 2
    local.get 2
    struct.get 2 1
    local.get 1
    return_call $adder
  )
)
