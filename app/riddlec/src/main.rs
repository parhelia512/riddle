use ast::{self, support::AstNode};
use frontend::incremental::IncrementalParser;
use hir::{
    HirFile,
    body::{BinaryOp, Body, Expr, ExprId, UnaryOp},
    item_tree::HirTypeRef,
    lower_root,
};
use scope_graph::{builder::build_scope_graph, resolve::resolve_hir};
use type_checker::{TypeCheckResult, check_hir};

fn main() {
    let mut parser = IncrementalParser::new();
    let parse = parser.set_source(
        r#"
struct Point {
    x: i32,
    y: i64,
    label: str,
}

impl Point{
    fun new(x: i32, y: i64, label: str) -> Point {
        Point{x, y, label}
    }
}

fun add(left: i64, right: i64) -> i64 {
    left + right
}

fun weight(value: f32) -> f32 {
    value + 1.5f32
}

fun main(flag: bool) -> i64 {
    let p = Point{x: 1, y: 2, label: "origin"};
    let px: i64 = 1;
    let total = add(px, p.y);
    let w: f32 = weight(0.5);
    if flag { total } else { 0 };
    let q = Point::new(1, 2, "q");
    0
}

fun broken() -> bool {
    let value: bool = 1;
    return add(1, 2);
}
"#,
    );

    if !parse.errors.is_empty() {
        println!("parse errors:");
        for error in &parse.errors {
            println!("  - {error}");
        }
        return;
    }

    let syntax = parse.syntax();
    let root = ast::Root::cast(syntax.clone()).unwrap();
    let mut hir = lower_root(root);
    let sg = build_scope_graph(&hir, &syntax);
    resolve_hir(&mut hir, &sg);

    let result = check_hir(&hir);
    print_expr_types(&hir, &result);
    print_type_diagnostics(&result);
}

fn print_expr_types(hir: &HirFile, result: &TypeCheckResult) {
    println!("== expression types ==");
    for (fid, function) in hir.item_tree.functions.iter() {
        let Some(body_id) = hir.function_bodies.get(&fid).copied() else {
            continue;
        };

        let params = function
            .params
            .iter()
            .map(|param| format!("{}: {}", param.name.0, type_ref_text(&param.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = function
            .ret_type
            .as_ref()
            .map(type_ref_text)
            .unwrap_or_else(|| "()".to_string());
        println!("fun {}({params}) -> {ret}", function.name.0);

        let body = &hir.bodies[body_id];
        for (expr_id, _) in body.exprs.iter() {
            let expr = expr_text(body, expr_id);
            let ty = result
                .expr_types
                .get(&(body_id, expr_id))
                .map(|ty| ty.display(hir))
                .unwrap_or_else(|| "<not checked>".to_string());
            println!("  {expr_id:?}: {expr} : {ty}");
        }
        println!();
    }
}

fn print_type_diagnostics(result: &TypeCheckResult) {
    println!("== type diagnostics ==");
    if result.diagnostics.is_empty() {
        println!("  <none>");
        return;
    }

    for diagnostic in &result.diagnostics {
        println!("  - {}", diagnostic.message);
    }
}

fn expr_text(body: &Body, expr: ExprId) -> String {
    match &body.exprs[expr] {
        Expr::Missing => "<missing>".to_string(),
        Expr::IntLiteral { value, suffix } => {
            format!("{}{}", value, suffix.as_deref().unwrap_or(""))
        }
        Expr::FloatLiteral { value, suffix } => {
            format!("{}{}", value, suffix.as_deref().unwrap_or(""))
        }
        Expr::StringLiteral { value } => value.clone(),
        Expr::CharLiteral { value } => value.clone(),
        Expr::BoolLiteral { value } => value.to_string(),
        Expr::Path { path, .. } => path.display(),
        Expr::Binary { lhs, rhs, op } => {
            format!(
                "({} {} {})",
                expr_text(body, *lhs),
                binary_op_text(*op),
                expr_text(body, *rhs)
            )
        }
        Expr::Unary { operand, op } => {
            format!("({}{})", unary_op_text(*op), expr_text(body, *operand))
        }
        Expr::Block { stmts, tail } => {
            let tail = tail
                .map(|tail| format!("; {}", expr_text(body, tail)))
                .unwrap_or_default();
            format!("{{ {} stmt(s){tail} }}", stmts.len())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let else_text = else_branch
                .map(|expr| format!(" else {}", expr_text(body, expr)))
                .unwrap_or_default();
            format!(
                "if {} {}{}",
                expr_text(body, *cond),
                expr_text(body, *then_branch),
                else_text
            )
        }
        Expr::While { condition, body: b } => {
            format!(
                "while {} {}",
                expr_text(body, *condition),
                expr_text(body, *b)
            )
        }
        Expr::Match { scrutinee, arms } => {
            format!(
                "match {} {{ {} arm(s) }}",
                expr_text(body, *scrutinee),
                arms.len()
            )
        }
        Expr::Array { elements } => {
            let elements = elements
                .iter()
                .map(|element| expr_text(body, *element))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{elements}]")
        }
        Expr::Struct { path, fields, .. } => {
            let fields = fields
                .iter()
                .map(|field| format!("{}: {}", field.name.0, expr_text(body, field.value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}{{{fields}}}", path.display())
        }
        Expr::Call { callee, args } => {
            let args = args
                .iter()
                .map(|arg| expr_text(body, *arg))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({args})", expr_text(body, *callee))
        }
        Expr::FieldAccess { base, field } => {
            format!("{}.{}", expr_text(body, *base), field.0)
        }
    }
}

fn type_ref_text(ty: &HirTypeRef) -> String {
    match ty {
        HirTypeRef::Named(path) => path.display(),
        HirTypeRef::Ref(inner) => format!("&{}", type_ref_text(inner)),
        HirTypeRef::Tuple(elements) => {
            let elements = elements
                .iter()
                .map(type_ref_text)
                .collect::<Vec<_>>()
                .join(", ");
            format!("({elements})")
        }
        HirTypeRef::Array(inner) => format!("[{}]", type_ref_text(inner)),
        HirTypeRef::Unknown => "_".to_string(),
        HirTypeRef::Error => "<error>".to_string(),
    }
}

fn binary_op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Assign => "=",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::Eq => "==",
        BinaryOp::Neq => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Gt => ">",
        BinaryOp::LtEq => "<=",
        BinaryOp::GtEq => ">=",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
    }
}

fn unary_op_text(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Pos => "+",
        UnaryOp::Ref => "&",
        UnaryOp::Deref => "*",
        UnaryOp::Not => "!",
    }
}
