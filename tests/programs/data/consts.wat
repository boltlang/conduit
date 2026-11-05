(module
  (rec
    (type (;0;) (array (mut i8)))
    (type (;1;) (sub (struct (field i32))))
    (type (;2;) (sub final 1 (struct (field i32) (field (mut i32)) (field (mut (ref null 1))))))
    (type (;3;) (struct (field (mut (ref null 0))) (field (mut f64))))
    (type (;4;) (struct (field (mut (ref null 5)))))
    (type (;5;) (array (mut i32)))
    (type (;6;) (array (mut (ref null 5))))
    (type (;7;) (sub final 1 (struct (field i32))))
  )
  (type (;8;) (func (result i32)))
  (type (;9;) (func (param (ref null 1)) (result i32)))
  (type (;10;) (func (result f64)))
  (type (;11;) (func (result i64)))
  (type (;12;) (func (result f32)))
  (global (;0;) (ref null 0) i32.const 104 i32.const 101 i32.const 108 i32.const 108 i32.const 111 array.new_fixed 0 5)
  (global (;1;) (ref null 1) i32.const 0 struct.new 7)
  (global (;2;) (ref null 1) i32.const 1 i32.const 1 i32.const 1 i32.const 2 i32.const 1 i32.const 3 global.get 1 struct.new 2 struct.new 2 struct.new 2)
  (global (;3;) (ref null 3) global.get 0 f64.const 0x1.4p+1 (;=2.5;) struct.new 3)
  (global (;4;) (ref null 4) i32.const 7 i32.const 8 array.new_fixed 5 2 struct.new 4)
  (global (;5;) i64 i64.const 42)
  (global (;6;) (ref null 6) i32.const 1 i32.const 2 array.new_fixed 5 2 i32.const 3 array.new_fixed 5 1 array.new_fixed 5 0 array.new_fixed 6 3)
  (global (;7;) f32 f32.const nan (;=NaN;))
  (export "fn0" (func $hello_len))
  (export "fn1" (func $sum))
  (export "fn2" (func $sum_list))
  (export "fn3" (func $named_parts))
  (export "fn4" (func $cell_second))
  (export "fn5" (func $the_answer))
  (export "fn6" (func $the_nan))
  (export "fn7" (func $grid_parts))
  (export "fn8" (func $clone_and_write))
  (export "fn9" (func $as_ro))
  (func $hello_len (;0;) (type 8) (result i32)
    (local $s (ref null 0)) (local $n i32) (local $last i32) (local $b i32) (local $n1000 i32) (local $r i32)
    global.get 0
    local.set $s
    local.get $s
    array.len
    local.set $n
    local.get $s
    i32.const 4
    array.get_s 0
    local.set $last
    local.get $last
    i32.const 255
    i32.and
    local.set $b
    local.get $n
    i32.const 1000
    i32.mul
    local.set $n1000
    local.get $n1000
    local.get $b
    i32.add
    local.set $r
    local.get $r
    return
  )
  (func $sum (;1;) (type 9) (param $l (ref null 1)) (result i32)
    (local $head i32) (local $tail (ref null 1)) (local $rest i32) (local $r i32)
    block ;; label = @1
      block ;; label = @2
        local.get $l
        struct.get 1 0
        br_table 0 (;@2;) 1 (;@1;) 0 (;@2;)
      end
      i32.const 0
      return
    end
    local.get $l
    ref.cast (ref null 2)
    struct.get 2 1
    local.set $head
    local.get $l
    ref.cast (ref null 2)
    struct.get 2 2
    local.set $tail
    local.get $tail
    call $sum
    local.set $rest
    local.get $head
    local.get $rest
    i32.add
    local.set $r
    local.get $r
    return
  )
  (func $sum_list (;2;) (type 8) (result i32)
    (local $l (ref null 1)) (local $r i32)
    global.get 2
    local.set $l
    local.get $l
    return_call $sum
  )
  (func $named_parts (;3;) (type 10) (result f64)
    (local $n (ref null 3)) (local $s (ref null 0)) (local $first i32) (local $f f64) (local $b f64) (local $r f64)
    global.get 3
    local.set $n
    local.get $n
    struct.get 3 0
    local.set $s
    local.get $s
    i32.const 0
    array.get_s 0
    local.set $first
    local.get $n
    struct.get 3 1
    local.set $f
    local.get $first
    i32.const 255
    i32.and
    f64.convert_i32_u
    local.set $b
    local.get $b
    local.get $f
    f64.add
    local.set $r
    local.get $r
    return
  )
  (func $cell_second (;4;) (type 8) (result i32)
    (local $c (ref null 4)) (local $xs (ref null 5)) (local $x i32)
    global.get 4
    local.set $c
    local.get $c
    struct.get 4 0
    local.set $xs
    local.get $xs
    i32.const 1
    array.get 5
    local.set $x
    local.get $x
    return
  )
  (func $the_answer (;5;) (type 11) (result i64)
    (local $x i64)
    global.get 5
    local.set $x
    local.get $x
    return
  )
  (func $the_nan (;6;) (type 12) (result f32)
    (local $x f32)
    global.get 7
    local.set $x
    local.get $x
    return
  )
  (func $grid_parts (;7;) (type 8) (result i32)
    (local $g (ref null 6)) (local $row0 (ref null 5)) (local $x i32) (local $row2 (ref null 5)) (local $empty i32) (local $rows i32) (local $x100 i32) (local $rows10 i32) (local $a i32) (local $r i32)
    global.get 6
    local.set $g
    local.get $g
    i32.const 0
    array.get 6
    local.set $row0
    local.get $row0
    i32.const 1
    array.get 5
    local.set $x
    local.get $g
    i32.const 2
    array.get 6
    local.set $row2
    local.get $row2
    array.len
    local.set $empty
    local.get $g
    array.len
    local.set $rows
    local.get $x
    i32.const 100
    i32.mul
    local.set $x100
    local.get $rows
    i32.const 10
    i32.mul
    local.set $rows10
    local.get $x100
    local.get $rows10
    i32.add
    local.set $a
    local.get $a
    local.get $empty
    i32.add
    local.set $r
    local.get $r
    return
  )
  (func $clone_and_write (;8;) (type 8) (result i32)
    (local $s (ref null 0)) (local $m (ref null 0)) (local $again (ref null 0)) (local $b i32) (local $r i32) (local (ref null 0))
    global.get 0
    local.set $s
    local.get $s
    array.len
    array.new_default 0
    local.tee 5
    i32.const 0
    local.get $s
    i32.const 0
    local.get $s
    array.len
    array.copy 0 0
    local.get 5
    local.set $m
    local.get $m
    i32.const 0
    i32.const 0
    array.set 0
    global.get 0
    local.set $again
    local.get $again
    i32.const 0
    array.get_s 0
    local.set $b
    local.get $b
    i32.const 255
    i32.and
    local.set $r
    local.get $r
    return
  )
  (func $as_ro (;9;) (type 8) (result i32)
    (local $s (ref null 0)) (local $frozen (ref null 0)) (local $b i32) (local $r i32) (local (ref null 0))
    global.get 0
    local.set $s
    local.get $s
    array.len
    array.new_default 0
    local.tee 4
    i32.const 0
    local.get $s
    i32.const 0
    local.get $s
    array.len
    array.copy 0 0
    local.get 4
    local.set $frozen
    local.get $frozen
    i32.const 1
    array.get_s 0
    local.set $b
    local.get $b
    i32.const 255
    i32.and
    local.set $r
    local.get $r
    return
  )
)
