(module
  (type (;0;) (func (result i32)))
  (type (;1;) (func (param i32)))
  (type (;2;) (func (param i32 i32)))
  (type (;3;) (func))
  (type (;4;) (func (param i32) (result i32)))
  (table (;0;) 2 2 funcref)
  (memory (;0;) 1)
  (global (;0;) (mut i32) i32.const 16)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "main" (func $main_export))
  (export "memory" (memory 0))
  (export "conduit_reset" (func 8))
  (elem (;0;) (table 0) (i32.const 0) func 5 6)
  (func $main (;0;) (type 0) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 3
    local.get 3
    i32.const 0
    i32.store offset=8
    local.get 3
    local.set 0
    i32.const 8
    call $alloc
    local.set 4
    local.get 4
    i32.const 1
    i32.store offset=4
    local.get 4
    local.set 1
    global.get 1
    local.set 5
    i32.const 24
    call $alloc
    local.set 6
    local.get 6
    i32.const 1
    i32.store offset=8
    local.get 6
    local.get 5
    i32.store offset=12
    local.get 6
    i32.const 0
    i32.store offset=16
    local.get 6
    local.get 0
    i32.store offset=20
    local.get 6
    global.set 1
    local.get 1
    local.get 1
    i32.load offset=4
    call_indirect (type 1)
    local.get 5
    global.set 1
    local.get 0
    call $done
    local.get 0
    i32.load offset=8
    local.set 2
    local.get 2
    return
  )
  (func $add_yield (;1;) (type 2) (param i32 i32)
    (local i32 i32 i32)
    local.get 0
    i32.load offset=8
    local.set 3
    local.get 3
    local.get 1
    i32.add
    local.set 4
    local.get 0
    local.get 4
    i32.store offset=8
    return
  )
  (func $done (;2;) (type 1) (param i32)
    return
  )
  (func $produce (;3;) (type 3)
    (local i32 i32 i32 i32 i32 i32)
    global.get 1
    local.tee 0
    local.set 1
    block ;; label = @1
      loop ;; label = @2
        local.get 1
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 1
        i32.load offset=8
        i32.const 1
        i32.eq
        br_if 1 (;@1;)
        local.get 1
        i32.load offset=12
        local.set 1
        br 0 (;@2;)
      end
    end
    local.get 1
    i32.load offset=12
    global.set 1
    local.get 1
    i32.const 1
    local.get 1
    i32.load offset=16
    call_indirect (type 2)
    local.get 0
    global.set 1
    global.get 1
    local.tee 2
    local.set 3
    block ;; label = @1
      loop ;; label = @2
        local.get 3
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 3
        i32.load offset=8
        i32.const 1
        i32.eq
        br_if 1 (;@1;)
        local.get 3
        i32.load offset=12
        local.set 3
        br 0 (;@2;)
      end
    end
    local.get 3
    i32.load offset=12
    global.set 1
    local.get 3
    i32.const 2
    local.get 3
    i32.load offset=16
    call_indirect (type 2)
    local.get 2
    global.set 1
    global.get 1
    local.tee 4
    local.set 5
    block ;; label = @1
      loop ;; label = @2
        local.get 5
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 5
        i32.load offset=8
        i32.const 1
        i32.eq
        br_if 1 (;@1;)
        local.get 5
        i32.load offset=12
        local.set 5
        br 0 (;@2;)
      end
    end
    local.get 5
    i32.load offset=12
    global.set 1
    local.get 5
    i32.const 3
    local.get 5
    i32.load offset=16
    call_indirect (type 2)
    local.get 4
    global.set 1
    return
  )
  (func $alloc (;4;) (type 4) (param i32) (result i32)
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
  (func (;5;) (type 2) (param i32 i32)
    local.get 0
    i32.load offset=20
    local.get 1
    return_call $add_yield
  )
  (func (;6;) (type 1) (param i32)
    return_call $produce
  )
  (func $main_export (;7;) (type 0) (result i32)
    (local i32 i32)
    global.get 1
    local.set 0
    global.get 2
    local.set 1
    i32.const 0
    global.set 1
    call $main
    local.get 0
    global.set 1
    local.get 1
    global.set 2
  )
  (func (;8;) (type 3)
    i32.const 0
    global.set 1
    i32.const 0
    global.set 2
  )
)
