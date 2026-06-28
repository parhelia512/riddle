use mir::builder::Builder;
use mir::instr::*;
use mir::types::*;
use mir::value::Value;

/// builder 是 `mir::builder::Builder` 的别名，display 测试验证格式化输出。
/// 这里使用 builder 构造 IR，然后通过 Display 检查输出格式。

#[test]
fn display_const_int() {
    let mut func = mir::func::Function::new("test".into(), Type::Int(IntTy::I32));
    let mut builder = Builder::new(&mut func);
    builder.iconst(42, IntTy::I32);
    builder.set_return(None);

    let module = make_module(func);
    let output = format!("{}", module);
    assert!(
        output.contains("iconst"),
        "output should contain 'iconst': {}",
        output
    );
    assert!(
        output.contains("42"),
        "output should contain '42': {}",
        output
    );
    assert!(
        output.contains("return"),
        "output should contain 'return': {}",
        output
    );
}

#[test]
fn display_binop() {
    let mut func = mir::func::Function::new("add".into(), Type::Int(IntTy::I32));
    let mut builder = Builder::new(&mut func);
    let a = builder.iconst(1, IntTy::I32);
    let b = builder.iconst(2, IntTy::I32);
    builder.binop(BinOp::Add, a, b, Type::Int(IntTy::I32));
    builder.set_return(None);

    let module = make_module(func);
    let output = format!("{}", module);
    assert!(
        output.contains("Add"),
        "output should contain 'Add': {}",
        output
    );
}

#[test]
fn display_function_name() {
    let mut func = mir::func::Function::new("my_func".into(), Type::Unit);
    let mut builder = Builder::new(&mut func);
    builder.set_return(None);

    let module = make_module(func);
    let output = format!("{}", module);
    assert!(
        output.contains("my_func"),
        "output should contain function name: {}",
        output
    );
}

#[test]
fn display_block_labels() {
    let mut func = mir::func::Function::new("test".into(), Type::Unit);

    // 先创建块
    let then_block = func.new_block_labeled("then");
    let else_block = func.new_block_labeled("else");

    let mut builder = Builder::new(&mut func);
    builder.set_cond_branch(Value(0), then_block, else_block);

    let module = make_module(func);
    let output = format!("{}", module);
    assert!(
        output.contains("block_then"),
        "output should contain 'block_then': {}",
        output
    );
    assert!(
        output.contains("block_else"),
        "output should contain 'block_else': {}",
        output
    );
}

#[test]
fn display_heap_alloc() {
    let mut func = mir::func::Function::new("test".into(), Type::Unit);
    let mut builder = Builder::new(&mut func);
    builder.heap_alloc(Type::Int(IntTy::I32));
    builder.set_return(None);

    let module = make_module(func);
    let output = format!("{}", module);
    assert!(
        output.contains("heap_alloc"),
        "output should contain 'heap_alloc': {}",
        output
    );
}

#[test]
fn display_phi_node() {
    let mut func = mir::func::Function::new("test".into(), Type::Int(IntTy::I32));
    let then_block = func.new_block_labeled("then");
    let else_block = func.new_block_labeled("else");
    let merge_block = func.new_block_labeled("merge");

    // 在 merge 块中手动添加 phi
    let phi = Inst::new(
        InstKind::Phi(vec![(Value(1), then_block), (Value(2), else_block)]),
        Type::Int(IntTy::I32),
    );
    func.push_inst(merge_block, phi);
    func.set_terminator(merge_block, Terminator::Return(Some(Value(3))));

    // 确保 entry 有终止指令
    func.set_terminator(func.entry, Terminator::Branch(merge_block));

    let module = make_module(func);
    let output = format!("{}", module);
    assert!(
        output.contains("phi"),
        "output should contain 'phi': {}",
        output
    );
}

fn make_module(func: mir::func::Function) -> mir::Module {
    let mut module = mir::Module::new("test");
    module.add_function(func);
    module
}
