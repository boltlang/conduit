(module
  (type (;0;) (func (param i32 i32) (result i32)))
  (type (;1;) (func (param i32) (result i32)))
  (table (;0;) 0 0 funcref)
  (memory (;0;) 1)
  (global (;0;) (mut i32) i32.const 16)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "count" (func $count))
  (export "memory" (memory 0))
  (func $count (;0;) (type 0) (param i32 i32) (result i32)
    (local i32 i32 i32)
    block ;; label = @1
      block ;; label = @2
        local.get 0
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 1
      return
    end
    local.get 0
    i32.const 1
    i32.sub
    local.set 2
    local.get 1
    i32.const 2
    i32.add
    local.set 3
    local.get 2
    local.get 3
    return_call $count
  )
  (func $alloc (;1;) (type 1) (param i32) (result i32)
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
)
