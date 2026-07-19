use crate::{compile, lower};

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
    let string = func.blocks[func.entry]
        .insts
        .iter()
        .find(|inst| {
            matches!(
                inst.kind,
                mir::instr::InstKind::Const(mir::instr::ConstValue::String(_))
            )
        })
        .expect("missing string constant");
    assert_eq!(
        string.ty,
        mir::types::Type::Ref(Box::new(mir::types::Type::Str), false)
    );
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
fn generic_for_loop_lowers_to_iterator_calls() {
    let module = lower(
        r#"
        enum Option<T> {
            Some(T),
            None,
        }

        trait Iterator {
            type Item;
            fun next(&mut self) -> Option<Self::Item>;
        }

        trait IntoIterator {
            type Item;
            type IntoIter;
            fun into_iter(self) -> Self::IntoIter;
        }

        struct Counter {
            current: i32,
        }

        impl Iterator for Counter {
            type Item = i32;

            fun next(&mut self) -> Option<Self::Item> {
                if self.current < 3 {
                    let value = self.current;
                    self.current += 1;
                    Option::Some(value)
                } else {
                    Option::None
                }
            }
        }

        impl IntoIterator for Counter {
            type Item = i32;
            type IntoIter = Counter;

            fun into_iter(self) -> Self::IntoIter {
                self
            }
        }

        fun main() {
            let counter = Counter { current: 0 };
            for item in counter {
                let next = item + 1;
            }
        }
        "#,
    );
    let func = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|func| func.name == "main")
        .unwrap();

    let calls = func
        .blocks
        .iter()
        .flat_map(|(_, block)| block.insts.iter())
        .filter_map(|inst| match &inst.kind {
            mir::instr::InstKind::Call(mir::FuncRef::Local(name), _) => Some(name.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        calls.iter().any(|name| name.starts_with("into_iter")),
        "{func:#?}"
    );
    assert!(
        calls.iter().any(|name| name.starts_with("next")),
        "{func:#?}"
    );
    assert!(
        func.blocks
            .iter()
            .any(|(_, block)| matches!(block.terminator, mir::instr::Terminator::CondBranch(..)))
    );
}

#[test]
fn generic_function_for_loop_uses_concrete_iterator_impl_and_enum_layout() {
    let module = lower(
        r#"
        enum Option<T> {
            Spare(bool),
            Some(T),
            None,
        }

        trait Iterator {
            type Item;
            fun next(&mut self) -> Option<Self::Item>;
        }

        trait IntoIterator {
            type Item;
            type IntoIter;
            fun into_iter(self) -> Self::IntoIter;
        }

        struct Counter<T> { current: T }

        impl<T> Iterator for Counter<T> {
            type Item = T;
            fun next(&mut self) -> Option<Self::Item> { Option::None }
        }

        impl<T> IntoIterator for Counter<T> {
            type Item = T;
            type IntoIter = Counter<T>;
            fun into_iter(self) -> Self::IntoIter { self }
        }

        fun consume<T: IntoIterator<Item = i32, IntoIter = Counter<i32>>>(values: T) {
            for value in values {
                let next = value + 1;
            }
        }

        fun main() {
            consume(Counter { current: 0 });
        }
        "#,
    );
    let func = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|func| func.name == "consume__Counter_i32")
        .unwrap();
    let insts = func
        .blocks
        .iter()
        .flat_map(|(_, block)| block.insts.iter())
        .collect::<Vec<_>>();

    assert!(insts.iter().any(|inst| {
        matches!(&inst.kind, mir::instr::InstKind::Call(mir::FuncRef::Local(name), _) if name.starts_with("into_iter"))
    }));
    assert!(insts.iter().any(|inst| {
        matches!(&inst.kind, mir::instr::InstKind::Call(mir::FuncRef::Local(name), _) if name.starts_with("next"))
    }));
    assert!(
        insts
            .iter()
            .any(|inst| { matches!(inst.kind, mir::instr::InstKind::ExtractValue(_, 2)) })
    );
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
fn break_and_continue_lower_to_loop_targets_and_skip_dead_code() {
    let module = lower(
        r#"
        fun dead() {}

        fun main() {
            let mut i = 0;
            while i < 5 {
                i += 1;
                if i == 2 {
                    continue;
                    dead();
                }
                break;
                dead();
            }

            for item in [1, 2, 3] {
                if item == 1 {
                    continue;
                }
                break;
            }
        }
        "#,
    );
    let func = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|func| func.name == "main")
        .unwrap();
    let block_id = |label: &str| {
        func.blocks
            .iter()
            .find_map(|(id, block)| (block.label.as_deref() == Some(label)).then_some(id))
            .unwrap()
    };
    let branch_count = |target| {
        func.blocks
            .iter()
            .filter(|(_, block)| {
                matches!(block.terminator, mir::instr::Terminator::Branch(id) if id == target)
            })
            .count()
    };

    assert!(branch_count(block_id("while_cond")) >= 2, "{func:#?}");
    assert!(branch_count(block_id("while_exit")) >= 1, "{func:#?}");
    assert!(branch_count(block_id("for_array_step")) >= 1, "{func:#?}");
    assert!(branch_count(block_id("for_array_exit")) >= 1, "{func:#?}");
    assert!(!func.blocks.iter().any(|(_, block)| {
        block.insts.iter().any(|inst| {
            matches!(&inst.kind, mir::instr::InstKind::Call(mir::FuncRef::Local(name), _) if name == "dead")
        })
    }));
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
fn primitive_lang_operator_methods_lower_without_wrapper_functions() {
    let module = lower(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        #[lang = "neg"]
        trait Neg {
            type Output;
            fun neg(self) -> Self::Output;
        }

        #[lang = "add_assign"]
        trait AddAssign {
            fun add_assign(&mut self, rhs: Self);
        }

        impl Add for i32 {
            type Output = i32;
            fun add(self, rhs: Self) -> Self::Output { self + rhs }
        }

        impl Neg for i32 {
            type Output = i32;
            fun neg(self) -> Self::Output { -self }
        }

        impl AddAssign for i32 {
            fun add_assign(&mut self, rhs: Self) { *self += rhs; }
        }

        fun main() -> i32 {
            let mut value: i32 = 1;
            let sum = value.add(2);
            let negated = sum.neg();
            value.add_assign(3);
            value + negated
        }
        "#,
    );

    let wrappers = module
        .function_order
        .iter()
        .map(|fid| module.functions[*fid].name.as_str())
        .filter(|name| matches!(*name, "add__i32" | "neg__i32" | "add_assign__i32"))
        .collect::<Vec<_>>();
    assert!(wrappers.is_empty(), "unexpected wrappers: {wrappers:?}");
    let main = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|function| function.name == "main")
        .unwrap();
    let instructions = main
        .blocks
        .iter()
        .flat_map(|(_, block)| block.insts.iter())
        .collect::<Vec<_>>();

    assert!(instructions.iter().any(|instruction| matches!(
        instruction.kind,
        mir::instr::InstKind::BinOp(mir::instr::BinOp::Add, _, _)
    )));
    assert!(instructions.iter().any(|instruction| matches!(
        instruction.kind,
        mir::instr::InstKind::UnOp(mir::instr::UnOp::Neg, _)
    )));
    assert!(!instructions.iter().any(|instruction| matches!(
        &instruction.kind,
        mir::instr::InstKind::Call(mir::FuncRef::Local(name), _)
            if name == "add__i32" || name == "neg__i32" || name == "add_assign__i32"
    )));
    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction.kind, mir::instr::InstKind::Store(_, _)))
            .count(),
        2,
        "initialization and add_assign must both write to value"
    );
    assert!(instructions.windows(4).any(|window| {
        matches!(
            window[0].kind,
            mir::instr::InstKind::Const(mir::instr::ConstValue::Int(3, _))
        ) && matches!(window[1].kind, mir::instr::InstKind::Load(_))
            && matches!(
                window[2].kind,
                mir::instr::InstKind::BinOp(mir::instr::BinOp::Add, _, _)
            )
            && matches!(window[3].kind, mir::instr::InstKind::Store(_, _))
    }));
}

