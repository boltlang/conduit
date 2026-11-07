(module
  (type (;0;) (func (param i32 i32) (result i32)))
  (type (;1;) (func (param i32) (result i32)))
  (table (;0;) 1 1 funcref)
  (memory (;0;) 1)
  (global (;0;) (mut i32) i32.const 16)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "fn1" (func $add))
  (export "memory" (memory 0))
  (elem (;0;) (table 0) (i32.const 0) func 3)
  (func $adder (;0;) (type 0) (param i32 i32) (result i32)
    (local i32)
    local.get 1
    local.get 0
    i32.add
    local.set 2
    local.get 2
    return
  )
  (func $add (;1;) (type 0) (param i32 i32) (result i32)
    (local i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 4
    local.get 4
    i32.const 0
    i32.store offset=4
    local.get 4
    local.get 0
    i32.store offset=8
    local.get 4
    local.set 2
    local.get 2
    local.get 1
    local.get 2
    i32.load offset=4
    return_call_indirect (type 0)
  )
  (func $alloc (;2;) (type 1) (param i32) (result i32)
    (local i32 i32)
    local.get 0
    i32.const 7
    i32.add
    i32.const -8
    i32.and
    local.set 0
    global.get 0
    local.tee 1
    local.get 0
    i32.add
    local.tee 2
    local.get 1
    i32.lt_u
    if ;; label = @1
      unreachable
    end
    local.get 2
    memory.size
    i32.const 16
    i32.shl
    i32.gt_u
    if ;; label = @1
      local.get 2
      memory.size
      i32.const 16
      i32.shl
      i32.sub
      i32.const 65535
      i32.add
      i32.const 16
      i32.shr_u
      memory.grow
      i32.const -1
      i32.eq
      if ;; label = @2
        unreachable
      end
    end
    local.get 2
    global.set 0
    local.get 1
  )
  (func (;3;) (type 0) (param i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 1
    return_call $adder
  )
)
