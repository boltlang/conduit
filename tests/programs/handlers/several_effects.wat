(module
  (type (;0;) (func (param i32 i32) (result i32)))
  (type (;1;) (func (param i32 i32)))
  (type (;2;) (func (param i32 i32 i32)))
  (type (;3;) (func (param i32) (result i32)))
  (type (;4;) (func (param i32 i32 i32) (result i32)))
  (type (;5;) (func (result i32)))
  (type (;6;) (func (param i32)))
  (table (;0;) 19 19 funcref)
  (memory (;0;) 1)
  (tag (;0;) (type 6) (param i32))
  (global (;0;) (mut i32) i32.const 16)
  (global (;1;) (mut i32) i32.const 0)
  (global (;2;) (mut i32) i32.const 0)
  (export "fn15" (func $issue_export))
  (export "fn17" (func $log_reads_outer_export))
  (export "fn20" (func $log_then_fail_export))
  (export "fn22" (func $nested_export))
  (export "memory" (memory 0))
  (elem (;0;) (table 0) (i32.const 0) func 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45)
  (func $get (;0;) (type 0) (param i32 i32) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32)
    global.get 1
    local.tee 5
    local.set 6
    block ;; label = @1
      loop ;; label = @2
        local.get 6
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 6
        i32.load offset=8
        local.set 8
        local.get 8
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 2
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 5
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 7
          br 2 (;@1;)
        end
        local.get 6
        i32.load offset=12
        local.set 6
        br 0 (;@2;)
      end
    end
    local.get 6
    i32.load offset=12
    global.set 1
    local.get 6
    i32.const 100
    local.get 6
    local.get 7
    i32.add
    i32.load
    call_indirect (type 1)
    local.get 5
    global.set 1
    local.get 0
    i32.load offset=8
    local.set 3
    local.get 3
    return
  )
  (func $get_plain (;1;) (type 0) (param i32 i32) (result i32)
    (local i32 i32 i32)
    local.get 0
    i32.load offset=8
    local.set 3
    local.get 3
    return
  )
  (func $set (;2;) (type 2) (param i32 i32 i32)
    (local i32 i32)
    local.get 0
    local.get 2
    i32.store offset=8
    return
  )
  (func $set_plain (;3;) (type 2) (param i32 i32 i32)
    (local i32 i32)
    local.get 0
    local.get 2
    i32.store offset=8
    return
  )
  (func $log (;4;) (type 2) (param i32 i32 i32)
    (local i32 i32 i32 i32 i32)
    local.get 1
    i32.load offset=8
    local.set 4
    local.get 4
    i32.const 10
    i32.mul
    local.set 5
    local.get 5
    local.get 2
    i32.add
    local.set 6
    local.get 1
    local.get 6
    i32.store offset=8
    return
  )
  (func $log_reads (;5;) (type 2) (param i32 i32 i32)
    (local i32 i32 i32 i32 i32 i32 i32)
    global.get 1
    local.tee 6
    local.set 7
    block ;; label = @1
      loop ;; label = @2
        local.get 7
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 7
        i32.load offset=8
        local.set 9
        local.get 9
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 8
          br 2 (;@1;)
        end
        local.get 9
        i32.const 3
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 8
          br 2 (;@1;)
        end
        local.get 9
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 8
          br 2 (;@1;)
        end
        local.get 7
        i32.load offset=12
        local.set 7
        br 0 (;@2;)
      end
    end
    local.get 7
    i32.load offset=12
    global.set 1
    local.get 7
    local.get 7
    local.get 8
    i32.add
    i32.load
    call_indirect (type 3)
    local.get 6
    global.set 1
    local.set 4
    local.get 1
    local.get 4
    i32.store offset=8
    return
  )
  (func $job_done (;6;) (type 4) (param i32 i32 i32) (result i32)
    local.get 2
    return
  )
  (func $add_log (;7;) (type 1) (param i32 i32)
    (local i32 i32 i32 i32)
    local.get 0
    i32.load offset=8
    local.set 3
    local.get 3
    local.get 1
    i32.add
    local.set 4
    local.get 0
    local.get 4
    i32.store offset=8
    return
  )
  (func $sum_done (;8;) (type 0) (param i32 i32) (result i32)
    local.get 1
    return
  )
  (func $fixed_get (;9;) (type 3) (param i32) (result i32)
    (local i32 i32)
    local.get 0
    return
  )
  (func $fixed_set (;10;) (type 1) (param i32 i32)
    (local i32 i32)
    return
  )
  (func $fixed_done (;11;) (type 0) (param i32 i32) (result i32)
    local.get 1
    return
  )
  (func $fail (;12;) (type 3) (param i32) (result i32)
    (local i32)
    i32.const -1
    return
  )
  (func $record (;13;) (type 1) (param i32 i32)
    (local i32 i32)
    local.get 0
    local.get 1
    i32.store offset=8
    return
  )
  (func $work (;14;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    global.get 1
    local.tee 3
    local.set 4
    block ;; label = @1
      loop ;; label = @2
        local.get 4
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 4
        i32.load offset=8
        local.set 6
        local.get 6
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 5
          br 2 (;@1;)
        end
        local.get 6
        i32.const 2
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 5
          br 2 (;@1;)
        end
        local.get 6
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 5
          br 2 (;@1;)
        end
        local.get 6
        i32.const 5
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 5
          br 2 (;@1;)
        end
        local.get 4
        i32.load offset=12
        local.set 4
        br 0 (;@2;)
      end
    end
    local.get 4
    i32.load offset=12
    global.set 1
    local.get 4
    i32.const 1
    local.get 4
    local.get 5
    i32.add
    i32.load
    call_indirect (type 1)
    local.get 3
    global.set 1
    global.get 1
    local.tee 7
    local.set 8
    block ;; label = @1
      loop ;; label = @2
        local.get 8
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 8
        i32.load offset=8
        local.set 10
        local.get 10
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 9
          br 2 (;@1;)
        end
        local.get 10
        i32.const 3
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 9
          br 2 (;@1;)
        end
        local.get 10
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 9
          br 2 (;@1;)
        end
        local.get 8
        i32.load offset=12
        local.set 8
        br 0 (;@2;)
      end
    end
    local.get 8
    i32.load offset=12
    global.set 1
    local.get 8
    local.get 8
    local.get 9
    i32.add
    i32.load
    call_indirect (type 3)
    local.get 7
    global.set 1
    local.set 0
    local.get 0
    i32.const 1
    i32.add
    local.set 1
    global.get 1
    local.tee 11
    local.set 12
    block ;; label = @1
      loop ;; label = @2
        local.get 12
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 12
        i32.load offset=8
        local.set 14
        local.get 14
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 13
          br 2 (;@1;)
        end
        local.get 14
        i32.const 3
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 13
          br 2 (;@1;)
        end
        local.get 14
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 13
          br 2 (;@1;)
        end
        local.get 12
        i32.load offset=12
        local.set 12
        br 0 (;@2;)
      end
    end
    local.get 12
    i32.load offset=12
    global.set 1
    local.get 12
    local.get 1
    local.get 12
    local.get 13
    i32.add
    i32.load
    call_indirect (type 1)
    local.get 11
    global.set 1
    global.get 1
    local.tee 15
    local.set 16
    block ;; label = @1
      loop ;; label = @2
        local.get 16
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 16
        i32.load offset=8
        local.set 18
        local.get 18
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 17
          br 2 (;@1;)
        end
        local.get 18
        i32.const 3
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 17
          br 2 (;@1;)
        end
        local.get 18
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 17
          br 2 (;@1;)
        end
        local.get 16
        i32.load offset=12
        local.set 16
        br 0 (;@2;)
      end
    end
    local.get 16
    i32.load offset=12
    global.set 1
    local.get 16
    local.get 16
    local.get 17
    i32.add
    i32.load
    call_indirect (type 3)
    local.get 15
    global.set 1
    local.set 2
    global.get 1
    local.tee 19
    local.set 20
    block ;; label = @1
      loop ;; label = @2
        local.get 20
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 20
        i32.load offset=8
        local.set 22
        local.get 22
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 21
          br 2 (;@1;)
        end
        local.get 22
        i32.const 2
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 21
          br 2 (;@1;)
        end
        local.get 22
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 21
          br 2 (;@1;)
        end
        local.get 22
        i32.const 5
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 21
          br 2 (;@1;)
        end
        local.get 20
        i32.load offset=12
        local.set 20
        br 0 (;@2;)
      end
    end
    local.get 20
    i32.load offset=12
    global.set 1
    local.get 20
    i32.const 2
    local.get 20
    local.get 21
    i32.add
    i32.load
    call_indirect (type 1)
    local.get 19
    global.set 1
    local.get 2
    return
  )
  (func $issue (;15;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 6
    local.get 6
    i32.const 0
    i32.store offset=8
    local.get 6
    local.set 0
    i32.const 8
    call $alloc
    local.set 7
    local.get 7
    i32.const 11
    i32.store offset=4
    local.get 7
    local.set 1
    global.get 1
    local.set 9
    i32.const 24
    call $alloc
    local.set 10
    local.get 10
    i32.const 2
    i32.store offset=8
    local.get 10
    local.get 9
    i32.store offset=12
    local.get 10
    i32.const 3
    i32.store offset=16
    local.get 10
    local.get 0
    i32.store offset=20
    local.get 10
    global.set 1
    local.get 1
    local.get 1
    i32.load offset=4
    call_indirect (type 3)
    local.set 8
    local.get 9
    global.set 1
    local.get 0
    local.get 8
    call $sum_done
    local.set 2
    local.get 0
    i32.load offset=8
    local.set 3
    local.get 3
    i32.const 1000
    i32.mul
    local.set 4
    local.get 2
    local.get 4
    i32.add
    local.set 5
    local.get 5
    return
  )
  (func $in_job (;16;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 7
    local.get 7
    i32.const 5
    i32.store offset=8
    local.get 7
    local.set 0
    i32.const 16
    call $alloc
    local.set 8
    local.get 8
    i32.const 0
    i32.store offset=8
    local.get 8
    local.set 1
    i32.const 8
    call $alloc
    local.set 9
    local.get 9
    i32.const 12
    i32.store offset=4
    local.get 9
    local.set 2
    global.get 1
    local.set 11
    i32.const 40
    call $alloc
    local.set 12
    local.get 12
    i32.const 1
    i32.store offset=8
    local.get 12
    local.get 11
    i32.store offset=12
    local.get 12
    i32.const 0
    i32.store offset=16
    local.get 12
    i32.const 1
    i32.store offset=20
    local.get 12
    i32.const 2
    i32.store offset=24
    local.get 12
    local.get 0
    i32.store offset=28
    local.get 12
    local.get 1
    i32.store offset=32
    local.get 12
    global.set 1
    local.get 2
    local.get 2
    i32.load offset=4
    call_indirect (type 3)
    local.set 10
    local.get 11
    global.set 1
    local.get 0
    local.get 1
    local.get 10
    call $job_done
    local.set 3
    local.get 1
    i32.load offset=8
    local.set 4
    local.get 4
    i32.const 10
    i32.mul
    local.set 5
    local.get 3
    local.get 5
    i32.add
    local.set 6
    local.get 6
    return
  )
  (func $log_reads_outer (;17;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32)
    i32.const 8
    call $alloc
    local.set 2
    local.get 2
    i32.const 13
    i32.store offset=4
    local.get 2
    local.set 0
    global.get 1
    local.set 4
    i32.const 32
    call $alloc
    local.set 5
    local.get 5
    i32.const 3
    i32.store offset=8
    local.get 5
    local.get 4
    i32.store offset=12
    local.get 5
    i32.const 4
    i32.store offset=16
    local.get 5
    i32.const 5
    i32.store offset=20
    local.get 5
    i32.const 7
    i32.store offset=24
    local.get 5
    global.set 1
    local.get 0
    local.get 0
    i32.load offset=4
    call_indirect (type 3)
    local.set 3
    local.get 4
    global.set 1
    i32.const 7
    local.get 3
    return_call $fixed_done
  )
  (func $in_log_state (;18;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 7
    local.get 7
    i32.const 5
    i32.store offset=8
    local.get 7
    local.set 0
    i32.const 16
    call $alloc
    local.set 8
    local.get 8
    i32.const 0
    i32.store offset=8
    local.get 8
    local.set 1
    i32.const 8
    call $alloc
    local.set 9
    local.get 9
    i32.const 14
    i32.store offset=4
    local.get 9
    local.set 2
    global.get 1
    local.set 11
    i32.const 40
    call $alloc
    local.set 12
    local.get 12
    i32.const 4
    i32.store offset=8
    local.get 12
    local.get 11
    i32.store offset=12
    local.get 12
    i32.const 6
    i32.store offset=16
    local.get 12
    i32.const 7
    i32.store offset=20
    local.get 12
    i32.const 8
    i32.store offset=24
    local.get 12
    local.get 0
    i32.store offset=28
    local.get 12
    local.get 1
    i32.store offset=32
    local.get 12
    global.set 1
    local.get 2
    local.get 2
    i32.load offset=4
    call_indirect (type 3)
    local.set 10
    local.get 11
    global.set 1
    local.get 0
    local.get 1
    local.get 10
    call $job_done
    local.set 3
    local.get 1
    i32.load offset=8
    local.set 4
    local.get 4
    i32.const 10
    i32.mul
    local.set 5
    local.get 3
    local.get 5
    i32.add
    local.set 6
    local.get 6
    return
  )
  (func $log_then_get (;19;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32)
    global.get 1
    local.tee 1
    local.set 2
    block ;; label = @1
      loop ;; label = @2
        local.get 2
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 2
        i32.load offset=8
        local.set 4
        local.get 4
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 3
          br 2 (;@1;)
        end
        local.get 4
        i32.const 2
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 3
          br 2 (;@1;)
        end
        local.get 4
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 3
          br 2 (;@1;)
        end
        local.get 4
        i32.const 5
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 3
          br 2 (;@1;)
        end
        local.get 2
        i32.load offset=12
        local.set 2
        br 0 (;@2;)
      end
    end
    local.get 2
    i32.load offset=12
    global.set 1
    local.get 2
    i32.const 0
    local.get 2
    local.get 3
    i32.add
    i32.load
    call_indirect (type 1)
    local.get 1
    global.set 1
    global.get 1
    local.tee 5
    local.set 6
    block ;; label = @1
      loop ;; label = @2
        local.get 6
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 6
        i32.load offset=8
        local.set 8
        local.get 8
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 3
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 7
          br 2 (;@1;)
        end
        local.get 6
        i32.load offset=12
        local.set 6
        br 0 (;@2;)
      end
    end
    local.get 6
    i32.load offset=12
    global.set 1
    local.get 6
    local.get 6
    local.get 7
    i32.add
    i32.load
    call_indirect (type 3)
    local.get 5
    global.set 1
    local.set 0
    local.get 0
    return
  )
  (func $log_then_fail (;20;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 6
    local.get 6
    i32.const 0
    i32.store offset=8
    local.get 6
    local.set 0
    i32.const 8
    call $alloc
    local.set 7
    local.get 7
    i32.const 15
    i32.store offset=4
    local.get 7
    local.set 1
    global.get 1
    local.set 9
    i32.const 32
    call $alloc
    local.set 10
    local.get 10
    i32.const 5
    i32.store offset=8
    local.get 10
    local.get 9
    i32.store offset=12
    local.get 10
    i32.const 9
    i32.store offset=16
    local.get 10
    i32.const 10
    i32.store offset=20
    local.get 10
    local.get 0
    i32.store offset=24
    local.get 10
    global.set 1
    block ;; label = @1
      block (result i32) ;; label = @2
        try_table (catch 0 0 (;@2;)) ;; label = @3
          local.get 1
          local.get 1
          i32.load offset=4
          call_indirect (type 3)
          local.set 8
        end
        local.get 9
        global.set 1
        local.get 0
        local.get 8
        call $sum_done
        local.set 11
        br 1 (;@1;)
      end
      local.tee 12
      local.get 10
      i32.ne
      if ;; label = @2
        local.get 12
        throw 0
      end
      local.get 9
      global.set 1
      local.get 10
      i32.load offset=28
      local.set 11
    end
    local.get 11
    local.set 2
    local.get 0
    i32.load offset=8
    local.set 3
    local.get 3
    i32.const 10
    i32.mul
    local.set 4
    local.get 4
    local.get 2
    i32.add
    local.set 5
    local.get 5
    return
  )
  (func $logs_then_fails (;21;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32)
    global.get 1
    local.tee 0
    local.set 1
    block ;; label = @1
      loop ;; label = @2
        local.get 1
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 1
        i32.load offset=8
        local.set 3
        local.get 3
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 2
          br 2 (;@1;)
        end
        local.get 3
        i32.const 2
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 2
          br 2 (;@1;)
        end
        local.get 3
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 2
          br 2 (;@1;)
        end
        local.get 3
        i32.const 5
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 2
          br 2 (;@1;)
        end
        local.get 1
        i32.load offset=12
        local.set 1
        br 0 (;@2;)
      end
    end
    local.get 1
    i32.load offset=12
    global.set 1
    local.get 1
    i32.const 5
    local.get 1
    local.get 2
    i32.add
    i32.load
    call_indirect (type 1)
    local.get 0
    global.set 1
    global.get 1
    local.tee 4
    local.set 5
    block ;; label = @1
      loop ;; label = @2
        local.get 5
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 5
        i32.load offset=8
        i32.const 5
        i32.eq
        br_if 1 (;@1;)
        local.get 5
        i32.load offset=12
        local.set 5
        br 0 (;@2;)
      end
    end
    local.get 5
    i32.load offset=12
    global.set 1
    local.get 5
    local.get 5
    i32.load offset=16
    call_indirect (type 6)
    local.get 4
    global.set 1
    i32.const 1000
    return
  )
  (func $nested (;22;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 3
    local.get 3
    i32.const 0
    i32.store offset=8
    local.get 3
    local.set 0
    i32.const 8
    call $alloc
    local.set 4
    local.get 4
    i32.const 16
    i32.store offset=4
    local.get 4
    local.set 1
    global.get 1
    local.set 6
    i32.const 24
    call $alloc
    local.set 7
    local.get 7
    i32.const 2
    i32.store offset=8
    local.get 7
    local.get 6
    i32.store offset=12
    local.get 7
    i32.const 3
    i32.store offset=16
    local.get 7
    local.get 0
    i32.store offset=20
    local.get 7
    global.set 1
    local.get 1
    local.get 1
    i32.load offset=4
    call_indirect (type 3)
    local.set 5
    local.get 6
    global.set 1
    local.get 0
    local.get 5
    return_call $sum_done
  )
  (func $outer_job (;23;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 7
    local.get 7
    i32.const 1
    i32.store offset=8
    local.get 7
    local.set 0
    i32.const 16
    call $alloc
    local.set 8
    local.get 8
    i32.const 0
    i32.store offset=8
    local.get 8
    local.set 1
    i32.const 8
    call $alloc
    local.set 9
    local.get 9
    i32.const 17
    i32.store offset=4
    local.get 9
    local.set 2
    global.get 1
    local.set 11
    i32.const 40
    call $alloc
    local.set 12
    local.get 12
    i32.const 1
    i32.store offset=8
    local.get 12
    local.get 11
    i32.store offset=12
    local.get 12
    i32.const 0
    i32.store offset=16
    local.get 12
    i32.const 1
    i32.store offset=20
    local.get 12
    i32.const 2
    i32.store offset=24
    local.get 12
    local.get 0
    i32.store offset=28
    local.get 12
    local.get 1
    i32.store offset=32
    local.get 12
    global.set 1
    local.get 2
    local.get 2
    i32.load offset=4
    call_indirect (type 3)
    local.set 10
    local.get 11
    global.set 1
    local.get 0
    local.get 1
    local.get 10
    call $job_done
    local.set 3
    local.get 1
    i32.load offset=8
    local.set 4
    local.get 4
    i32.const 100
    i32.mul
    local.set 5
    local.get 3
    local.get 5
    i32.add
    local.set 6
    local.get 6
    return
  )
  (func $inner_job (;24;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32 i32)
    i32.const 16
    call $alloc
    local.set 7
    local.get 7
    i32.const 2
    i32.store offset=8
    local.get 7
    local.set 0
    i32.const 16
    call $alloc
    local.set 8
    local.get 8
    i32.const 0
    i32.store offset=8
    local.get 8
    local.set 1
    i32.const 8
    call $alloc
    local.set 9
    local.get 9
    i32.const 18
    i32.store offset=4
    local.get 9
    local.set 2
    global.get 1
    local.set 11
    i32.const 40
    call $alloc
    local.set 12
    local.get 12
    i32.const 1
    i32.store offset=8
    local.get 12
    local.get 11
    i32.store offset=12
    local.get 12
    i32.const 0
    i32.store offset=16
    local.get 12
    i32.const 1
    i32.store offset=20
    local.get 12
    i32.const 2
    i32.store offset=24
    local.get 12
    local.get 0
    i32.store offset=28
    local.get 12
    local.get 1
    i32.store offset=32
    local.get 12
    global.set 1
    local.get 2
    local.get 2
    i32.load offset=4
    call_indirect (type 3)
    local.set 10
    local.get 11
    global.set 1
    local.get 0
    local.get 1
    local.get 10
    call $job_done
    local.set 3
    local.get 1
    i32.load offset=8
    local.set 4
    local.get 4
    i32.const 10
    i32.mul
    local.set 5
    local.get 3
    local.get 5
    i32.add
    local.set 6
    local.get 6
    return
  )
  (func $get_then_log (;25;) (type 5) (result i32)
    (local i32 i32 i32 i32 i32 i32 i32 i32 i32)
    global.get 1
    local.tee 1
    local.set 2
    block ;; label = @1
      loop ;; label = @2
        local.get 2
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 2
        i32.load offset=8
        local.set 4
        local.get 4
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 3
          br 2 (;@1;)
        end
        local.get 4
        i32.const 3
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 3
          br 2 (;@1;)
        end
        local.get 4
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 3
          br 2 (;@1;)
        end
        local.get 2
        i32.load offset=12
        local.set 2
        br 0 (;@2;)
      end
    end
    local.get 2
    i32.load offset=12
    global.set 1
    local.get 2
    local.get 2
    local.get 3
    i32.add
    i32.load
    call_indirect (type 3)
    local.get 1
    global.set 1
    local.set 0
    global.get 1
    local.tee 5
    local.set 6
    block ;; label = @1
      loop ;; label = @2
        local.get 6
        i32.eqz
        if ;; label = @3
          unreachable
        end
        local.get 6
        i32.load offset=8
        local.set 8
        local.get 8
        i32.const 1
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 2
        i32.eq
        if ;; label = @3
          i32.const 16
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 4
        i32.eq
        if ;; label = @3
          i32.const 24
          local.set 7
          br 2 (;@1;)
        end
        local.get 8
        i32.const 5
        i32.eq
        if ;; label = @3
          i32.const 20
          local.set 7
          br 2 (;@1;)
        end
        local.get 6
        i32.load offset=12
        local.set 6
        br 0 (;@2;)
      end
    end
    local.get 6
    i32.load offset=12
    global.set 1
    local.get 6
    i32.const 3
    local.get 6
    local.get 7
    i32.add
    i32.load
    call_indirect (type 1)
    local.get 5
    global.set 1
    local.get 0
    return
  )
  (func $alloc (;26;) (type 3) (param i32) (result i32)
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
  (func (;27;) (type 3) (param i32) (result i32)
    local.get 0
    i32.load offset=28
    local.get 0
    i32.load offset=32
    return_call $get
  )
  (func (;28;) (type 1) (param i32 i32)
    local.get 0
    i32.load offset=28
    local.get 0
    i32.load offset=32
    local.get 1
    return_call $set
  )
  (func (;29;) (type 1) (param i32 i32)
    local.get 0
    i32.load offset=28
    local.get 0
    i32.load offset=32
    local.get 1
    return_call $log
  )
  (func (;30;) (type 1) (param i32 i32)
    local.get 0
    i32.load offset=20
    local.get 1
    return_call $add_log
  )
  (func (;31;) (type 3) (param i32) (result i32)
    local.get 0
    i32.load offset=24
    return_call $fixed_get
  )
  (func (;32;) (type 1) (param i32 i32)
    local.get 0
    i32.load offset=24
    local.get 1
    return_call $fixed_set
  )
  (func (;33;) (type 3) (param i32) (result i32)
    local.get 0
    i32.load offset=28
    local.get 0
    i32.load offset=32
    return_call $get_plain
  )
  (func (;34;) (type 1) (param i32 i32)
    local.get 0
    i32.load offset=28
    local.get 0
    i32.load offset=32
    local.get 1
    return_call $set_plain
  )
  (func (;35;) (type 1) (param i32 i32)
    local.get 0
    i32.load offset=28
    local.get 0
    i32.load offset=32
    local.get 1
    return_call $log_reads
  )
  (func (;36;) (type 6) (param i32)
    (local i32)
    local.get 0
    i32.load offset=24
    call $fail
    local.set 1
    local.get 0
    local.get 1
    i32.store offset=28
    local.get 0
    throw 0
  )
  (func (;37;) (type 1) (param i32 i32)
    local.get 0
    i32.load offset=24
    local.get 1
    return_call $record
  )
  (func (;38;) (type 3) (param i32) (result i32)
    return_call $in_job
  )
  (func (;39;) (type 3) (param i32) (result i32)
    return_call $work
  )
  (func (;40;) (type 3) (param i32) (result i32)
    return_call $in_log_state
  )
  (func (;41;) (type 3) (param i32) (result i32)
    return_call $log_then_get
  )
  (func (;42;) (type 3) (param i32) (result i32)
    return_call $logs_then_fails
  )
  (func (;43;) (type 3) (param i32) (result i32)
    return_call $outer_job
  )
  (func (;44;) (type 3) (param i32) (result i32)
    return_call $inner_job
  )
  (func (;45;) (type 3) (param i32) (result i32)
    return_call $get_then_log
  )
  (func $issue_export (;46;) (type 5) (result i32)
    (local i32 i32)
    global.get 1
    local.set 0
    global.get 2
    local.set 1
    i32.const 0
    global.set 1
    call $issue
    local.get 0
    global.set 1
    local.get 1
    global.set 2
  )
  (func $log_reads_outer_export (;47;) (type 5) (result i32)
    (local i32 i32)
    global.get 1
    local.set 0
    global.get 2
    local.set 1
    i32.const 0
    global.set 1
    call $log_reads_outer
    local.get 0
    global.set 1
    local.get 1
    global.set 2
  )
  (func $log_then_fail_export (;48;) (type 5) (result i32)
    (local i32 i32)
    global.get 1
    local.set 0
    global.get 2
    local.set 1
    i32.const 0
    global.set 1
    call $log_then_fail
    local.get 0
    global.set 1
    local.get 1
    global.set 2
  )
  (func $nested_export (;49;) (type 5) (result i32)
    (local i32 i32)
    global.get 1
    local.set 0
    global.get 2
    local.set 1
    i32.const 0
    global.set 1
    call $nested
    local.get 0
    global.set 1
    local.get 1
    global.set 2
  )
)
