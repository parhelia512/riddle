use la_arena::{Arena, Idx};
use rowan::ast::SyntaxNodePtr;

use frontend::syntax_kind::RiddleLang;

use super::{
    Name,
    item_tree::{self, FunctionId, HirPath, HirTypeRef, ModuleId, StructId, UseId},
};

pub type ExprId = Idx<Expr>;
pub type StmtId = Idx<Stmt>;
pub type PatId = Idx<Pattern>;
pub type BodyId = Idx<Body>;

#[derive(Debug)]
pub struct Body {
    pub exprs: Arena<Expr>,
    pub stmts: Arena<Stmt>,
    pub pats: Arena<Pattern>,
    pub root_block: ExprId,
    /// Syntax pointer to the body's root block, used as the key for incremental
    /// invalidation of this body's scope-graph fragment.
    pub root_ptr: SyntaxNodePtr<RiddleLang>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: Name,
        ty: HirTypeRef,
        init: Option<ExprId>,
    },
    Expr {
        expr: ExprId,
    },
    Return {
        value: Option<ExprId>,
    },
    /// `mod inner { ... }` or `use foo::bar;` inside a function body.
    /// All such items are promoted to the global ItemTree, so we only
    /// keep an id-level reference here.
    Item {
        item: BodyItem,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum BodyItem {
    Module(ModuleId),
    Use(UseId),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Missing,
    IntLiteral {
        value: i64,
<<<<<<< HEAD
        suffix: Option<String>,
    },
    FloatLiteral {
        value: f64,
        suffix: Option<String>,
=======
    },
    FloatLiteral {
        value: f64,
>>>>>>> 0d7abe0350871a575608ce4fc1d8aae9223abb1c
    },
    StringLiteral {
        value: String,
    },
    CharLiteral {
        value: String,
    },
    BoolLiteral {
        value: bool,
    },
    Path {
        path: HirPath,
        resolved: Option<ResolvedName>,
    },
    Binary {
        lhs: ExprId,
        rhs: ExprId,
        op: BinaryOp,
    },
    Unary {
        operand: ExprId,
        op: UnaryOp,
    },
    Block {
        stmts: Vec<StmtId>,
        tail: Option<ExprId>,
    },
    If {
        cond: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
    },
    While {
        condition: ExprId,
        body: ExprId,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    Array {
        elements: Vec<ExprId>,
    },
    Struct {
        path: HirPath,
        fields: Vec<StructExprField>,
        resolved: Option<ResolvedName>,
    },
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
    },
    FieldAccess {
        base: ExprId,
        field: Name,
    },
}

#[derive(Debug, Clone)]
pub struct StructExprField {
    pub name: Name,
    pub value: ExprId,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pat: PatId,
    pub guard: Option<ExprId>,
    pub body: ExprId,
}

/// Lowered pattern. Bindings introduced by patterns become locals in the arm body.
#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    /// A literal pattern such as `1` or `"x"`.
    Literal,
    /// A bare identifier that binds a new name, e.g. `x` in `match v { x => ... }`.
    Binding {
        name: Name,
    },
    /// A path pattern referring to an existing item (enum variant / const), e.g. `Foo::Bar`.
    Path {
        path: HirPath,
    },
    Tuple {
        elements: Vec<PatId>,
    },
    /// `Variant(a, b)` tuple-style enum pattern.
    TupleStruct {
        path: HirPath,
        elements: Vec<PatId>,
    },
    /// `Variant { a, b: c }` struct-style enum/struct pattern.
    Struct {
        path: HirPath,
        fields: Vec<FieldPat>,
    },
}

#[derive(Debug, Clone)]
pub struct FieldPat {
    pub name: Name,
    pub pat: Option<PatId>,
}

#[derive(Debug, Clone)]
pub enum ResolvedName {
    Local(StmtId),
    Param(usize),
    Function(FunctionId),
    Struct(StructId),
    Enum(item_tree::EnumId),
    Trait(item_tree::TraitId),
    Const(item_tree::ConstId),
    TypeAlias(item_tree::TypeAliasId),
    Module(ModuleId),
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Pos,
    Ref,
    Deref,
    Not,
}

impl Body {
    pub fn pretty<'a>(&'a self, hir: &'a super::HirFile) -> PrettyBody<'a> {
        PrettyBody { body: self, hir }
    }
}

pub struct PrettyBody<'a> {
    body: &'a Body,
    hir: &'a super::HirFile,
}

impl std::fmt::Display for PrettyBody<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let printer = BodyPrinter {
            body: self.body,
            hir: self.hir,
        };
        write!(f, "{}", printer.print_body())
    }
}

struct BodyPrinter<'a> {
    body: &'a Body,
    hir: &'a super::HirFile,
}

