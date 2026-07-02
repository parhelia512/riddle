use crate::lower;

#[test]
fn simple_function_no_params() {
    let module = lower(
        r#"
        fun main() {
            let x = 42;
        }
        "#,
    );
    assert_eq!(module.function_order.len(), 1);
    let func = &module.functions[module.function_order[0]];
    assert_eq!(func.name, "main");
    assert_eq!(func.params.len(), 0);
}

#[test]
fn function_with_params() {
    let module = lower(
        r#"
        fun add(a: i32, b: i32) -> i32 {
            return a + b;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    assert_eq!(func.name, "add");
    assert_eq!(func.params.len(), 2);
    assert_eq!(func.params[0].name, "a");
    assert_eq!(func.params[1].name, "b");
}

#[test]
fn integer_literal() {
    let module = lower(
        r#"
        fun main() {
            let x = 42;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    // 入口块应当包含 const 指令
    let entry = &func.blocks[func.entry];
    assert!(
        !entry.insts.is_empty(),
        "entry block should have instructions"
    );
}

#[test]
fn string_literal() {
    let module = lower(
        r#"
        fun main() {
            let x = "hello";
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    assert!(!func.blocks[func.entry].insts.is_empty());
}

#[test]
fn array_repeat_lowers_to_array_value() {
    let module = lower(
        r#"
        fun main() {
            let xs: [i32; 4] = [5; 4];
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    let repeated = entry.insts.iter().find_map(|inst| match &inst.kind {
        mir::instr::InstKind::ArrayValue(values) => Some(values),
        _ => None,
    });

    assert!(
        matches!(repeated, Some(values) if values.len() == 4 && values.iter().all(|v| *v == values[0])),
        "expected repeated ArrayValue, got {:?}",
        entry.insts.iter().map(|i| &i.kind).collect::<Vec<_>>()
    );
}

#[test]
fn array_for_loop_lowers_to_indexed_loop() {
    let module = lower(
        r#"
        fun main() {
            let mut sum = 0;
            let values = [1, 2, 3];
            for item in values {
                sum += item;
            }
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let has_loop_branch = func
        .blocks
        .iter()
        .any(|(_, block)| matches!(block.terminator, mir::instr::Terminator::CondBranch(..)));
    let has_index_ptr = func.blocks.iter().any(|(_, block)| {
        block
            .insts
            .iter()
            .any(|inst| matches!(inst.kind, mir::instr::InstKind::IndexPtr(..)))
    });

    assert!(has_loop_branch, "{func:#?}");
    assert!(has_index_ptr, "{func:#?}");
}

#[test]
fn if_expression_creates_blocks() {
    let module = lower(
        r#"
        fun choose(flag: bool) -> i32 {
            if flag {
                return 1;
            }
            return 0;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    // if 应当产生多个基本块
    let block_count = func.blocks.iter().count();
    assert!(
        block_count >= 2,
        "expected at least 2 blocks for if, got {}",
        block_count
    );
}

#[test]
fn while_loop_creates_blocks() {
    let module = lower(
        r#"
        fun loop_test() {
            let x = true;
            while x {
                let x = false;
            }
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    // while 应当产生至少 3 个块 (cond, body, exit)
    let block_count = func.blocks.iter().count();
    assert!(
        block_count >= 3,
        "expected at least 3 blocks for while, got {}",
        block_count
    );
}

#[test]
fn arithmetic_operations() {
    let module = lower(
        r#"
        fun compute(a: i32, b: i32) -> i32 {
            let c = a + b;
            let d = c * 2;
            let e = d - 1;
            return e / 3;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    // 应当有多条指令
    assert!(entry.insts.len() >= 4, "expected at least 4 instructions");
}

#[test]
fn i32_add_lowers_to_builtin_binop() {
    let module = lower(
        r#"
        fun main() {
            let a: i32 = 1;
            let b: i32 = 2;
            let sum = a + b;
        }
        "#,
    );
    let func = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|func| func.name == "main")
        .unwrap();
    let entry = &func.blocks[func.entry];

    assert!(entry.insts.iter().any(|i| matches!(
        i.kind,
        mir::instr::InstKind::BinOp(mir::instr::BinOp::Add, _, _)
    )));
}

#[test]
fn overloaded_add_lowers_to_method_call() {
    let module = lower(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        impl Add for i32 {
            type Output = i32;
            fun add(self, rhs: Self) -> Self::Output {
                self + rhs
            }
        }

        struct Box<T> {
            value: T,
        }

        impl<T: Add<Output = T>> Add for Box<T> {
            type Output = T;

            fun add(self, rhs: Self) -> Self::Output {
                self.value + rhs.value
            }
        }

        fun main() {
            let a: Box<i32> = Box { value: 1 };
            let b: Box<i32> = Box { value: 2 };
            let sum = a + b;
        }
        "#,
    );
    let func = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|func| func.name == "main")
        .unwrap();
    let entry = &func.blocks[func.entry];

    assert!(entry.insts.iter().any(|i| matches!(
        &i.kind,
        mir::instr::InstKind::Call(mir::FuncRef::Local(name), _) if name.starts_with("add")
    )));
    assert!(
        !entry.insts.iter().any(|i| matches!(
            i.kind,
            mir::instr::InstKind::BinOp(mir::instr::BinOp::Add, _, _)
        )),
        "overloaded add should call Add::add, got {:?}",
        entry.insts.iter().map(|i| &i.kind).collect::<Vec<_>>()
    );
}

#[test]
fn enum_variant_constructor_lowers_to_discriminant() {
    let module = lower(
        r#"
        enum Option<T> {
            Some(T),
            None,
        }

        fun make() -> Option<i32> {
            Option::Some(1)
        }
        "#,
    );
    let func = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|func| func.name == "make")
        .unwrap();
    let entry = &func.blocks[func.entry];

    assert!(!entry.insts.iter().any(|i| matches!(
        &i.kind,
        mir::instr::InstKind::Call(mir::FuncRef::Local(name), _) if name == "Option::Some"
    )));
}

#[test]
fn compound_assignment_lowers_to_load_binop_store() {
    let module = lower(
        r#"
        fun main() {
            let mut n: i32 = 1;
            n += 2;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];

    assert!(
        entry
            .insts
            .iter()
            .any(|i| matches!(i.kind, mir::instr::InstKind::Load(_)))
    );
    assert!(entry.insts.iter().any(|i| matches!(
        i.kind,
        mir::instr::InstKind::BinOp(mir::instr::BinOp::Add, _, _)
    )));
    assert!(
        entry
            .insts
            .iter()
            .any(|i| matches!(i.kind, mir::instr::InstKind::Store(_, _)))
    );
}

#[test]
fn struct_literal() {
    let module = lower(
        r#"
        struct Point { x: i32, y: i32 }

        fun make() -> Point {
            let p = Point { x: 1, y: 2 };
            return p;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    assert!(!func.blocks[func.entry].insts.is_empty());
}

#[test]
fn field_access() {
    let module = lower(
        r#"
        struct Point { x: i32, y: i32 }

        fun get_x(p: &Point) -> i32 {
            return p.x;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    assert!(!func.blocks[func.entry].insts.is_empty());
}

#[test]
fn function_call() {
    let module = lower(
        r#"
        fun square(n: i32) -> i32 {
            return n * n;
        }

        fun main() -> i32 {
            return square(5);
        }
        "#,
    );
    let func = &module.functions[module.function_order[1]];
    assert!(!func.blocks[func.entry].insts.is_empty());
}

#[test]
fn multiple_functions() {
    let module = lower(
        r#"
        fun a() {}
        fun b() {}
        fun c() {}
        "#,
    );
    assert_eq!(module.function_order.len(), 3);
}

#[test]
fn empty_function() {
    let module = lower(
        r#"
        fun nothing() {}
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    // 空函数应当至少有一个 return 终止指令
    assert!(
        matches!(entry.terminator, mir::instr::Terminator::Return(_)),
        "empty function should end with return"
    );
}

#[test]
fn comparison_operators() {
    let module = lower(
        r#"
        fun cmp(a: i32, b: i32) -> bool {
            return a < b;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    assert!(!func.blocks[func.entry].insts.is_empty());
}

#[test]
fn bool_literal() {
    let module = lower(
        r#"
        fun truth() -> bool {
            return true;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    assert!(!func.blocks[func.entry].insts.is_empty());
}

#[test]
fn escape_analysis_affects_allocation() {
    let module = lower(
        r#"
        struct Data { value: i32 }

        fun keep() {
            let local = Data { value: 1 };
            // local 不逃逸，应当栈分配
        }

        fun escape() -> &Data {
            let local = Data { value: 1 };
            return &local;
            // local 逃逸，应当堆分配
        }
        "#,
    );
    assert_eq!(module.function_order.len(), 2);
}

#[test]
fn param_used_in_return() {
    let module = lower(
        r#"
        fun identity(n: i32) -> i32 {
            return n;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    // Should contain "return" with the param value, not unit
    assert!(
        matches!(entry.terminator, mir::instr::Terminator::Return(Some(_))),
        "param should be used in return, got {:?}",
        entry.terminator
    );
}

#[test]
fn param_used_in_expression() {
    let module = lower(
        r#"
        fun double(n: i32) -> i32 {
            let d = n + n;
            return d;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    assert!(entry.insts.len() >= 2, "expected param load + add + return");
    assert!(
        matches!(entry.terminator, mir::instr::Terminator::Return(Some(_))),
        "should return a value, got {:?}",
        entry.terminator
    );
}

#[test]
fn local_var_used_as_init() {
    let module = lower(
        r#"
        fun f() -> i32 {
            let x = 42;
            let y = x;
            return y;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    assert!(
        matches!(entry.terminator, mir::instr::Terminator::Return(Some(_))),
        "local var chain should resolve, got {:?}",
        entry.terminator
    );
}

#[test]
fn let_without_init_binds_unit() {
    let module = lower(
        r#"
        fun f() {
            let x: i32;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    // Should not panic — let without init maps to unit_const
    assert_eq!(func.name, "f");
}

#[test]
fn two_params_both_used() {
    let module = lower(
        r#"
        fun add(a: i32, b: i32) -> i32 {
            return a + b;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    assert_eq!(func.params.len(), 2);
    let entry = &func.blocks[func.entry];
    assert!(
        entry
            .insts
            .iter()
            .any(|i| matches!(i.kind, mir::instr::InstKind::BinOp(..))),
        "expected a BinOp instruction for a + b, got {:?}",
        entry.insts.iter().map(|i| &i.kind).collect::<Vec<_>>()
    );
}

#[test]
fn escaping_local_produces_heap_alloc_instruction() {
    let module = lower(
        r#"
        struct Data { value: i32 }

        fun escape() -> &Data {
            let local = Data { value: 1 };
            return &local;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    let has_heap_alloc = entry
        .insts
        .iter()
        .any(|i| matches!(i.kind, mir::instr::InstKind::HeapAlloc(_)));
    assert!(
        has_heap_alloc,
        "escaping local should produce HeapAlloc, got: {:?}",
        entry.insts.iter().map(|i| &i.kind).collect::<Vec<_>>()
    );
}

#[test]
fn non_escaping_local_no_heap_alloc() {
    let module = lower(
        r#"
        struct Data { value: i32 }

        fun keep() {
            let local = Data { value: 1 };
            // local doesn't escape — must be stack allocated (no HeapAlloc)
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    let has_heap_alloc = entry
        .insts
        .iter()
        .any(|i| matches!(i.kind, mir::instr::InstKind::HeapAlloc(_)));
    assert!(
        !has_heap_alloc,
        "non-escaping local should NOT produce HeapAlloc"
    );
}

#[test]
fn pos_unary_is_noop() {
    let module = lower(
        r#"
        fun f(x: i32) -> i32 {
            let y = +x;
            return y;
        }
        "#,
    );
    let func = &module.functions[module.function_order[0]];
    let entry = &func.blocks[func.entry];
    // +x should not produce a Neg instruction
    let has_neg = entry
        .insts
        .iter()
        .any(|i| matches!(i.kind, mir::instr::InstKind::UnOp(mir::instr::UnOp::Neg, _)));
    assert!(!has_neg, "+x should not produce Neg instruction");
    assert!(
        matches!(entry.terminator, mir::instr::Terminator::Return(Some(_))),
        "should return a value, got {:?}",
        entry.terminator
    );
}
