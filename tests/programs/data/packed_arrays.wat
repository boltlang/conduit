(module
  (rec
    (type (;0;) (array (mut i8)))
    (type (;1;) (array (mut i16)))
    (type (;2;) (struct (field (mut (ref null 0)))))
  )
  (type (;3;) (func (param i32) (result i32)))
  (export "fn0" (func 0))
  (export "fn1" (func 1))
  (export "fn2" (func 2))
  (export "fn3" (func 3))
  (export "fn4" (func 4))
  (export "fn5" (func 5))
  (func (;0;) (type 3) (param i32) (result i32)
    (local (ref null 0) i32)
    local.get 0
    i32.const 1
    array.new 0
    local.set 1
    local.get 1
    i32.const 0
    array.get_u 0
    local.set 2
    local.get 2
    return
  )
  (func (;1;) (type 3) (param i32) (result i32)
    (local (ref null 0) i32)
    local.get 0
    array.new_fixed 0 1
    local.set 1
    local.get 1
    i32.const 0
    array.get_s 0
    local.set 2
    local.get 2
    return
  )
  (func (;2;) (type 3) (param i32) (result i32)
    (local (ref null 1) i32)
    i32.const 0
    i32.const 2
    array.new 1
    local.set 1
    local.get 1
    i32.const 1
    local.get 0
    array.set 1
    local.get 1
    i32.const 1
    array.get_u 1
    local.set 2
    local.get 2
    return
  )
  (func (;3;) (type 3) (param i32) (result i32)
    (local (ref null 1) i32)
    i32.const 0
    local.get 0
    array.new_fixed 1 2
    local.set 1
    local.get 1
    i32.const 1
    array.get_s 1
    local.set 2
    local.get 2
    return
  )
  (func (;4;) (type 3) (param i32) (result i32)
    (local (ref null 0) (ref null 0) i32 i32 i32 (ref null 0))
    local.get 0
    i32.const 3
    array.new 0
    local.set 1
    local.get 1
    array.len
    array.new_default 0
    local.tee 6
    i32.const 0
    local.get 1
    i32.const 0
    local.get 1
    array.len
    array.copy 0 0
    local.get 6
    local.set 2
    local.get 1
    i32.const 0
    i32.const 1
    array.set 0
    local.get 2
    i32.const 0
    array.get_u 0
    local.set 3
    local.get 2
    array.len
    local.set 4
    local.get 3
    local.get 4
    i32.add
    local.set 5
    local.get 5
    return
  )
  (func (;5;) (type 3) (param i32) (result i32)
    (local (ref null 0) (ref null 2) (ref null 0) i32)
    i32.const 104
    local.get 0
    array.new_fixed 0 2
    local.set 1
    local.get 1
    struct.new 2
    local.set 2
    local.get 2
    struct.get 2 0
    local.set 3
    local.get 3
    i32.const 1
    array.get_u 0
    local.set 4
    local.get 4
    return
  )
)
