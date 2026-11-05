(module
  (type (;0;) (func (param i32) (result i32)))
  (type (;1;) (func (param i64) (result i64)))
  (table (;0;) 0 0 funcref)
  (memory (;0;) 1)
  (global (;0;) (mut i32) i32.const 16)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "fn0" (func $dense))
  (export "fn1" (func $sparse))
  (export "fn2" (func $wide))
  (export "memory" (memory 0))
  (func $dense (;0;) (type 0) (param i32) (result i32)
    block ;; label = @1
      block ;; label = @2
        block ;; label = @3
          block ;; label = @4
            block ;; label = @5
              block ;; label = @6
                local.get 0
                br_table 0 (;@6;) 1 (;@5;) 2 (;@4;) 3 (;@3;) 5 (;@1;) 4 (;@2;) 5 (;@1;)
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
      i32.const 15
      return
    end
    i32.const 99
    return
  )
  (func $sparse (;1;) (type 0) (param i32) (result i32)
    block ;; label = @1
      block ;; label = @2
        block ;; label = @3
          block ;; label = @4
            local.get 0
            i32.const -1000
            i32.eq
            br_if 0 (;@4;)
            local.get 0
            i32.const 7
            i32.eq
            br_if 1 (;@3;)
            local.get 0
            i32.const 1048576
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
  (func $wide (;2;) (type 1) (param i64) (result i64)
    block ;; label = @1
      block ;; label = @2
        local.get 0
        i64.const 1099511627776
        i64.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      i64.const 1
      return
    end
    i64.const 2
    return
  )
  (func $alloc (;3;) (type 0) (param i32) (result i32)
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
)
