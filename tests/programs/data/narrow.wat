(module
  (rec
    (type (;0;) (struct (field i32)))
    (type (;1;) (array (mut i8)))
    (type (;2;) (array (mut i16)))
    (type (;3;) (struct (field (mut i32)) (field (mut i32))))
  )
  (type (;4;) (func (param i32) (result i32 i32 i64 i64 i32 i32)))
  (type (;5;) (func (param i64) (result i32 i32 i32)))
  (type (;6;) (func (param f64) (result i32)))
  (type (;7;) (func (param f32) (result i32)))
  (type (;8;) (func (param i32) (result f64 f32)))
  (type (;9;) (func (param i32) (result i32)))
  (type (;10;) (func (param i32) (result i32 i32)))
  (type (;11;) (func (param i32 i32) (result i32)))
  (export "fn0" (func 0))
  (export "fn1" (func 1))
  (export "fn2" (func 2))
  (export "fn3" (func 3))
  (export "fn4" (func 4))
  (export "fn5" (func 5))
  (export "fn6" (func 6))
  (export "fn7" (func 7))
  (export "fn8" (func 8))
  (export "fn10" (func 9))
  (export "fn11" (func 10))
  (export "fn12" (func 11))
  (func (;0;) (type 4) (param i32) (result i32 i32 i64 i64 i32 i32)
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
  (func (;1;) (type 5) (param i64) (result i32 i32 i32)
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
  (func (;2;) (type 6) (param f64) (result i32)
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
  (func (;3;) (type 6) (param f64) (result i32)
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
  (func (;4;) (type 7) (param f32) (result i32)
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
  (func (;5;) (type 8) (param i32) (result f64 f32)
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
  (func (;6;) (type 9) (param i32) (result i32)
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
  (func (;7;) (type 9) (param i32) (result i32)
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
  (func (;8;) (type 9) (param i32) (result i32)
    (local anyref i32)
    local.get 0
    struct.new 0
    local.set 1
    local.get 1
    ref.cast (ref null 0)
    struct.get 0 0
    local.set 2
    local.get 2
    return
  )
  (func (;9;) (type 10) (param i32) (result i32 i32)
    (local i32 i32 (ref null 1) (ref null 1) i32 i32 (ref null 1))
    local.get 0
    i32.const -128
    i32.add
    local.set 1
    local.get 1
    i32.const -128
    i32.add
    local.set 2
    i32.const 0
    i32.const 2
    array.new 1
    local.set 3
    local.get 3
    i32.const 1
    local.get 2
    array.set 1
    local.get 3
    array.len
    array.new_default 1
    local.tee 7
    i32.const 0
    local.get 3
    i32.const 0
    local.get 3
    array.len
    array.copy 1 1
    local.get 7
    local.set 4
    local.get 4
    i32.const 1
    array.get_s 1
    local.set 5
    local.get 5
    i32.const 255
    i32.and
    local.set 6
    local.get 5
    local.get 6
    return
  )
  (func (;10;) (type 9) (param i32) (result i32)
    (local (ref null 2) i32 i32 i32)
    local.get 0
    i32.const 255
    array.new_fixed 2 2
    local.set 1
    local.get 1
    i32.const 0
    array.get_s 2
    local.set 2
    local.get 1
    i32.const 1
    array.get_s 2
    local.set 3
    local.get 2
    local.get 3
    i32.add
    local.set 4
    local.get 4
    return
  )
  (func (;11;) (type 11) (param i32 i32) (result i32)
    (local (ref null 3) i32 i32 i32 i32)
    local.get 0
    local.get 1
    struct.new 3
    local.set 2
    local.get 2
    struct.get 3 0
    local.set 3
    local.get 2
    struct.get 3 1
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
)