impl BodyPrinter<'_> {
    fn print_body(&self) -> String {
        let mut out = self.print_expr(self.body.root_block, 0, 0);
        if !self.body.diagnostics.is_empty() {
            out.push_str("\n\n// diagnostics\n");
            for d in &self.body.diagnostics {
                out.push_str("// - ");
                out.push_str(&d.message);
                out.push('\n');
            }
        }
        out
    }

    fn print_stmt(&self, stmt: StmtId, indent: usize) -> String {
        match &self.body.stmts[stmt] {
            Stmt::Let { name, ty, init } => {
                let mut out = format!("let {}", name.0);
                if !matches!(ty, HirTypeRef::Unknown) {
                    out.push_str(": ");
                    out.push_str(&Self::type_text(ty));
                }
                if let Some(init) = init {
                    out.push_str(" = ");
                    out.push_str(&self.print_expr(*init, 0, indent));
                }
                out.push(';');
                out
            }
            Stmt::Return { value } => {
                let mut out = String::from("return");
                if let Some(v) = value {
                    out.push(' ');
                    out.push_str(&self.print_expr(*v, 0, indent));
                }
                out.push(';');
                out
            }
            Stmt::Expr { expr } => {
                let mut out = self.print_expr(*expr, 0, indent);
                out.push(';');
                out
            }
            Stmt::Item { item } => match item {
                BodyItem::Module(mid) => {
                    let m = &self.hir.item_tree.modules[*mid];
                    match &m.items {
                        None => format!("mod {};", m.name.0),
                        Some(_) => format!("mod {} {{ /* ... */ }}", m.name.0),
                    }
                }
                BodyItem::Use(uid) => {
                    let u = &self.hir.item_tree.uses[*uid];
                    format!("use {};", Self::use_tree_text(&u.tree))
                }
            },
        }
    }

    fn use_tree_text(t: &super::item_tree::HirUseTree) -> String {
        use super::item_tree::HirUseTreeKind::*;
        let prefix = t.prefix.display();
        match &t.kind {
            Simple { alias: None } => prefix,
            Simple { alias: Some(a) } => format!("{} as {}", prefix, a.0),
            Glob => {
                if prefix.is_empty() {
                    "*".into()
                } else {
                    format!("{}::*", prefix)
                }
            }
            List(children) => {
                let inner = children
                    .iter()
                    .map(Self::use_tree_text)
                    .collect::<Vec<_>>()
                    .join(", ");
                if prefix.is_empty() {
                    format!("{{{}}}", inner)
                } else {
                    format!("{}::{{{}}}", prefix, inner)
                }
            }
        }
    }

    fn print_expr(&self, expr: ExprId, parent_prec: u8, indent: usize) -> String {
        let current_prec = self.expr_prec(expr);
        let out = match &self.body.exprs[expr] {
            Expr::Missing => "<missing>".to_string(),
<<<<<<< HEAD
            Expr::IntLiteral { value, suffix } => {
                format!("{}{}", value, suffix.as_deref().unwrap_or(""))
            }
            Expr::FloatLiteral { value, suffix } => {
                format!("{}{}", value, suffix.as_deref().unwrap_or(""))
            }
=======
            Expr::IntLiteral { value } => value.to_string(),
            Expr::FloatLiteral { value } => value.to_string(),
>>>>>>> 0d7abe0350871a575608ce4fc1d8aae9223abb1c
            Expr::StringLiteral { value } => format!("\"{}\"", value),
            Expr::CharLiteral { value } => format!("'{}'", value),
            Expr::BoolLiteral { value } => value.to_string(),
            Expr::Path { path, resolved } => match resolved {
                Some(ResolvedName::Unresolved) => format!("{}/*?*/", path.display()),
                Some(_) => path.display(),
                None => path.display(),
            },
            Expr::Unary { operand, op } => {
                let operand = self.print_expr(*operand, current_prec, indent);
                format!("({}{})", Self::unary_op_text(op), operand)
            }
            Expr::Binary { lhs, rhs, op } => {
                let lhs = self.print_expr(*lhs, current_prec, indent);
                let rhs = self.print_expr(*rhs, current_prec + 1, indent);
                format!("({} {} {})", lhs, Self::binary_op_text(op), rhs)
            }
            Expr::Call { callee, args } => {
                let callee = self.print_expr(*callee, current_prec, indent);
                let args = args
                    .iter()
                    .map(|a| self.print_expr(*a, 0, indent))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}({})", callee, args)
            }
            Expr::FieldAccess { base, field } => {
                let base = self.print_expr(*base, current_prec, indent);
                format!("({}.{})", base, field.0)
            }
            Expr::Block { stmts, tail } => self.print_block(stmts, *tail, indent),
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let mut out = String::from("if ");
                out.push_str(&self.print_expr(*cond, 0, indent));
                out.push(' ');
                out.push_str(&self.print_block_like(*then_branch, indent));
                if let Some(else_branch) = else_branch {
                    out.push_str(" else ");
                    match &self.body.exprs[*else_branch] {
                        Expr::If { .. } => out.push_str(&self.print_expr(*else_branch, 0, indent)),
                        _ => out.push_str(&self.print_block_like(*else_branch, indent)),
                    }
                }
                out
            }
            Expr::While { condition, body } => {
                let mut out = String::from("while ");
                out.push_str(&self.print_expr(*condition, 0, indent));
                out.push(' ');
                out.push_str(&self.print_block_like(*body, indent));
                out
            }
            Expr::Match { scrutinee, arms } => {
                let mut out = String::from("match ");
                out.push_str(&self.print_expr(*scrutinee, 0, indent));
                out.push_str(" {\n");
                for arm in arms {
                    out.push_str(&Self::indent(indent + 1));
                    out.push_str(&self.print_pat(arm.pat));
                    if let Some(g) = arm.guard {
                        out.push_str(" if ");
                        out.push_str(&self.print_expr(g, 0, indent + 1));
                    }
                    out.push_str(" => ");
                    out.push_str(&self.print_expr(arm.body, 0, indent + 1));
                    out.push_str(",\n");
                }
                out.push_str(&Self::indent(indent));
                out.push('}');
                out
            }
            Expr::Array { elements } => {
                let items = elements
                    .iter()
                    .map(|e| self.print_expr(*e, 0, indent))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{}]", items)
            }
            Expr::Struct { path, fields, .. } => {
                let fields = fields
                    .iter()
                    .map(|field| {
                        format!(
                            "{}: {}",
                            field.name.0,
                            self.print_expr(field.value, 0, indent)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{{}}}", path.display(), fields)
            }
        };
        if current_prec < parent_prec {
            format!("({})", out)
        } else {
            out
        }
    }

    fn print_block_like(&self, expr: ExprId, indent: usize) -> String {
        match &self.body.exprs[expr] {
            Expr::Block { stmts, tail } => self.print_block(stmts, *tail, indent),
            _ => self.print_expr(expr, 0, indent),
        }
    }

    fn print_block(&self, stmts: &[StmtId], tail: Option<ExprId>, indent: usize) -> String {
        let mut out = String::from("{\n");
        for s in stmts {
            out.push_str(&Self::indent(indent + 1));
            out.push_str(&self.print_stmt(*s, indent + 1));
            out.push('\n');
        }
        if let Some(tail) = tail {
            out.push_str(&Self::indent(indent + 1));
            out.push_str(&self.print_expr(tail, 0, indent + 1));
            out.push('\n');
        }
        out.push_str(&Self::indent(indent));
        out.push('}');
        out
    }

    fn expr_prec(&self, expr: ExprId) -> u8 {
        match &self.body.exprs[expr] {
            Expr::Missing
            | Expr::IntLiteral { .. }
            | Expr::FloatLiteral { .. }
            | Expr::StringLiteral { .. }
            | Expr::CharLiteral { .. }
            | Expr::BoolLiteral { .. }
            | Expr::Path { .. }
            | Expr::Struct { .. }
            | Expr::Array { .. } => 100,
            Expr::Call { .. } | Expr::FieldAccess { .. } => 90,
            Expr::Unary { .. } => 80,
            Expr::Binary { op, .. } => Self::binary_prec(op),
            Expr::Block { .. } | Expr::If { .. } | Expr::While { .. } | Expr::Match { .. } => 0,
        }
    }

    fn binary_prec(op: &BinaryOp) -> u8 {
        match op {
            BinaryOp::Assign => 5,
            BinaryOp::Or => 10,
            BinaryOp::And => 20,
            BinaryOp::Eq | BinaryOp::Neq => 30,
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => 40,
            BinaryOp::Add | BinaryOp::Sub => 50,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 60,
        }
    }

    fn binary_op_text(op: &BinaryOp) -> &'static str {
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

    fn unary_op_text(op: &UnaryOp) -> &'static str {
        match op {
            UnaryOp::Pos => "+",
            UnaryOp::Neg => "-",
            UnaryOp::Ref => "&",
            UnaryOp::Deref => "*",
            UnaryOp::Not => "!",
        }
    }

    fn type_text(ty: &HirTypeRef) -> String {
        match ty {
            HirTypeRef::Unknown => "_".to_string(),
            HirTypeRef::Error => "<error>".to_string(),
            HirTypeRef::Named(p) => p.display(),
            HirTypeRef::Ref(inner) => format!("&{}", Self::type_text(inner)),
            HirTypeRef::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(|t| Self::type_text(t))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", inner)
            }
            HirTypeRef::Array(elem) => format!("[{}]", Self::type_text(elem)),
        }
    }

    fn print_pat(&self, pat: PatId) -> String {
        match &self.body.pats[pat] {
            Pattern::Wildcard => "_".to_string(),
            Pattern::Literal => "<lit>".to_string(),
            Pattern::Binding { name } => name.0.clone(),
            Pattern::Path { path } => path.display(),
            Pattern::Tuple { elements } => {
                let inner = elements
                    .iter()
                    .map(|p| self.print_pat(*p))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({})", inner)
            }
            Pattern::TupleStruct { path, elements } => {
                let inner = elements
                    .iter()
                    .map(|p| self.print_pat(*p))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}({})", path.display(), inner)
            }
            Pattern::Struct { path, fields } => {
                let inner = fields
                    .iter()
                    .map(|fp| match &fp.pat {
                        Some(p) => format!("{}: {}", fp.name.0, self.print_pat(*p)),
                        None => fp.name.0.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{ {} }}", path.display(), inner)
            }
        }
    }

    fn indent(level: usize) -> String {
        "    ".repeat(level)
    }
}
