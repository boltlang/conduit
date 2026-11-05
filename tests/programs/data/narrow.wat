(module
  (type (;0;) (func (param i32) (result i32 i32 i64 i64 i32 i32)))
  (type (;1;) (func (param i64) (result i32 i32 i32)))
  (type (;2;) (func (param f64) (result i32)))
  (type (;3;) (func (param f32) (result i32)))
  (type (;4;) (func (param i32) (result f64 f32)))
  (type (;5;) (func (param i32) (result i32)))
  (type (;6;) (func (param i32) (result i32 i32)))
  (type (;7;) (func (param i32 i32) (result i32)))
  (table (;0;) 0 0 funcref)
  (memory (;0;) 1)
  (global (;0;) (mut i32) i32.const 32)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "fn0" (func $widen))
  (export "fn1" (func $narrow))
  (export "fn2" (func $f64_to_i8))
  (export "fn3" (func $f64_to_u8))
  (export "fn4" (func $f32_to_i16))
  (export "fn5" (func $to_floats))
  (export "fn6" (func $case_i8))
  (export "fn7" (func $case_i16))
  (export "fn8" (func $boxed))
  (export "fn9" (func $array_i8))
  (export "fn10" (func $array_i16))
  (export "fn11" (func $fields))
  (export "fn12" (func $set_field))
  (export "fn13" (func $tagged))
  (export "memory" (memory 0))
  (func $widen (;0;) (type 0) (param i32) (result i32 i32 i64 i64 i32 i32)
    (local i32 i32 i32 i32 i64 i64 i32 i32)
    local.get 0
    i32.const -128
    i32.add
    local.set 1
    local.get 1
    i32.const -128
    i32.add
    local.set 2
    local.get 2
    i32.extend8_s
    local.set 3
    local.get 2
    i32.const 255
    i32.and
    local.set 4
    local.get 2
    i32.extend8_s
    i64.extend_i32_s
    local.set 5
    local.get 2
    i32.const 255
    i32.and
    i64.extend_i32_u
    local.set 6
    local.get 2
    i32.extend8_s
    local.set 7
    local.get 2
    i32.const 255
    i32.and
    local.set 8
    local.get 3
    local.get 4
    local.get 5
    local.get 6
    local.get 7
    local.get 8
    return
  )
  (func $narrow (;1;) (type 1) (param i64) (result i32 i32 i32)
    (local i32 i32 i32)
    local.get 0
    i32.wrap_i64
    local.set 1
    local.get 0
    i32.wrap_i64
    local.set 2
    local.get 2
    local.set 3
    local.get 1
    local.get 2
    local.get 3
    return
  )
  (func $f64_to_i8 (;2;) (type 2) (param f64) (result i32)
    (local i32 i32)
    local.get 0
    i32.trunc_f64_s
    local.tee 2
    i32.const 128
    i32.add
    i32.const 256
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 2
    local.set 1
    local.get 1
    return
  )
  (func $f64_to_u8 (;3;) (type 2) (param f64) (result i32)
    (local i32 i32)
    local.get 0
    i32.trunc_f64_u
    local.tee 2
    i32.const 256
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 2
    local.set 1
    local.get 1
    return
  )
  (func $f32_to_i16 (;4;) (type 3) (param f32) (result i32)
    (local i32 i32)
    local.get 0
    i32.trunc_f32_s
    local.tee 2
    i32.const 32768
    i32.add
    i32.const 65536
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 2
    local.set 1
    local.get 1
    return
  )
  (func $to_floats (;5;) (type 4) (param i32) (result f64 f32)
    (local i32 i32 f64 f32)
    local.get 0
    i32.const -32768
    i32.add
    local.set 1
    local.get 1
    i32.const -32768
    i32.add
    local.set 2
    local.get 2
    i32.extend16_s
    f64.convert_i32_s
    local.set 3
    local.get 2
    i32.const 65535
    i32.and
    f32.convert_i32_u
    local.set 4
    local.get 3
    local.get 4
    return
  )
  (func $case_i8 (;6;) (type 5) (param i32) (result i32)
    (local i32 i32)
    local.get 0
    i32.const -128
    i32.add
    local.set 1
    local.get 1
    i32.const -128
    i32.add
    local.set 2
    block ;; label = @1
      block ;; label = @2
        block ;; label = @3
          block ;; label = @4
            local.get 2
            i32.extend8_s
            i32.const -128
            i32.eq
            br_if 0 (;@4;)
            local.get 2
            i32.extend8_s
            i32.const -1
            i32.eq
            br_if 1 (;@3;)
            local.get 2
            i32.extend8_s
            i32.const 127
            i32.eq
            br_if 2 (;@2;)
            br 3 (;@1;)
          end
          i32.const 1
          return
        end
        i32.const 2
        return
      end
      i32.const 3
      return
    end
    i32.const 0
    return
  )
  (func $case_i16 (;7;) (type 5) (param i32) (result i32)
    (local i32 i32)
    local.get 0
    i32.const -32768
    i32.add
    local.set 1
    local.get 1
    i32.const -32768
    i32.add
    local.set 2
    block ;; label = @1
      block ;; label = @2
        block ;; label = @3
          block ;; label = @4
            block ;; label = @5
              block ;; label = @6
                local.get 2
                i32.extend16_s
                i32.const -2
                i32.sub
                br_table 0 (;@6;) 1 (;@5;) 2 (;@4;) 3 (;@3;) 4 (;@2;) 5 (;@1;)
              end
              i32.const 10
              return
            end
            i32.const 11
            return
          end
          i32.const 12
          return
        end
        i32.const 13
        return
      end
      i32.const 14
      return
    end
    i32.const 0
    return
  )
  (func $boxed (;8;) (type 5) (param i32) (result i32)
    (local i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 3
    local.get 3
    local.get 0
    i32.store8 offset=8
    local.get 3
    local.set 1
    local.get 1
    i32.load8_s offset=8
    local.set 2
    local.get 2
    return
  )
  (func $array_i8 (;9;) (type 6) (param i32) (result i32 i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    local.get 0
    i32.const -128
    i32.add
    local.set 1
    local.get 1
    i32.const -128
    i32.add
    local.set 2
    i32.const 2
    local.set 7
    local.get 7
    i32.const 2147418112
    i32.gt_u
    if ;; label = @1
      unreachable
    end
    local.get 7
    i32.const 8
    i32.add
    call $alloc
    local.tee 8
    local.get 7
    i32.store offset=4
    block ;; label = @1
      loop ;; label = @2
        local.get 7
        i32.eqz
        br_if 1 (;@1;)
        local.get 7
        i32.const 1
        i32.sub
        local.tee 7
        local.get 8
        i32.add
        i32.const 0
        i32.store8 offset=8
        br 0 (;@2;)
      end
    end
    local.get 8
    local.set 3
    i32.const 1
    local.get 3
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 3
    i32.const 1
    i32.add
    local.get 2
    i32.store8 offset=8
    local.get 3
    i32.load offset=4
    local.set 9
    local.get 9
    i32.const 2147418112
    i32.gt_u
    if ;; label = @1
      unreachable
    end
    local.get 9
    i32.const 8
    i32.add
    call $alloc
    local.tee 10
    local.get 9
    i32.store offset=4
    local.get 10
    i32.const 8
    i32.add
    local.get 3
    i32.const 8
    i32.add
    local.get 9
    memory.copy
    local.get 10
    local.set 4
    i32.const 1
    local.get 4
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 4
    i32.const 1
    i32.add
    i32.load8_s offset=8
    local.set 5
    local.get 5
    i32.const 255
    i32.and
    local.set 6
    local.get 5
    local.get 6
    return
  )
  (func $array_i16 (;10;) (type 5) (param i32) (result i32)
    (local i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 5
    local.get 5
    i32.const 2
    i32.store offset=4
    local.get 5
    local.get 0
    i32.store16 offset=8
    local.get 5
    i32.const 255
    i32.store16 offset=10
    local.get 5
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
    i32.const 1
    i32.shl
    i32.add
    i32.load16_s offset=8
    local.set 2
    i32.const 1
    local.get 1
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 1
    i32.const 1
    i32.const 1
    i32.shl
    i32.add
    i32.load16_s offset=8
    local.set 3
    local.get 2
    local.get 3
    i32.add
    local.set 4
    local.get 4
    return
  )
  (func $fields (;11;) (type 7) (param i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 7
    local.get 7
    i32.const 0
    i32.store offset=4
    local.get 7
    local.get 0
    i32.store8 offset=8
    local.get 7
    local.get 1
    i32.store16 offset=10
    local.get 7
    local.set 2
    local.get 2
    i32.load8_s offset=8
    local.set 3
    local.get 2
    i32.load16_s offset=10
    local.set 4
    local.get 3
    i32.extend8_s
    local.set 5
    local.get 5
    i32.extend16_s
    local.get 4
    i32.extend16_s
    i32.lt_s
    local.set 6
    local.get 6
    return
  )
  (func $set_field (;12;) (type 6) (param i32) (result i32 i32)
    (local i32 i32 i32 i32 i32 i32)
    local.get 0
    i32.const 1
    i32.add
    local.set 1
    i32.const 16
    call $alloc
    local.set 6
    local.get 6
    i32.const 0
    i32.store offset=4
    local.get 6
    i32.const 0
    i32.store8 offset=8
    local.get 6
    i32.const 0
    i32.store16 offset=10
    local.get 6
    local.set 2
    local.get 2
    local.get 1
    i32.store8 offset=8
    local.get 2
    i32.load8_s offset=8
    local.set 3
    local.get 3
    i32.const 255
    i32.and
    local.set 4
    local.get 3
    i32.extend8_s
    local.set 5
    local.get 4
    local.get 5
    return
  )
  (func $tagged (;13;) (type 5) (param i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 13
    local.get 13
    i32.const 1
    i32.store offset=4
    local.get 13
    i32.const 1000
    i32.store offset=8
    local.get 13
    local.get 0
    i32.store8 offset=12
    local.get 13
    local.set 1
    i32.const 16
    local.set 2
    block ;; label = @1
      block ;; label = @2
        local.get 1
        i32.load offset=4
        br_table 0 (;@2;) 1 (;@1;) 0 (;@2;)
      end
      i32.const 0
      return
    end
    local.get 1
    i32.load8_s offset=12
    local.set 3
    local.get 1
    i32.load offset=8
    local.set 4
    local.get 2
    i32.load8_s offset=8
    local.set 5
    local.get 2
    i32.load16_s offset=10
    local.set 6
    local.get 3
    i32.extend8_s
    local.set 7
    local.get 5
    i32.const 255
    i32.and
    local.set 8
    local.get 6
    i32.extend16_s
    local.set 9
    local.get 4
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
    local.get 12
    return
  )
  (func $alloc (;14;) (type 5) (param i32) (result i32)
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
  (data (;0;) (i32.const 16) "\00\00\00\00\00\00\00\00\ff\00\fe\ff\00\00\00\00")
)
