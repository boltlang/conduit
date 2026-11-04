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
  (func $widen (;0;) (type 4) (param $x i32) (result i32 i32 i64 i64 i32 i32)
    (local $x1 i32) (local $x2 i32) (local $s32 i32) (local $u32 i32) (local $s64 i64) (local $u64 i64) (local $s16 i32) (local $u16 i32)
    local.get $x
    i32.const -128
    i32.add
    local.set $x1
    local.get $x1
    i32.const -128
    i32.add
    local.set $x2
    local.get $x2
    i32.extend8_s
    local.set $s32
    local.get $x2
    i32.const 255
    i32.and
    local.set $u32
    local.get $x2
    i32.extend8_s
    i64.extend_i32_s
    local.set $s64
    local.get $x2
    i32.const 255
    i32.and
    i64.extend_i32_u
    local.set $u64
    local.get $x2
    i32.extend8_s
    local.set $s16
    local.get $x2
    i32.const 255
    i32.and
    local.set $u16
    local.get $s32
    local.get $u32
    local.get $s64
    local.get $u64
    local.get $s16
    local.get $u16
    return
  )
  (func $narrow (;1;) (type 5) (param $x i64) (result i32 i32 i32)
    (local $s8 i32) (local $u16 i32) (local $u8 i32)
    local.get $x
    i32.wrap_i64
    local.set $s8
    local.get $x
    i32.wrap_i64
    local.set $u16
    local.get $u16
    local.set $u8
    local.get $s8
    local.get $u16
    local.get $u8
    return
  )
  (func $f64_to_i8 (;2;) (type 6) (param $x f64) (result i32)
    (local $y i32) (local i32)
    local.get $x
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
    local.set $y
    local.get $y
    return
  )
  (func $f64_to_u8 (;3;) (type 6) (param $x f64) (result i32)
    (local $y i32) (local i32)
    local.get $x
    i32.trunc_f64_u
    local.tee 2
    i32.const 256
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 2
    local.set $y
    local.get $y
    return
  )
  (func $f32_to_i16 (;4;) (type 7) (param $x f32) (result i32)
    (local $y i32) (local i32)
    local.get $x
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
    local.set $y
    local.get $y
    return
  )
  (func $to_floats (;5;) (type 8) (param $x i32) (result f64 f32)
    (local $x1 i32) (local $x2 i32) (local $f f64) (local $s f32)
    local.get $x
    i32.const -32768
    i32.add
    local.set $x1
    local.get $x1
    i32.const -32768
    i32.add
    local.set $x2
    local.get $x2
    i32.extend16_s
    f64.convert_i32_s
    local.set $f
    local.get $x2
    i32.const 65535
    i32.and
    f32.convert_i32_u
    local.set $s
    local.get $f
    local.get $s
    return
  )
  (func $case_i8 (;6;) (type 9) (param $x i32) (result i32)
    (local $x1 i32) (local $x2 i32)
    local.get $x
    i32.const -128
    i32.add
    local.set $x1
    local.get $x1
    i32.const -128
    i32.add
    local.set $x2
    block ;; label = @1
      block ;; label = @2
        block ;; label = @3
          block ;; label = @4
            local.get $x2
            i32.extend8_s
            i32.const -128
            i32.eq
            br_if 0 (;@4;)
            local.get $x2
            i32.extend8_s
            i32.const -1
            i32.eq
            br_if 1 (;@3;)
            local.get $x2
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
  (func $case_i16 (;7;) (type 9) (param $x i32) (result i32)
    (local $x1 i32) (local $x2 i32)
    local.get $x
    i32.const -32768
    i32.add
    local.set $x1
    local.get $x1
    i32.const -32768
    i32.add
    local.set $x2
    block ;; label = @1
      block ;; label = @2
        block ;; label = @3
          block ;; label = @4
            block ;; label = @5
              block ;; label = @6
                local.get $x2
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
  (func $boxed (;8;) (type 9) (param $x i32) (result i32)
    (local $boxed anyref) (local $y i32)
    local.get $x
    struct.new 0
    local.set $boxed
    local.get $boxed
    ref.cast (ref null 0)
    struct.get 0 0
    local.set $y
    local.get $y
    return
  )
  (func $array_i8 (;9;) (type 10) (param $x i32) (result i32 i32)
    (local $x1 i32) (local $x2 i32) (local $xs (ref null 1)) (local $frozen (ref null 1)) (local $y i32) (local $yu i32) (local (ref null 1))
    local.get $x
    i32.const -128
    i32.add
    local.set $x1
    local.get $x1
    i32.const -128
    i32.add
    local.set $x2
    i32.const 0
    i32.const 2
    array.new 1
    local.set $xs
    local.get $xs
    i32.const 1
    local.get $x2
    array.set 1
    local.get $xs
    array.len
    array.new_default 1
    local.tee 7
    i32.const 0
    local.get $xs
    i32.const 0
    local.get $xs
    array.len
    array.copy 1 1
    local.get 7
    local.set $frozen
    local.get $frozen
    i32.const 1
    array.get_s 1
    local.set $y
    local.get $y
    i32.const 255
    i32.and
    local.set $yu
    local.get $y
    local.get $yu
    return
  )
  (func $array_i16 (;10;) (type 9) (param $x i32) (result i32)
    (local $xs (ref null 2)) (local $a i32) (local $b i32) (local $sum i32)
    local.get $x
    i32.const 255
    array.new_fixed 2 2
    local.set $xs
    local.get $xs
    i32.const 0
    array.get_s 2
    local.set $a
    local.get $xs
    i32.const 1
    array.get_s 2
    local.set $b
    local.get $a
    local.get $b
    i32.add
    local.set $sum
    local.get $sum
    return
  )
  (func $fields (;11;) (type 11) (param $a i32) (param $b i32) (result i32)
    (local $s (ref null 3)) (local $a2 i32) (local $b2 i32) (local $a16 i32) (local $lt i32)
    local.get $a
    local.get $b
    struct.new 3
    local.set $s
    local.get $s
    struct.get 3 0
    local.set $a2
    local.get $s
    struct.get 3 1
    local.set $b2
    local.get $a2
    i32.extend8_s
    local.set $a16
    local.get $a16
    i32.extend16_s
    local.get $b2
    i32.extend16_s
    i32.lt_s
    local.set $lt
    local.get $lt
    return
  )
)
