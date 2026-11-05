(module
  (type (;0;) (func (result i32)))
  (type (;1;) (func (param i32) (result i32)))
  (type (;2;) (func (result f64)))
  (type (;3;) (func (result i64)))
  (type (;4;) (func (result f32)))
  (table (;0;) 0 0 funcref)
  (memory (;0;) 1)
  (global (;0;) (mut i32) i32.const 224)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "fn0" (func $hello_len))
  (export "fn1" (func $sum))
  (export "fn2" (func $sum_list))
  (export "fn3" (func $named_parts))
  (export "fn4" (func $cell_second))
  (export "fn5" (func $the_answer))
  (export "fn6" (func $the_nan))
  (export "fn7" (func $grid_parts))
  (export "fn8" (func $clone_and_write))
  (export "fn9" (func $bytes_sum))
  (export "fn10" (func $as_ro))
  (export "memory" (memory 0))
  (func $hello_len (;0;) (type 0) (result i32)
    (local i32 i32 i32 i32 i32 i32)
    i32.const 16
    local.set 0
    local.get 0
    i32.load offset=4
    local.set 1
    i32.const 4
    local.get 0
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 0
    i32.const 4
    i32.add
    i32.load8_s offset=8
    local.set 2
    local.get 2
    i32.const 255
    i32.and
    local.set 3
    local.get 1
    i32.const 1000
    i32.mul
    local.set 4
    local.get 4
    local.get 3
    i32.add
    local.set 5
    local.get 5
    return
  )
  (func $sum (;1;) (type 1) (param i32) (result i32)
    (local i32 i32 i32 i32)
    block ;; label = @1
      block ;; label = @2
        local.get 0
        i32.load offset=4
        br_table 0 (;@2;) 1 (;@1;) 0 (;@2;)
      end
      i32.const 0
      return
    end
    local.get 0
    i32.load offset=8
    local.set 1
    local.get 0
    i32.load offset=12
    local.set 2
    local.get 2
    call $sum
    local.set 3
    local.get 1
    local.get 3
    i32.add
    local.set 4
    local.get 4
    return
  )
  (func $sum_list (;2;) (type 0) (result i32)
    (local i32 i32)
    i32.const 88
    local.set 0
    local.get 0
    return_call $sum
  )
  (func $named_parts (;3;) (type 2) (result f64)
    (local i32 i32 i32 f64 f64 f64)
    i32.const 104
    local.set 0
    local.get 0
    i32.load offset=8
    local.set 1
    i32.const 0
    local.get 1
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 1
    i32.const 0
    i32.add
    i32.load8_s offset=8
    local.set 2
    local.get 0
    f64.load offset=16
    local.set 3
    local.get 2
    i32.const 255
    i32.and
    f64.convert_i32_u
    local.set 4
    local.get 4
    local.get 3
    f64.add
    local.set 5
    local.get 5
    return
  )
  (func $cell_second (;4;) (type 0) (result i32)
    (local i32 i32 i32)
    i32.const 144
    local.set 0
    local.get 0
    i32.load offset=8
    local.set 1
    i32.const 1
    local.get 1
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 1
    i32.const 1
    i32.const 2
    i32.shl
    i32.add
    i32.load offset=8
    local.set 2
    local.get 2
    return
  )
  (func $the_answer (;5;) (type 3) (result i64)
    (local i64)
    i64.const 42
    local.set 0
    local.get 0
    return
  )
  (func $the_nan (;6;) (type 4) (result f32)
    (local f32)
    f32.const nan (;=NaN;)
    local.set 0
    local.get 0
    return
  )
  (func $grid_parts (;7;) (type 0) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 200
    local.set 0
    i32.const 0
    local.get 0
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 0
    i32.const 0
    i32.const 2
    i32.shl
    i32.add
    i32.load offset=8
    local.set 1
    i32.const 1
    local.get 1
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 1
    i32.const 1
    i32.const 2
    i32.shl
    i32.add
    i32.load offset=8
    local.set 2
    i32.const 2
    local.get 0
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 0
    i32.const 2
    i32.const 2
    i32.shl
    i32.add
    i32.load offset=8
    local.set 3
    local.get 3
    i32.load offset=4
    local.set 4
    local.get 0
    i32.load offset=4
    local.set 5
    local.get 2
    i32.const 100
    i32.mul
    local.set 6
    local.get 5
    i32.const 10
    i32.mul
    local.set 7
    local.get 6
    local.get 7
    i32.add
    local.set 8
    local.get 8
    local.get 4
    i32.add
    local.set 9
    local.get 9
    return
  )
  (func $clone_and_write (;8;) (type 0) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    local.set 0
    local.get 0
    i32.load offset=4
    local.set 5
    local.get 5
    i32.const 2147418112
    i32.gt_u
    if ;; label = @1
      unreachable
    end
    local.get 5
    i32.const 8
    i32.add
    call $alloc
    local.tee 6
    local.get 5
    i32.store offset=4
    local.get 6
    i32.const 8
    i32.add
    local.get 0
    i32.const 8
    i32.add
    local.get 5
    memory.copy
    local.get 6
    local.set 1
    i32.const 0
    local.get 1
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 1
    i32.const 0
    i32.add
    i32.const 0
    i32.store8 offset=8
    i32.const 16
    local.set 2
    i32.const 0
    local.get 2
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 2
    i32.const 0
    i32.add
    i32.load8_s offset=8
    local.set 3
    local.get 3
    i32.const 255
    i32.and
    local.set 4
    local.get 4
    return
  )
  (func $bytes_sum (;9;) (type 0) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 32
    local.set 0
    local.get 0
    i32.load offset=4
    local.set 1
    i32.const 0
    local.get 0
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 0
    i32.const 0
    i32.add
    i32.load8_s offset=8
    local.set 2
    i32.const 1
    local.get 0
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 0
    i32.const 1
    i32.add
    i32.load8_s offset=8
    local.set 3
    i32.const 2
    local.get 0
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 0
    i32.const 2
    i32.add
    i32.load8_s offset=8
    local.set 4
    i32.const 4
    local.get 0
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 0
    i32.const 4
    i32.add
    i32.load8_s offset=8
    local.set 5
    local.get 2
    i32.const 255
    i32.and
    local.set 6
    local.get 3
    i32.const 255
    i32.and
    local.set 7
    local.get 4
    i32.const 255
    i32.and
    local.set 8
    local.get 5
    i32.const 255
    i32.and
    local.set 9
    local.get 6
    local.get 7
    i32.add
    local.set 10
    local.get 10
    local.get 8
    i32.add
    local.set 11
    local.get 11
    local.get 9
    i32.add
    local.set 12
    local.get 1
    i32.const 1000
    i32.mul
    local.set 13
    local.get 13
    local.get 12
    i32.add
    local.set 14
    local.get 14
    return
  )
  (func $as_ro (;10;) (type 0) (result i32)
    (local i32 i32 i32 i32 i32 i32)
    i32.const 16
    local.set 0
    local.get 0
    i32.load offset=4
    local.set 4
    local.get 4
    i32.const 2147418112
    i32.gt_u
    if ;; label = @1
      unreachable
    end
    local.get 4
    i32.const 8
    i32.add
    call $alloc
    local.tee 5
    local.get 4
    i32.store offset=4
    local.get 5
    i32.const 8
    i32.add
    local.get 0
    i32.const 8
    i32.add
    local.get 4
    memory.copy
    local.get 5
    local.set 1
    i32.const 1
    local.get 1
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 1
    i32.const 1
    i32.add
    i32.load8_s offset=8
    local.set 2
    local.get 2
    i32.const 255
    i32.and
    local.set 3
    local.get 3
    return
  )
  (func $alloc (;11;) (type 1) (param i32) (result i32)
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
  (data (;0;) (i32.const 16) "\00\00\00\00\05\00\00\00hello\00\00\00\00\00\00\00\05\00\00\00\22\ff\00\c3\a9\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\01\00\00\00\03\00\00\000\00\00\00\00\00\00\00\01\00\00\00\02\00\00\008\00\00\00\00\00\00\00\01\00\00\00\01\00\00\00H\00\00\00\00\00\00\00\00\00\00\00\10\00\00\00\00\00\00\00\00\00\00\00\00\00\04@\00\00\00\00\02\00\00\00\07\00\00\00\08\00\00\00\00\00\00\00\00\00\00\00\80\00\00\00\00\00\00\00\00\00\00\00\02\00\00\00\01\00\00\00\02\00\00\00\00\00\00\00\01\00\00\00\03\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\00\03\00\00\00\a0\00\00\00\b0\00\00\00\c0\00\00\00")
)
