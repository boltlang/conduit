(module
  (rec
    (type (;0;) (struct (field (mut i32))))
    (type (;1;) (sub (struct (field (ref null 2)))))
    (type (;2;) (func (param (ref null 1))))
    (type (;3;) (sub final 1 (struct (field (ref null 2)))))
    (type (;4;) (sub (struct (field i32) (field (ref null 4)))))
    (type (;5;) (sub 4 (struct (field i32) (field (ref null 4)) (field (ref null 6)))))
    (type (;6;) (func (param (ref null 5) i32)))
    (type (;7;) (sub final 5 (struct (field i32) (field (ref null 4)) (field (ref null 6)) (field (ref null 0)))))
  )
  (type (;8;) (func (result i32)))
  (type (;9;) (func (param (ref null 0) i32)))
  (type (;10;) (func (param (ref null 0))))
  (type (;11;) (func))
  (global (;0;) (mut (ref null 4)) ref.null 4)
  (export "fn0" (func $main))
  (export "fn1" (func $add_yield))
  (export "fn2" (func $done))
  (export "fn3" (func $produce))
  (elem (;0;) declare func 4 5)
  (func $main (;0;) (type 8) (result i32)
    (local $acc (ref null 0)) (local $body (ref null 1)) (local $total i32) (local (ref null 4))
    i32.const 0
    struct.new 0
    local.set $acc
    ref.func 5
    struct.new 3
    local.set $body
    global.get 0
    local.set 3
    i32.const 0
    local.get 3
    ref.func 4
    local.get $acc
    struct.new 7
    global.set 0
    local.get $body
    local.get $body
    struct.get 1 0
    call_ref 2
    local.get 3
    global.set 0
    local.get $acc
    call $done
    local.get $acc
    struct.get 0 0
    local.set $total
    local.get $total
    return
  )
  (func $add_yield (;1;) (type 9) (param $acc (ref null 0)) (param $x i32)
    (local $k i32) (local $old i32) (local $new i32)
    local.get $acc
    struct.get 0 0
    local.set $old
    local.get $old
    local.get $x
    i32.add
    local.set $new
    local.get $acc
    local.get $new
    struct.set 0 0
    return
  )
  (func $done (;2;) (type 10) (param $acc (ref null 0))
    return
  )
  (func $produce (;3;) (type 11)
    (local (ref null 4) (ref null 4) (ref null 5) (ref null 4) (ref null 4) (ref null 5) (ref null 4) (ref null 4) (ref null 5))
    global.get 0
    local.tee 0
    local.set 1
    block ;; label = @1
      loop ;; label = @2
        local.get 1
        struct.get 4 0
        i32.const 0
        i32.eq
        br_if 1 (;@1;)
        local.get 1
        struct.get 4 1
        local.set 1
        br 0 (;@2;)
      end
    end
    local.get 1
    struct.get 4 1
    global.set 0
    local.get 1
    ref.cast (ref null 5)
    local.tee 2
    i32.const 1
    local.get 2
    struct.get 5 2
    call_ref 6
    local.get 0
    global.set 0
    global.get 0
    local.tee 3
    local.set 4
    block ;; label = @1
      loop ;; label = @2
        local.get 4
        struct.get 4 0
        i32.const 0
        i32.eq
        br_if 1 (;@1;)
        local.get 4
        struct.get 4 1
        local.set 4
        br 0 (;@2;)
      end
    end
    local.get 4
    struct.get 4 1
    global.set 0
    local.get 4
    ref.cast (ref null 5)
    local.tee 5
    i32.const 2
    local.get 5
    struct.get 5 2
    call_ref 6
    local.get 3
    global.set 0
    global.get 0
    local.tee 6
    local.set 7
    block ;; label = @1
      loop ;; label = @2
        local.get 7
        struct.get 4 0
        i32.const 0
        i32.eq
        br_if 1 (;@1;)
        local.get 7
        struct.get 4 1
        local.set 7
        br 0 (;@2;)
      end
    end
    local.get 7
    struct.get 4 1
    global.set 0
    local.get 7
    ref.cast (ref null 5)
    local.tee 8
    i32.const 3
    local.get 8
    struct.get 5 2
    call_ref 6
    local.get 6
    global.set 0
    return
  )
  (func (;4;) (type 6) (param (ref null 5) i32)
    (local (ref null 7))
    local.get 0
    ref.cast (ref null 7)
    local.set 2
    local.get 2
    struct.get 7 3
    local.get 1
    return_call $add_yield
  )
  (func (;5;) (type 2) (param (ref null 1))
    (local (ref null 3))
    local.get 0
    ref.cast (ref null 3)
    local.set 1
    return_call $produce
  )
)