#[test]
fn primitive_operator_trait_without_lang_marker_keeps_ordinary_method() {
    let module = lower(
        r#"
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        impl Add for i32 {
            type Output = i32;
            fun add(self, rhs: Self) -> Self::Output { self + rhs }
        }

        fun main() -> i32 {
            let value: i32 = 1;
            value.add(2)
        }
        "#,
    );

    assert!(
        module
            .function_order
            .iter()
            .any(|fid| module.functions[*fid].name == "add__i32")
    );
    let main = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|function| function.name == "main")
        .unwrap();
    assert!(
        main.blocks
            .iter()
            .any(|(_, block)| block.insts.iter().any(|instruction| matches!(
                &instruction.kind,
                mir::instr::InstKind::Call(mir::FuncRef::Local(name), _)
                    if name == "add__i32"
            )))
    );
}

#[test]
fn malformed_lang_operator_signature_keeps_ordinary_method() {
    let module = lower(
        r#"
        #[lang = "add"]
        trait Add {
            fun add(self) -> i32;
        }

        impl Add for i32 {
            fun add(self) -> i32 { self }
        }

        fun main() -> i32 {
            let value: i32 = 1;
            value.add()
        }
        "#,
    );

    assert!(
        module
            .function_order
            .iter()
            .any(|fid| module.functions[*fid].name == "add__i32")
    );
}

