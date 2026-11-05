(module
  (rec
    (type (;0;) (sub (struct (field (ref null 1)))))
    (type (;1;) (func (param (ref null 0) i32) (result i32)))
    (type (;2;) (sub (struct (field i32) (field (mut (ref null 2))))))
    (type (;3;) (sub final 2 (struct (field i32) (field (mut (ref null 2))))))
    (type (;4;) (sub 2 (struct (field i32) (field (mut (ref null 2))) (field (ref null 5)))))
    (type (;5;) (func (param (ref null 4)) (result i32)))
    (type (;6;) (sub (struct (field (ref null 7)))))
    (type (;7;) (func (param (ref null 6)) (result i32)))
    (type (;8;) (sub final 6 (struct (field (ref null 7)))))
    (type (;9;) (func (result i32)))
    (type (;10;) (cont 11))
    (type (;11;) (func (param i32) (result i32)))
    (type (;12;) (func (result (ref null 10))))
    (type (;13;) (cont 7))
    (type (;14;) (sub final 0 (struct (field (ref null 1)) (field (ref null 3)) (field (ref null 10)))))
  )
  (type (;15;) (func (param (ref null 0)) (result i32)))
  (type (;16;) (func (param i32) (result i32)))
  (type (;17;) (func (result i32)))
  (type (;18;) (func (param (ref null 3) (ref null 6)) (result i32)))
  (type (;19;) (func (param (ref null 3) (ref null 10) i32) (result i32)))
  (tag (;0;) (type 17) (result i32))
  (global (;0;) (mut (ref null 2)) ref.null 2)
  (export "fn0" (func $ask))
  (export "fn1" (func $done))
  (export "fn2" (func $ask_plus_10))
  (export "fn3" (func $main))
  (elem (;0;) declare func $doubler.start $doubler.drive0 $doubler.k0 7)
  (func $ask (;0;) (type 15) (param $k (ref null 0)) (result i32)
    (local $x i32) (local $r i32)
    local.get $k
    i32.const 1
    local.get $k
    struct.get 0 0
    call_ref 1
    local.set $x
    local.get $x
    i32.const 2
    i32.mul
    local.set $r
    local.get $r
    return
  )
  (func $done (;1;) (type 16) (param $r i32) (result i32)
    local.get $r
    return
  )
  (func $ask_plus_10 (;2;) (type 17) (result i32)
    (local $x i32) (local $r i32) (local i32 (ref null 2) (ref null 2) (ref null 2) (ref null 2) (ref null 4))
    global.get 0
    local.set 3
    global.get 0
    local.set 4
    block ;; label = @1
      loop ;; label = @2
        local.get 4
        struct.get 2 0
        i32.const 0
        i32.eq
        br_if 1 (;@1;)
        local.get 4
        struct.get 2 1
        local.set 4
        br 0 (;@2;)
      end
    end
    local.get 4
    ref.test (ref 3)
    if ;; label = @1
      suspend 0
      local.set 2
      local.get 3
      global.set 0
    else
      global.get 0
      local.tee 5
      local.set 6
      block ;; label = @2
        loop ;; label = @3
          local.get 6
          struct.get 2 0
          i32.const 0
          i32.eq
          br_if 1 (;@2;)
          local.get 6
          struct.get 2 1
          local.set 6
          br 0 (;@3;)
        end
      end
      local.get 6
      struct.get 2 1
      global.set 0
      local.get 6
      ref.cast (ref null 4)
      local.tee 7
      local.get 7
      struct.get 4 2
      call_ref 5
      local.get 5
      global.set 0
      local.set 2
    end
    local.get 2
    local.set $x
    local.get $x
    i32.const 10
    i32.add
    local.set $r
    local.get $r
    return
  )
  (func $main (;3;) (type 17) (result i32)
    (local $body (ref null 6)) (local $r i32)
    ref.func 7
    struct.new 8
    local.set $body
    i32.const 0
    ref.null 2
    struct.new 3
    local.get $body
    return_call $doubler.start
  )
  (func $doubler.start (;4;) (type 18) (param (ref null 3) (ref null 6)) (result i32)
    (local (ref null 10) i32)
    local.get 0
    global.get 0
    struct.set 2 1
    local.get 0
    global.set 0
    block (type 9) (result i32) ;; label = @1
      block (type 12) (result (ref null 10)) ;; label = @2
        local.get 1
        local.get 1
        struct.get 6 0
        cont.new 13
        resume 13 (on 0 0 (;@2;))
        br 1 (;@1;)
      end
      local.set 2
      local.get 0
      struct.get 2 1
      global.set 0
      ref.func $doubler.k0
      local.get 0
      local.get 2
      struct.new 14
      return_call $ask
    end
    local.set 3
    local.get 0
    struct.get 2 1
    global.set 0
    local.get 3
    return_call $done
  )
  (func $doubler.drive0 (;5;) (type 19) (param (ref null 3) (ref null 10) i32) (result i32)
    (local (ref null 10) i32)
    local.get 0
    global.get 0
    struct.set 2 1
    local.get 0
    global.set 0
    block (type 9) (result i32) ;; label = @1
      block (type 12) (result (ref null 10)) ;; label = @2
        local.get 2
        local.get 1
        resume 10 (on 0 0 (;@2;))
        br 1 (;@1;)
      end
      local.set 3
      local.get 0
      struct.get 2 1
      global.set 0
      ref.func $doubler.k0
      local.get 0
      local.get 3
      struct.new 14
      return_call $ask
    end
    local.set 4
    local.get 0
    struct.get 2 1
    global.set 0
    local.get 4
    return_call $done
  )
  (func $doubler.k0 (;6;) (type 1) (param (ref null 0) i32) (result i32)
    (local (ref null 14))
    local.get 0
    ref.cast (ref null 14)
    local.set 2
    local.get 2
    struct.get 14 1
    local.get 2
    struct.get 14 2
    local.get 1
    return_call $doubler.drive0
  )
  (func (;7;) (type 7) (param (ref null 6)) (result i32)
    (local (ref null 8))
    local.get 0
    ref.cast (ref null 8)
    local.set 1
    return_call $ask_plus_10
  )
)
