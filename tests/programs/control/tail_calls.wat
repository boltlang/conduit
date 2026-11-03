(module
  (type (;0;) (func (param i32 i32) (result i32)))
  (export "fn0" (func 0))
  (func (;0;) (type 0) (param i32 i32) (result i32)
    (local i32 i32 i32)
    block ;; label = @1
      block ;; label = @2
        local.get 0
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 1
      return
    end
    local.get 0
    i32.const 1
    i32.sub
    local.set 2
    local.get 1
    i32.const 2
    i32.add
    local.set 3
    local.get 2
    local.get 3
    return_call 0
  )
)