#[test]
fn generic_lang_operator_method_uses_builtin_after_monomorphization() {
    let module = lower(
        r#"
        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Self) -> Self::Output;
        }

        impl Add for i32 {
            type Output = i32;
            fun add(self, rhs: Self) -> Self::Output { self + rhs }
        }

        fun sum<T: Add<Output = T>>(left: T, right: T) -> T {
            left.add(right)
        }

        fun main() -> i32 {
            sum(1, 2)
        }
        "#,
    );

    assert!(
        module
            .function_order
            .iter()
            .all(|fid| module.functions[*fid].name != "add__i32")
    );
    let sum = module
        .function_order
        .iter()
        .map(|fid| &module.functions[*fid])
        .find(|function| function.name == "sum__i32")
        .unwrap();
    assert!(
        sum.blocks
            .iter()
            .any(|(_, block)| block.insts.iter().any(|instruction| matches!(
                instruction.kind,
                mir::instr::InstKind::BinOp(mir::instr::BinOp::Add, _, _)
            )))
    );
    assert!(!sum.blocks.iter().any(|(_, block)| {
        block
            .insts
            .iter()
            .any(|instruction| matches!(instruction.kind, mir::instr::InstKind::Call(_, _)))
    }));
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
fn all_reference_forms_promote_their_source_local() {
    let (_, type_result, _, module) = compile(
        r#"
        struct Data { value: i32 }
        struct Receiver { value: i32 }
        struct Other { value: i64 }
        struct Slot { value: &Data }
        struct Holder { value: &Data }

        fun direct_mut() -> &mut Data {
            let mut local = Data { value: 1 };
            &mut local
        }

        fun field_ref() -> &i32 {
            let local = Data { value: 1 };
            &local.value
        }

        fun param_field_ref(value: &Data) -> &i32 { &value.value }

        fun alias_field_ref() -> &i32 {
            let local = Data { value: 1 };
            let alias = &local;
            &alias.value
        }

        fun mutable_alias_field_ref() -> &i32 {
            let local = Data { value: 1 };
            let mut alias = &local;
            &alias.value
        }

        fun index_ref() -> &Data {
            let items = [Data { value: 1 }, Data { value: 2 }];
            &items[0]
        }

        fun mutable_alias_index_ref() -> &Data {
            let items = [Data { value: 1 }, Data { value: 2 }];
            let mut alias = &items;
            &(*alias)[0]
        }

        fun mutable_alias_deref_ref() -> &Data {
            let local = Data { value: 1 };
            let mut alias = &local;
            &*alias
        }

        fun block_ref() -> &Data {
            let local = Data { value: 1 };
            { &local }
        }

        fun by_value_param_ref(value: Data) -> &Data { &value }

        fun identity<T>(value: T) -> T { value }

        fun generic_before_param_ref(value: Data) -> &Data {
            let ignored = identity(1);
            &value
        }

        fun branch_refs(flag: bool) -> &Data {
            let left = Data { value: 1 };
            let right = Data { value: 2 };
            if flag { &left } else { &right }
        }

        struct Pair {
            left: &Data,
            right: &Data,
        }

        fun aggregate_refs() -> Pair {
            let left = Data { value: 1 };
            let right = Data { value: 2 };
            Pair { left: &left, right: &right }
        }

        fun reassigned_ref(flag: bool) -> &Data {
            let left = Data { value: 1 };
            let right = Data { value: 2 };
            let mut reference = &left;
            if flag { reference = &right; }
            reference
        }

        impl Data {
            fun value_ref(&self) -> &i32 { &self.value }
        }

        impl Receiver {
            fun choose(receiver: &Receiver, other: &Other) -> &Other { other }
        }

        fun method_ref() -> &i32 {
            let local = Data { value: 1 };
            local.value_ref()
        }

        fun method_alias_ref() -> &i32 {
            let local = Data { value: 1 };
            let alias = &local;
            alias.value_ref()
        }

        fun method_arg_ref() -> &Other {
            let receiver = Receiver { value: 1 };
            let other = Other { value: 2 };
            receiver.choose(&other)
        }

        fun loop_backedge(flag: bool) -> &Data {
            let first = Data { value: 1 };
            let later = Data { value: 2 };
            let mut current = &first;
            let mut escaped = current;
            while flag {
                escaped = current;
                current = &later;
            }
            escaped
        }

        fun indirect_store(slot: &mut Slot) {
            let local = Data { value: 1 };
            slot.value = &local;
        }

        fun deref_store(slot: &mut &Data) {
            let local = Data { value: 1 };
            *slot = &local;
        }

        fun consume_holder(holder: Holder) -> i32 { holder.value.value }

        fun value_capture() -> fun() -> i32 {
            let local = Data { value: 1 };
            let holder = Holder { value: &local };
            fun() { consume_holder(holder) }
        }

        fun lambda_param_ref() -> fun(Data) -> &Data {
            fun(value: Data) -> &Data { &value }
        }

        fun read(value: &Data) -> i32 { value.value }

        fun nonescaping_call() -> i32 {
            let local = Data { value: 1 };
            read(&local)
        }

        extern "C" fun store(value: &Data);

        fun escaping_call() {
            let local = Data { value: 1 };
            unsafe { store({ &local }); }
        }

        fun loop_call_sink(flag: bool) {
            let first = Data { value: 1 };
            let later = Data { value: 2 };
            let mut reference = &first;
            while flag {
                unsafe { store(reference); }
                reference = &later;
            }
        }
        "#,
    );
    assert!(
        type_result.diagnostics.is_empty(),
        "type errors: {:?}",
        type_result.diagnostics
    );
    let find_function = |name: &str| {
        module
            .function_order
            .iter()
            .map(|id| &module.functions[*id])
            .find(|function| function.name == name)
            .unwrap()
    };

    for (name, expected_allocations) in [
        ("direct_mut", 1),
        ("field_ref", 1),
        ("param_field_ref", 0),
        ("alias_field_ref", 1),
        ("mutable_alias_field_ref", 1),
        ("index_ref", 1),
        ("mutable_alias_index_ref", 1),
        ("mutable_alias_deref_ref", 1),
        ("block_ref", 1),
        ("by_value_param_ref", 1),
        ("generic_before_param_ref", 1),
        ("branch_refs", 2),
        ("aggregate_refs", 2),
        ("reassigned_ref", 2),
        ("method_ref", 1),
        ("loop_backedge", 2),
        ("indirect_store", 1),
        ("deref_store", 1),
        ("nonescaping_call", 0),
        ("escaping_call", 1),
        ("loop_call_sink", 2),
        ("method_alias_ref", 1),
    ] {
        let function = find_function(name);
        let allocations = function
            .blocks
            .iter()
            .flat_map(|(_, block)| &block.insts)
            .filter(|inst| matches!(inst.kind, mir::instr::InstKind::HeapAlloc(_)))
            .count();
        assert_eq!(
            allocations, expected_allocations,
            "{name} should heap-allocate every escaping local"
        );
    }

    let method = find_function("method_arg_ref");
    let allocated_structs: Vec<_> = method
        .blocks
        .iter()
        .flat_map(|(_, block)| &block.insts)
        .filter_map(|inst| match &inst.kind {
            mir::instr::InstKind::HeapAlloc(mir::types::Type::Ptr(ty)) => match ty.as_ref() {
                mir::types::Type::Struct(ty) => Some(ty.name.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(allocated_structs, ["Other"]);

    let method_ref = find_function("method_ref");
    let find_inst = |value: mir::Value| {
        method_ref.blocks.iter().find_map(|(_, block)| {
            let index = value.0.checked_sub(block.start_value)? as usize;
            block.insts.get(index)
        })
    };
    let receiver = method_ref
        .blocks
        .iter()
        .flat_map(|(_, block)| &block.insts)
        .find_map(|inst| match &inst.kind {
            mir::instr::InstKind::Call(_, args) => args.first().copied(),
            _ => None,
        })
        .unwrap();
    let receiver_place = match &find_inst(receiver).unwrap().kind {
        mir::instr::InstKind::UnOp(mir::instr::UnOp::Ref, place) => *place,
        inst => panic!("method receiver should be borrowed from a place, got {inst:?}"),
    };
    assert!(matches!(
        find_inst(receiver_place).unwrap().kind,
        mir::instr::InstKind::HeapAlloc(_)
    ));

    let index_ref = find_function("index_ref");
    let index_base = index_ref
        .blocks
        .iter()
        .flat_map(|(_, block)| &block.insts)
        .find_map(|inst| match inst.kind {
            mir::instr::InstKind::IndexPtr(base, _) => Some(base),
            _ => None,
        })
        .unwrap();
    let index_base_inst = index_ref
        .blocks
        .iter()
        .find_map(|(_, block)| {
            let index = index_base.0.checked_sub(block.start_value)? as usize;
            block.insts.get(index)
        })
        .unwrap();
    assert!(matches!(
        index_base_inst.kind,
        mir::instr::InstKind::HeapAlloc(_)
    ));

    let closure = find_function("value_capture");
    assert!(
        closure
            .blocks
            .iter()
            .flat_map(|(_, block)| &block.insts)
            .any(|inst| matches!(
                &inst.kind,
                mir::instr::InstKind::HeapAlloc(mir::types::Type::Ptr(ty))
                    if matches!(ty.as_ref(), mir::types::Type::Struct(ty) if ty.name == "Data")
            )),
        "a value-captured aggregate must keep its referenced local alive"
    );

    let lambda = module
        .function_order
        .iter()
        .map(|id| &module.functions[*id])
        .find(|function| {
            function.name.starts_with("__riddle_lambda_")
                && matches!(
                    &function.ret_type,
                    mir::types::Type::Ref(ty, _)
                        if matches!(ty.as_ref(), mir::types::Type::Struct(ty) if ty.name == "Data")
                )
        })
        .unwrap();
    assert!(
        lambda
            .blocks
            .iter()
            .flat_map(|(_, block)| &block.insts)
            .any(|inst| matches!(inst.kind, mir::instr::InstKind::HeapAlloc(_)))
    );

    let generic_caller = find_function("generic_before_param_ref");
    let borrowed = generic_caller
        .blocks
        .iter()
        .flat_map(|(_, block)| &block.insts)
        .find_map(|inst| match inst.kind {
            mir::instr::InstKind::UnOp(mir::instr::UnOp::Ref, place) => Some(place),
            _ => None,
        })
        .unwrap();
    assert!(generic_caller.blocks.iter().any(|(_, block)| {
        let index = borrowed
            .0
            .checked_sub(block.start_value)
            .map(|index| index as usize);
        index
            .and_then(|index| block.insts.get(index))
            .is_some_and(|inst| matches!(inst.kind, mir::instr::InstKind::HeapAlloc(_)))
    }));
}

#[test]
fn overloaded_operator_uses_parameter_escape_summary() {
    let (_, type_result, _, module) = compile(
        r#"
        struct LeftData { value: i32 }
        struct RightData { value: i32 }
        struct Left { value: &LeftData }
        struct Right { value: &RightData }

        #[lang = "add"]
        trait Add {
            type Output;
            fun add(self, rhs: Right) -> Self::Output;
        }

        impl Add for Left {
            type Output = &RightData;

            fun add(self, rhs: Right) -> Self::Output {
                rhs.value
            }
        }

        fun overloaded_add_ref() -> &RightData {
            let left_data = LeftData { value: 1 };
            let right_data = RightData { value: 2 };
            let left = Left { value: &left_data };
            let right = Right { value: &right_data };
            left + right
        }
        "#,
    );
    assert!(
        type_result.diagnostics.is_empty(),
        "type errors: {:?}",
        type_result.diagnostics
    );
    assert_eq!(type_result.operator_calls.len(), 1);

    let function = module
        .function_order
        .iter()
        .map(|id| &module.functions[*id])
        .find(|function| function.name == "overloaded_add_ref")
        .unwrap();
    let allocated_structs = function
        .blocks
        .iter()
        .flat_map(|(_, block)| &block.insts)
        .filter_map(|inst| match &inst.kind {
            mir::instr::InstKind::HeapAlloc(mir::types::Type::Ptr(ty)) => match ty.as_ref() {
                mir::types::Type::Struct(ty) => Some(ty.name.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(allocated_structs, ["RightData"]);
}

#[test]
fn pattern_bindings_preserve_reference_sources() {
    let (_, type_result, _, module) = compile(
        r#"
        struct Data { value: i32 }
        struct Holder { value: &Data }

        fun match_pattern_ref() -> &Data {
            let local = Data { value: 1 };
            match &local {
                value => value
            }
        }

        fun for_pattern_ref(fallback: &Data) -> &Data {
            let local = Data { value: 2 };
            for item in [&local] {
                return item;
            }
            fallback
        }

        fun shorthand_pattern_ref() -> &Data {
            let local = Data { value: 3 };
            let holder = Holder { value: &local };
            match holder {
                Holder { value } => value
            }
        }
        "#,
    );
    assert!(
        type_result.diagnostics.is_empty(),
        "type errors: {:?}",
        type_result.diagnostics
    );

    for name in [
        "match_pattern_ref",
        "for_pattern_ref",
        "shorthand_pattern_ref",
    ] {
        let function = module
            .function_order
            .iter()
            .map(|id| &module.functions[*id])
            .find(|function| function.name == name)
            .unwrap();
        assert_eq!(
            function
                .blocks
                .iter()
                .flat_map(|(_, block)| &block.insts)
                .filter(|inst| matches!(inst.kind, mir::instr::InstKind::HeapAlloc(_)))
                .count(),
            1,
            "{name} should heap-allocate the referenced local"
        );
    }
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

#[test]
fn anonymous_function_lowers_to_function_pointer_call() {
    let module = lower(
        r#"
        fun apply(f: fun(i32) -> i32, value: i32) -> i32 {
            f(value)
        }

        fun main() -> i32 {
            let inc = fun(x) { x + 1 };
            apply(inc, 41)
        }
        "#,
    );

    assert!(
        module
            .function_order
            .iter()
            .any(|id| module.functions[*id].name.starts_with("__riddle_lambda_"))
    );
    let apply = module
        .function_order
        .iter()
        .map(|id| &module.functions[*id])
        .find(|function| function.name == "apply")
        .unwrap();
    assert!(apply.blocks.iter().any(|(_, block)| {
        block
            .insts
            .iter()
            .any(|inst| matches!(inst.kind, mir::instr::InstKind::CallIndirect(..)))
    }));
}

#[test]
fn capturing_closure_lowers_environment_and_hidden_parameter() {
    let module = lower(
        r#"
        fun main() -> i32 {
            let base = 40;
            let add = fun(value: i32) { base + value };
            add(2)
        }
        "#,
    );

    let lambda = module
        .function_order
        .iter()
        .map(|id| &module.functions[*id])
        .find(|function| function.name.starts_with("__riddle_lambda_"))
        .unwrap();
    assert!(matches!(lambda.params[0].ty, mir::types::Type::Ptr(_)));
    let main = module
        .function_order
        .iter()
        .map(|id| &module.functions[*id])
        .find(|function| function.name == "main")
        .unwrap();
    assert!(main.blocks.iter().any(|(_, block)| {
        block
            .insts
            .iter()
            .any(|inst| matches!(inst.kind, mir::instr::InstKind::HeapAlloc(_)))
    }));
}
