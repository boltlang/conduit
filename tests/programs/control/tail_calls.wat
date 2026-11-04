(module
  (type (;0;) (func (param i32 i32) (result i32)))
  (export "fn0" (func $count))
  (func $count (;0;) (type 0) (param $n i32) (param $acc i32) (result i32)
    (local $n1 i32) (local $acc1 i32) (local $r i32)
    block ;; label = @1
      block ;; label = @2
        local.get $n
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get $acc
      return
    end
    local.get $n
    i32.const 1
    i32.sub
    local.set $n1
    local.get $acc
    i32.const 2
    i32.add
    local.set $acc1
    local.get $n1
    local.get $acc1
    return_call $count
  )
)
