(module
  (type (;0;) (func (param i32) (result i32 i32 i64 i64 i32 i32)))
  (type (;1;) (func (param i64) (result i32 i32 i32)))
  (type (;2;) (func (param f64) (result i32)))
  (type (;3;) (func (param f32) (result i32)))
  (type (;4;) (func (param i32) (result f64 f32)))
  (type (;5;) (func (param i32) (result i32)))
  (type (;6;) (func (param i32) (result i32 i32)))
  (type (;7;) (func (param i32 i32) (result i32)))
  (type (;8;) (func (param i32)))
  (type (;9;) (func (param i32 i32)))
  (type (;10;) (func))
  (table (;0;) 0 0 funcref)
  (memory (;0;) 2)
  (global (;0;) (mut i32) i32.const 65568)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (global (;3;) (mut i32) i32.const 65568)
  (global (;4;) (mut i32) i32.const 32)
  (global (;5;) (mut i32) i32.const 0)
  (export "widen" (func $widen_export))
  (export "narrow" (func $narrow_export))
  (export "f64_to_i8" (func $f64_to_i8_export))
  (export "f64_to_u8" (func $f64_to_u8_export))
  (export "f32_to_i16" (func $f32_to_i16_export))
  (export "to_floats" (func $to_floats_export))
  (export "case_i8" (func $case_i8_export))
  (export "case_i16" (func $case_i16_export))
  (export "boxed" (func $boxed_export))
  (export "array_i8" (func $array_i8_export))
  (export "array_i16" (func $array_i16_export))
  (export "fields" (func $fields_export))
  (export "set_field" (func $set_field_export))
  (export "tagged" (func $tagged_export))
  (export "memory" (memory 0))
  (export "conduit_reset" (func 31))
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
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    global.get 3
    local.set 7
    global.get 4
    local.set 8
    local.get 0
    i32.const -128
    i32.add
    local.set 1
    local.get 1
    i32.const -128
    i32.add
    local.set 2
    i32.const 2
    local.set 9
    local.get 9
    i32.const 2147418112
    i32.gt_u
    if ;; label = @1
      unreachable
    end
    local.get 9
    i32.const 15
    i32.add
    i32.const -8
    i32.and
    local.set 11
    global.get 3
    global.get 4
    i32.sub
    local.get 11
    i32.lt_u
    if ;; label = @1
      local.get 11
      call 15
    end
    global.get 3
    local.get 11
    i32.sub
    local.tee 10
    global.set 3
    local.get 10
    local.get 9
    i32.store offset=4
    block ;; label = @1
      loop ;; label = @2
        local.get 9
        i32.eqz
        br_if 1 (;@1;)
        local.get 9
        i32.const 1
        i32.sub
        local.tee 9
        local.get 10
        i32.add
        i32.const 0
        i32.store8 offset=8
        br 0 (;@2;)
      end
    end
    local.get 10
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
    local.set 12
    local.get 12
    i32.const 2147418112
    i32.gt_u
    if ;; label = @1
      unreachable
    end
    local.get 12
    i32.const 15
    i32.add
    i32.const -8
    i32.and
    local.set 14
    global.get 3
    global.get 4
    i32.sub
    local.get 14
    i32.lt_u
    if ;; label = @1
      local.get 14
      call 15
    end
    global.get 3
    local.get 14
    i32.sub
    local.tee 13
    global.set 3
    local.get 13
    local.get 12
    i32.store offset=4
    local.get 13
    i32.const 8
    i32.add
    local.get 3
    i32.const 8
    i32.add
    local.get 12
    memory.copy
    local.get 13
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
    global.get 4
    local.get 8
    i32.ne
    if ;; label = @1
      global.get 4
      local.get 8
      call 16
      local.get 8
      global.set 4
    end
    local.get 7
    global.set 3
    return
  )
  (func $array_i16 (;10;) (type 5) (param i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32)
    global.get 3
    local.set 5
    global.get 4
    local.set 6
    global.get 3
    global.get 4
    i32.sub
    i32.const 16
    i32.lt_u
    if ;; label = @1
      i32.const 16
      call 15
    end
    global.get 3
    i32.const 16
    i32.sub
    local.tee 7
    global.set 3
    local.get 7
    i32.const 2
    i32.store offset=4
    local.get 7
    local.get 0
    i32.store16 offset=8
    local.get 7
    i32.const 255
    i32.store16 offset=10
    local.get 7
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
    global.get 4
    local.get 6
    i32.ne
    if ;; label = @1
      global.get 4
      local.get 6
      call 16
      local.get 6
      global.set 4
    end
    local.get 5
    global.set 3
    return
  )
  (func $fields (;11;) (type 7) (param i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32)
    global.get 3
    local.set 7
    global.get 4
    local.set 8
    global.get 3
    global.get 4
    i32.sub
    i32.const 16
    i32.lt_u
    if ;; label = @1
      i32.const 16
      call 15
    end
    global.get 3
    i32.const 16
    i32.sub
    local.tee 9
    global.set 3
    local.get 9
    i32.const 0
    i32.store offset=4
    local.get 9
    local.get 0
    i32.store8 offset=8
    local.get 9
    local.get 1
    i32.store16 offset=10
    local.get 9
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
    global.get 4
    local.get 8
    i32.ne
    if ;; label = @1
      global.get 4
      local.get 8
      call 16
      local.get 8
      global.set 4
    end
    local.get 7
    global.set 3
    return
  )
  (func $set_field (;12;) (type 6) (param i32) (result i32 i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32)
    global.get 3
    local.set 6
    global.get 4
    local.set 7
    local.get 0
    i32.const 1
    i32.add
    local.set 1
    global.get 3
    global.get 4
    i32.sub
    i32.const 16
    i32.lt_u
    if ;; label = @1
      i32.const 16
      call 15
    end
    global.get 3
    i32.const 16
    i32.sub
    local.tee 8
    global.set 3
    local.get 8
    i32.const 0
    i32.store offset=4
    local.get 8
    i32.const 0
    i32.store8 offset=8
    local.get 8
    i32.const 0
    i32.store16 offset=10
    local.get 8
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
    global.get 4
    local.get 7
    i32.ne
    if ;; label = @1
      global.get 4
      local.get 7
      call 16
      local.get 7
      global.set 4
    end
    local.get 6
    global.set 3
    return
  )
  (func $tagged (;13;) (type 5) (param i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    global.get 3
    local.set 13
    global.get 4
    local.set 14
    global.get 3
    global.get 4
    i32.sub
    i32.const 16
    i32.lt_u
    if ;; label = @1
      i32.const 16
      call 15
    end
    global.get 3
    i32.const 16
    i32.sub
    local.tee 15
    global.set 3
    local.get 15
    i32.const 1
    i32.store offset=4
    local.get 15
    i32.const 1000
    i32.store offset=8
    local.get 15
    local.get 0
    i32.store8 offset=12
    local.get 15
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
      global.get 4
      local.get 14
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 14
        call 16
        local.get 14
        global.set 4
      end
      local.get 13
      global.set 3
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
    global.get 4
    local.get 14
    i32.ne
    if ;; label = @1
      global.get 4
      local.get 14
      call 16
      local.get 14
      global.set 4
    end
    local.get 13
    global.set 3
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
  (func (;15;) (type 8) (param i32)
    (local i32 i32)
    local.get 0
    i32.const 23
    i32.add
    i32.const -8
    i32.and
    local.tee 1
    i32.const 4096
    local.get 1
    i32.const 4096
    i32.gt_u
    select
    local.set 1
    global.get 5
    local.tee 2
    if (result i32) ;; label = @1
      local.get 2
      i32.load offset=8
      local.get 1
      i32.ge_u
    else
      i32.const 0
    end
    if ;; label = @1
      local.get 2
      i32.load
      global.set 5
      local.get 2
      i32.load offset=8
      local.set 1
    else
      local.get 1
      call $alloc
      local.set 2
    end
    local.get 2
    global.get 3
    i32.store
    local.get 2
    global.get 4
    i32.store offset=4
    local.get 2
    local.get 1
    i32.store offset=8
    local.get 2
    i32.const 16
    i32.add
    global.set 4
    local.get 2
    local.get 1
    i32.add
    global.set 3
  )
  (func (;16;) (type 9) (param i32 i32)
    (local i32)
    block ;; label = @1
      loop ;; label = @2
        local.get 0
        local.get 1
        i32.eq
        br_if 1 (;@1;)
        local.get 0
        i32.const 16
        i32.sub
        local.tee 2
        i32.load offset=4
        local.set 0
        local.get 2
        global.get 5
        i32.store
        local.get 2
        global.set 5
        br 0 (;@2;)
      end
    end
  )
  (func $widen_export (;17;) (type 0) (param i32) (result i32 i32 i64 i64 i32 i32)
    (local i32 i32 i32 i32 i64 i64 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $widen
          local.set 8
          local.set 7
          local.set 6
          local.set 5
          local.set 4
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
    local.get 4
    local.get 5
    local.get 6
    local.get 7
    local.get 8
  )
  (func $narrow_export (;18;) (type 1) (param i64) (result i32 i32 i32)
    (local i32 i32 i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $narrow
          local.set 5
          local.set 4
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
    local.get 4
    local.get 5
  )
  (func $f64_to_i8_export (;19;) (type 2) (param f64) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $f64_to_i8
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func $f64_to_u8_export (;20;) (type 2) (param f64) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $f64_to_u8
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func $f32_to_i16_export (;21;) (type 3) (param f32) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $f32_to_i16
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func $to_floats_export (;22;) (type 4) (param i32) (result f64 f32)
    (local i32 i32 f64 f32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $to_floats
          local.set 4
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
    local.get 4
  )
  (func $case_i8_export (;23;) (type 5) (param i32) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $case_i8
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func $case_i16_export (;24;) (type 5) (param i32) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $case_i16
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func $boxed_export (;25;) (type 5) (param i32) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $boxed
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func $array_i8_export (;26;) (type 6) (param i32) (result i32 i32)
    (local i32 i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $array_i8
          local.set 4
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
    local.get 4
  )
  (func $array_i16_export (;27;) (type 5) (param i32) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $array_i16
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func $fields_export (;28;) (type 7) (param i32 i32) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 2
    global.get 4
    local.set 3
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          local.get 1
          call $fields
          local.set 4
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 3
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 3
        call 16
        local.get 3
        global.set 4
      end
      local.get 2
      global.set 3
      throw_ref
    end
    local.get 4
  )
  (func $set_field_export (;29;) (type 6) (param i32) (result i32 i32)
    (local i32 i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $set_field
          local.set 4
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
    local.get 4
  )
  (func $tagged_export (;30;) (type 5) (param i32) (result i32)
    (local i32 i32 i32)
    global.get 3
    local.set 1
    global.get 4
    local.set 2
    block ;; label = @1
      block (result exnref) ;; label = @2
        try_table (catch_all_ref 0 (;@2;)) ;; label = @3
          local.get 0
          call $tagged
          local.set 3
        end
        br 1 (;@1;)
      end
      global.get 4
      local.get 2
      i32.ne
      if ;; label = @2
        global.get 4
        local.get 2
        call 16
        local.get 2
        global.set 4
      end
      local.get 1
      global.set 3
      throw_ref
    end
    local.get 3
  )
  (func (;31;) (type 10)
    i32.const 0
    global.set 1
    i32.const 0
    global.set 2
    global.get 4
    i32.const 32
    i32.ne
    if ;; label = @1
      global.get 4
      i32.const 32
      call 16
    end
    i32.const 65568
    global.set 3
    i32.const 32
    global.set 4
  )
  (data (;0;) (i32.const 16) "\00\00\00\00\00\00\00\00\ff\00\fe\ff\00\00\00\00")
)
