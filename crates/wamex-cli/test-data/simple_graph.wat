(module $simple_graph.wasm
  (type (;0;) (func (param i32)))
  (type (;1;) (func (param i32 i32)))
  (func $hello_world.1 (type 0) (param i32)
    local.get 0
    i32.const 13
    i32.store offset=4
    local.get 0
    i32.const 1048576
    i32.store)
  (func $version.1 (type 0) (param i32)
    local.get 0
    i32.const 5
    i32.store offset=4
    local.get 0
    i32.const 1048589
    i32.store)
  (func $constant.1 (type 0) (param i32)
    local.get 0
    i32.const 8
    i32.store offset=4
    local.get 0
    i32.const 1048594
    i32.store)
  (func $indirect_fn.1 (type 1) (param i32 i32)
    (local i32)
    global.get $__stack_pointer.1
    i32.const 16
    i32.sub
    local.tee 2
    global.set $__stack_pointer.1
    local.get 1
    i32.const 255
    i32.and
    i32.const 2
    i32.shl
    i32.const 1048604
    i32.add
    i32.load
    local.set 1
    local.get 2
    i32.const 8
    i32.add
    local.get 1
    call_indirect $__indirect_function_table (type 0)
    local.get 2
    i32.load offset=12
    local.set 1
    local.get 0
    local.get 2
    i32.load offset=8
    i32.store
    local.get 0
    local.get 1
    i32.store offset=4
    local.get 2
    i32.const 16
    i32.add
    global.set $__stack_pointer.1)
  (func $no_inline_fn.1 (type 1) (param i32 i32)
    (local i32 i32)
    global.get $__stack_pointer.1
    i32.const 32
    i32.sub
    local.tee 2
    global.set $__stack_pointer.1
    block  ;; label = @1
      block  ;; label = @2
        block  ;; label = @3
          block  ;; label = @4
            local.get 1
            i32.const 255
            i32.and
            br_table 0 (;@4;) 1 (;@3;) 2 (;@2;) 0 (;@4;)
          end
          local.get 2
          i32.const 8
          i32.add
          call $hello_world.1
          i32.const 1048576
          local.set 1
          i32.const 13
          local.set 3
          br 2 (;@1;)
        end
        local.get 2
        i32.const 16
        i32.add
        call $version.1
        i32.const 1048589
        local.set 1
        i32.const 5
        local.set 3
        br 1 (;@1;)
      end
      local.get 2
      i32.const 24
      i32.add
      call $constant.1
      i32.const 1048594
      local.set 1
      i32.const 8
      local.set 3
    end
    local.get 0
    local.get 3
    i32.store offset=4
    local.get 0
    local.get 1
    i32.store
    local.get 2
    i32.const 32
    i32.add
    global.set $__stack_pointer.1)
  (table $__indirect_function_table 4 4 funcref)
  (memory (;0;) 17)
  (global $__stack_pointer.1 (mut i32) (i32.const 1048576))
  (global (;1;) i32 (i32.const 1048616))
  (global (;2;) i32 (i32.const 1048624))
  (export "memory" (memory 0))
  (export "hello_world" (func $hello_world.1))
  (export "version" (func $version.1))
  (export "constant" (func $constant.1))
  (export "indirect_fn" (func $indirect_fn.1))
  (export "no_inline_fn" (func $no_inline_fn.1))
  (export "__data_end" (global 1))
  (export "__heap_base" (global 2))
  (elem (;0;) (i32.const 1) func $hello_world.1 $version.1 $constant.1)
  (data $.rodata (i32.const 1048576) "Hello, world!0.1.0constant\00\00\01\00\00\00\02\00\00\00\03\00\00\00"))
