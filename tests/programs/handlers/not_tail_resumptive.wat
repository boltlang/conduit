(module
  (type (;0;) (func (param i32) (result i32)))
  (type (;1;) (func (result i32)))
  (type (;2;) (func (param i32 i32) (result i32 i32)))
  (type (;3;) (func (param i32 i32 i32 i32) (result i32)))
  (type (;4;) (func (param i32 i32 i32) (result i32)))
  (type (;5;) (func (param i32 i32) (result i32)))
  (type (;6;) (func (param i32 i32 i32 i32 i32) (result i32)))
  (table (;0;) 9 9 funcref)
  (memory (;0;) 1)
  (global (;0;) (mut i32) i32.const 32)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "fn3" (func $main))
  (export "memory" (memory 0))
  (elem (;0;) (table 0) (i32.const 0) func 18 19 20 21 22 23 24 25 26)
  (func $ask (;0;) (type 0) (param i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 8
    local.get 8
    i32.const 0
    i32.store offset=8
    local.get 8
    local.set 1
    i32.const 0
    local.set 2
    i32.const 32
    call $alloc
    local.set 9
    local.get 9
    i32.const 0
    i32.store offset=4
    local.get 9
    i32.const 0
    i32.store offset=8
    local.get 9
    i32.const 0
    i32.store offset=12
    local.get 9
    local.get 1
    i32.store offset=16
    local.get 9
    local.get 2
    i32.store offset=20
    local.get 9
    i32.const 1
    i32.store offset=24
    local.get 9
    local.set 3
    i32.const 8
    call $alloc
    local.set 10
    local.get 10
    i32.const 0
    i32.store offset=4
    local.get 10
    local.set 4
    i32.const 16
    call $alloc
    local.set 11
    local.get 11
    i32.const 0
    i32.store offset=4
    local.get 11
    local.get 3
    i32.store offset=8
    local.get 11
    local.get 4
    i32.store offset=12
    local.get 11
    local.set 5
    local.get 0
    local.get 5
    call $ask_ev
    local.set 6
    local.get 3
    i32.load offset=8
    local.set 7
    block ;; label = @1
      block ;; label = @2
        local.get 7
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 6
      return
    end
    unreachable
  )
  (func $done (;1;) (type 0) (param i32) (result i32)
    local.get 0
    return
  )
  (func $ask_plus_10 (;2;) (type 1) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 7
    local.get 7
    i32.const 0
    i32.store offset=8
    local.get 7
    local.set 0
    i32.const 0
    local.set 1
    i32.const 32
    call $alloc
    local.set 8
    local.get 8
    i32.const 0
    i32.store offset=4
    local.get 8
    i32.const 0
    i32.store offset=8
    local.get 8
    i32.const 0
    i32.store offset=12
    local.get 8
    local.get 0
    i32.store offset=16
    local.get 8
    local.get 1
    i32.store offset=20
    local.get 8
    i32.const 1
    i32.store offset=24
    local.get 8
    local.set 2
    i32.const 8
    call $alloc
    local.set 9
    local.get 9
    i32.const 0
    i32.store offset=4
    local.get 9
    local.set 3
    i32.const 16
    call $alloc
    local.set 10
    local.get 10
    i32.const 0
    i32.store offset=4
    local.get 10
    local.get 2
    i32.store offset=8
    local.get 10
    local.get 3
    i32.store offset=12
    local.get 10
    local.set 4
    local.get 4
    call $ask_plus_10_ev
    local.set 5
    local.get 2
    i32.load offset=8
    local.set 6
    block ;; label = @1
      block ;; label = @2
        local.get 6
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 5
      return
    end
    unreachable
  )
  (func $main (;3;) (type 1) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 7
    local.get 7
    i32.const 0
    i32.store offset=8
    local.get 7
    local.set 0
    i32.const 0
    local.set 1
    i32.const 32
    call $alloc
    local.set 8
    local.get 8
    i32.const 0
    i32.store offset=4
    local.get 8
    i32.const 0
    i32.store offset=8
    local.get 8
    i32.const 0
    i32.store offset=12
    local.get 8
    local.get 0
    i32.store offset=16
    local.get 8
    local.get 1
    i32.store offset=20
    local.get 8
    i32.const 1
    i32.store offset=24
    local.get 8
    local.set 2
    i32.const 8
    call $alloc
    local.set 9
    local.get 9
    i32.const 0
    i32.store offset=4
    local.get 9
    local.set 3
    i32.const 16
    call $alloc
    local.set 10
    local.get 10
    i32.const 0
    i32.store offset=4
    local.get 10
    local.get 2
    i32.store offset=8
    local.get 10
    local.get 3
    i32.store offset=12
    local.get 10
    local.set 4
    local.get 4
    call $main_ev
    local.set 5
    local.get 2
    i32.load offset=8
    local.set 6
    block ;; label = @1
      block ;; label = @2
        local.get 6
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 5
      return
    end
    unreachable
  )
  (func $lookup (;4;) (type 2) (param i32 i32) (result i32 i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32)
    local.get 0
    i32.load offset=12
    local.set 2
    block ;; label = @1
      local.get 2
      local.set 3
      br 0 (;@1;)
    end
    loop ;; label = @1
      block ;; label = @2
        block ;; label = @3
          local.get 3
          i32.load offset=4
          br_table 0 (;@3;) 1 (;@2;) 0 (;@3;)
        end
        unreachable
      end
      local.get 3
      i32.load offset=8
      local.set 4
      local.get 4
      i32.load offset=8
      local.set 5
      local.get 1
      local.get 5
      i32.load offset=4
      i32.ge_u
      if ;; label = @2
        unreachable
      end
      local.get 5
      local.get 1
      i32.const 2
      i32.shl
      i32.add
      i32.load offset=8
      local.set 6
      local.get 6
      i32.const 0
      i32.ge_s
      local.set 7
      block ;; label = @2
        block ;; label = @3
          local.get 7
          i32.const 0
          i32.eq
          br_if 0 (;@3;)
          br 1 (;@2;)
        end
        local.get 4
        i32.load offset=24
        local.set 8
        local.get 8
        i32.load offset=12
        local.set 9
        local.get 9
        local.set 3
        br 1 (;@1;)
      end
      local.get 4
      local.get 6
      return
    end
    unreachable
  )
  (func $compose (;5;) (type 3) (param i32 i32 i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    local.get 0
    local.get 2
    local.get 3
    local.get 0
    i32.load offset=4
    call_indirect (type 4)
    local.set 4
    local.get 3
    i32.load offset=8
    local.set 5
    local.get 5
    i32.load offset=8
    local.set 6
    local.get 6
    i32.const 0
    i32.ne
    local.set 7
    block ;; label = @1
      block ;; label = @2
        local.get 7
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 1
      local.get 4
      local.get 3
      local.get 1
      i32.load offset=4
      return_call_indirect (type 4)
    end
    local.get 3
    i32.load offset=8
    local.set 9
    local.get 9
    i32.load offset=20
    local.set 10
    i32.const 16
    call $alloc
    local.set 13
    local.get 13
    i32.const 0
    i32.store offset=4
    local.get 13
    local.get 10
    i32.store offset=8
    local.get 13
    local.get 1
    i32.store offset=12
    local.get 13
    local.set 11
    local.get 9
    local.get 11
    i32.store offset=20
    i32.const 0
    local.set 12
    local.get 12
    return
  )
  (func $start_resume (;6;) (type 4) (param i32 i32 i32) (result i32)
    (local i32)
    local.get 0
    local.get 1
    local.get 2
    local.get 0
    i32.load offset=4
    return_call_indirect (type 4)
  )
  (func $doubler_start (;7;) (type 5) (param i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32)
    local.get 0
    local.get 1
    local.get 0
    i32.load offset=4
    call_indirect (type 5)
    local.set 2
    local.get 1
    i32.load offset=8
    local.set 3
    local.get 3
    i32.load offset=8
    local.set 4
    local.get 4
    i32.const 0
    i32.ne
    local.set 5
    block ;; label = @1
      block ;; label = @2
        local.get 5
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      i32.const 16
      call $alloc
      local.set 8
      local.get 8
      local.get 2
      i32.store offset=8
      local.get 8
      local.set 6
      local.get 6
      return
    end
    i32.const 0
    local.set 7
    local.get 7
    return
  )
  (func $doubler_drive (;8;) (type 4) (param i32 i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    local.get 1
    local.get 0
    i32.store offset=24
    local.get 0
    i32.load offset=8
    local.set 3
    i32.const 16
    call $alloc
    local.set 23
    local.get 23
    i32.const 1
    i32.store offset=4
    local.get 23
    local.get 1
    i32.store offset=8
    local.get 23
    local.set 4
    i32.const 16
    call $alloc
    local.set 24
    local.get 24
    i32.const 0
    i32.store offset=4
    local.get 24
    local.get 3
    i32.store offset=8
    local.get 24
    local.get 4
    i32.store offset=12
    local.get 24
    local.set 5
    local.get 2
    local.get 5
    local.get 2
    i32.load offset=4
    call_indirect (type 5)
    local.set 6
    local.get 3
    i32.load offset=8
    local.set 7
    block ;; label = @1
      block ;; label = @2
        local.get 7
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 6
      i32.load offset=8
      local.set 8
      local.get 8
      return_call $done
    end
    local.get 1
    i32.load offset=12
    local.set 21
    local.get 7
    local.get 21
    i32.eq
    local.set 22
    block ;; label = @1
      block ;; label = @2
        local.get 22
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 3
      i32.load offset=20
      local.set 10
      i32.const 16
      call $alloc
      local.set 25
      local.get 25
      i32.const 1
      i32.store offset=4
      local.get 25
      local.get 1
      i32.store offset=8
      local.get 25
      local.get 10
      i32.store offset=12
      local.get 25
      local.set 11
      local.get 3
      local.get 11
      i32.store offset=20
      i32.const 0
      local.set 12
      local.get 12
      return
    end
    local.get 3
    i32.load offset=12
    local.set 13
    local.get 3
    i32.load offset=16
    local.set 14
    local.get 3
    i32.load offset=20
    local.set 15
    local.get 3
    i32.const 0
    i32.store offset=8
    i32.const 0
    local.set 16
    local.get 3
    local.get 16
    i32.store offset=16
    i32.const 0
    local.set 17
    local.get 3
    local.get 17
    i32.store offset=20
    i32.const 16
    call $alloc
    local.set 26
    local.get 26
    i32.const 0
    i32.store offset=8
    local.get 26
    local.set 18
    i32.const 24
    call $alloc
    local.set 27
    local.get 27
    i32.const 2
    i32.store offset=4
    local.get 27
    local.get 1
    i32.store offset=8
    local.get 27
    local.get 15
    i32.store offset=12
    local.get 27
    local.get 18
    i32.store offset=16
    local.get 27
    local.set 19
    block ;; label = @1
      block ;; label = @2
        local.get 13
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 19
      local.get 0
      return_call $ask_ev
    end
    unreachable
  )
  (func $doubler_reprompt (;9;) (type 3) (param i32 i32 i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 11
    local.get 11
    i32.const 3
    i32.store offset=4
    local.get 11
    local.get 1
    i32.store offset=8
    local.get 11
    local.get 2
    i32.store offset=12
    local.get 11
    local.set 4
    local.get 3
    local.get 0
    local.get 4
    call $doubler_drive
    local.set 5
    local.get 3
    i32.load offset=8
    local.set 6
    local.get 6
    i32.load offset=8
    local.set 7
    local.get 7
    i32.const 0
    i32.ne
    local.set 8
    block ;; label = @1
      block ;; label = @2
        local.get 8
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      i32.const 16
      call $alloc
      local.set 12
      local.get 12
      local.get 5
      i32.store offset=8
      local.get 12
      local.set 9
      local.get 9
      return
    end
    i32.const 0
    local.set 10
    local.get 10
    return
  )
  (func $doubler_k (;10;) (type 6) (param i32 i32 i32 i32 i32) (result i32)
    (local i32 i32 i32 i32)
    local.get 2
    i32.load offset=8
    local.set 5
    block ;; label = @1
      block ;; label = @2
        local.get 5
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 2
      i32.const 1
      i32.store offset=8
      i32.const 16
      call $alloc
      local.set 8
      local.get 8
      i32.const 3
      i32.store offset=4
      local.get 8
      local.get 1
      i32.store offset=8
      local.get 8
      local.get 3
      i32.store offset=12
      local.get 8
      local.set 6
      local.get 4
      local.get 0
      local.get 6
      return_call $doubler_drive
    end
    unreachable
  )
  (func $ask_ev (;11;) (type 5) (param i32 i32) (result i32)
    (local i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 6
    local.get 6
    i32.const 4
    i32.store offset=4
    local.get 6
    i32.const 1
    i32.store offset=8
    local.get 6
    local.set 4
    local.get 4
    local.set 5
    local.get 0
    local.get 5
    local.get 1
    local.get 0
    i32.load offset=4
    call_indirect (type 4)
    local.set 2
    local.get 2
    i32.const 2
    i32.mul
    local.set 3
    local.get 3
    return
  )
  (func $ask_plus_10_ev (;12;) (type 0) (param i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    local.get 0
    i32.const 1
    call $lookup
    local.set 4
    local.set 3
    local.get 4
    i32.const 0
    i32.add
    local.set 5
    local.get 3
    i32.load offset=16
    local.set 6
    local.get 5
    local.get 6
    i32.load offset=4
    i32.ge_u
    if ;; label = @1
      unreachable
    end
    local.get 6
    local.get 5
    i32.const 2
    i32.shl
    i32.add
    i32.load offset=8
    local.set 7
    block ;; label = @1
      block ;; label = @2
        block ;; label = @3
          local.get 7
          i32.const 0
          i32.eq
          br_if 0 (;@3;)
          br 1 (;@2;)
        end
        local.get 0
        i32.load offset=8
        local.set 21
        local.get 3
        i32.load offset=12
        local.set 22
        local.get 21
        local.get 22
        i32.store offset=8
        local.get 21
        local.get 5
        i32.store offset=12
        i32.const 16
        call $alloc
        local.set 27
        local.get 27
        i32.const 0
        i32.store offset=8
        local.get 27
        local.set 23
        local.get 21
        local.get 23
        i32.store offset=16
        i32.const 16
        call $alloc
        local.set 28
        local.get 28
        i32.const 5
        i32.store offset=4
        local.get 28
        local.get 0
        i32.store offset=8
        local.get 28
        local.set 24
        i32.const 16
        call $alloc
        local.set 29
        local.get 29
        i32.const 6
        i32.store offset=4
        local.get 29
        local.get 24
        i32.store offset=8
        local.get 29
        local.get 0
        i32.store offset=12
        local.get 29
        local.set 25
        local.get 21
        local.get 25
        i32.store offset=20
        i32.const 0
        local.set 26
        local.get 26
        return
      end
      local.get 3
      i32.load offset=20
      local.set 16
      local.get 5
      local.get 16
      i32.load offset=4
      i32.ge_u
      if ;; label = @2
        unreachable
      end
      local.get 16
      local.get 5
      i32.const 2
      i32.shl
      i32.add
      i32.load offset=8
      local.set 17
      local.get 17
      local.set 18
      local.get 3
      i32.load offset=24
      local.set 19
      local.get 18
      local.get 19
      local.get 18
      i32.load offset=4
      call_indirect (type 5)
      local.set 20
      local.get 20
      local.set 1
      br 0 (;@1;)
    end
    local.get 0
    i32.load offset=8
    local.set 8
    local.get 8
    i32.load offset=8
    local.set 9
    local.get 9
    i32.const 0
    i32.ne
    local.set 10
    block ;; label = @1
      block ;; label = @2
        local.get 10
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      local.get 1
      i32.const 10
      i32.add
      local.set 2
      local.get 2
      return
    end
    i32.const 16
    call $alloc
    local.set 30
    local.get 30
    i32.const 5
    i32.store offset=4
    local.get 30
    local.get 0
    i32.store offset=8
    local.get 30
    local.set 11
    local.get 0
    i32.load offset=8
    local.set 12
    local.get 12
    i32.load offset=20
    local.set 13
    i32.const 16
    call $alloc
    local.set 31
    local.get 31
    i32.const 0
    i32.store offset=4
    local.get 31
    local.get 13
    i32.store offset=8
    local.get 31
    local.get 11
    i32.store offset=12
    local.get 31
    local.set 14
    local.get 12
    local.get 14
    i32.store offset=20
    i32.const 0
    local.set 15
    local.get 15
    return
  )
  (func $main_ev (;13;) (type 0) (param i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 8
    call $alloc
    local.set 12
    local.get 12
    i32.const 7
    i32.store offset=4
    local.get 12
    local.set 1
    local.get 0
    i32.load offset=8
    local.set 3
    local.get 3
    i32.load offset=24
    local.set 4
    local.get 4
    i32.const 1
    i32.add
    local.set 5
    local.get 3
    local.get 5
    i32.store offset=24
    i32.const 16
    call $alloc
    local.set 13
    local.get 13
    i32.const 1
    i32.store offset=4
    local.get 13
    i32.const 0
    i32.store offset=8
    local.get 13
    local.set 6
    i32.const 16
    call $alloc
    local.set 14
    local.get 14
    i32.const 0
    i32.store offset=8
    local.get 14
    local.set 7
    i32.const 16
    call $alloc
    local.set 15
    local.get 15
    i32.const 1
    i32.store offset=4
    local.get 15
    local.get 7
    i32.store offset=8
    local.get 15
    local.set 8
    i32.const 16
    local.set 9
    i32.const 32
    call $alloc
    local.set 16
    local.get 16
    i32.const 0
    i32.store offset=4
    local.get 16
    local.get 9
    i32.store offset=8
    local.get 16
    local.get 4
    i32.store offset=12
    local.get 16
    local.get 6
    i32.store offset=16
    local.get 16
    local.get 8
    i32.store offset=20
    local.get 16
    local.get 0
    i32.store offset=24
    local.get 16
    local.set 10
    i32.const 16
    call $alloc
    local.set 17
    local.get 17
    i32.const 8
    i32.store offset=4
    local.get 17
    local.get 1
    i32.store offset=8
    local.get 17
    local.set 11
    local.get 0
    local.get 10
    local.get 11
    return_call $doubler_drive
  )
  (func $resume_with (;14;) (type 5) (param i32 i32) (result i32)
    local.get 0
    return
  )
  (func $ask_plus_10_rest (;15;) (type 4) (param i32 i32 i32) (result i32)
    (local i32 i32 i32 i32)
    local.get 1
    i32.load offset=8
    local.set 3
    local.get 3
    i32.const 10
    i32.add
    local.set 4
    i32.const 16
    call $alloc
    local.set 6
    local.get 6
    local.get 4
    i32.store offset=8
    local.get 6
    local.set 5
    local.get 5
    return
  )
  (func $force (;16;) (type 3) (param i32 i32 i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    local.get 2
    local.set 4
    local.get 4
    local.get 1
    local.get 4
    i32.load offset=4
    call_indirect (type 5)
    local.set 5
    local.get 1
    i32.load offset=8
    local.set 6
    local.get 6
    i32.load offset=8
    local.set 7
    local.get 7
    i32.const 0
    i32.ne
    local.set 8
    block ;; label = @1
      block ;; label = @2
        local.get 8
        i32.const 0
        i32.eq
        br_if 0 (;@2;)
        br 1 (;@1;)
      end
      i32.const 16
      call $alloc
      local.set 15
      local.get 15
      local.get 5
      i32.store offset=8
      local.get 15
      local.set 9
      local.get 0
      local.get 9
      local.get 1
      local.get 0
      i32.load offset=4
      return_call_indirect (type 4)
    end
    local.get 1
    i32.load offset=8
    local.set 11
    local.get 11
    i32.load offset=20
    local.set 12
    i32.const 16
    call $alloc
    local.set 16
    local.get 16
    i32.const 0
    i32.store offset=4
    local.get 16
    local.get 12
    i32.store offset=8
    local.get 16
    local.get 0
    i32.store offset=12
    local.get 16
    local.set 13
    local.get 11
    local.get 13
    i32.store offset=20
    i32.const 0
    local.set 14
    local.get 14
    return
  )
  (func $alloc (;17;) (type 0) (param i32) (result i32)
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
  (func (;18;) (type 4) (param i32 i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 0
    i32.load offset=12
    local.get 1
    local.get 2
    return_call $compose
  )
  (func (;19;) (type 4) (param i32 i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 0
    i32.load offset=12
    local.get 1
    local.get 2
    return_call $doubler_reprompt
  )
  (func (;20;) (type 4) (param i32 i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 0
    i32.load offset=12
    local.get 0
    i32.load offset=16
    local.get 1
    local.get 2
    return_call $doubler_k
  )
  (func (;21;) (type 5) (param i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 0
    i32.load offset=12
    local.get 1
    return_call $start_resume
  )
  (func (;22;) (type 5) (param i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 1
    return_call $resume_with
  )
  (func (;23;) (type 4) (param i32 i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 1
    local.get 2
    return_call $ask_plus_10_rest
  )
  (func (;24;) (type 4) (param i32 i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 0
    i32.load offset=12
    local.get 1
    local.get 2
    return_call $force
  )
  (func (;25;) (type 5) (param i32 i32) (result i32)
    local.get 1
    return_call $ask_plus_10_ev
  )
  (func (;26;) (type 5) (param i32 i32) (result i32)
    local.get 0
    i32.load offset=8
    local.get 1
    return_call $doubler_start
  )
  (data (;0;) (i32.const 16) "\00\00\00\00\02\00\00\00\ff\ff\ff\ff\00\00\00\00")
)
