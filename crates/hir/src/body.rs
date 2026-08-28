use std::collections::HashMap;

use la_arena::{Arena, Idx};
use rowan::{TextRange, ast::SyntaxNodePtr};

use syntax::RiddleLang;

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
    /// Maps HIR ids to their source text ranges, populated during lowering.
    pub source_map: SourceMap,
}

#[derive(Debug, Default)]
pub struct SourceMap {
    pub expr_ranges: HashMap<ExprId, TextRange>,
    pub stmt_ranges: HashMap<StmtId, TextRange>,
    pub pat_ranges: HashMap<PatId, TextRange>,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub labels: Vec<SourceLabel>,
    pub help: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SourceLabel {
    pub range: TextRange,
    pub message: String,
    pub style: LabelStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    // PartialEq + Eq added for cross-crate diagnostic bridging
    Error,
    Warning,
    Note,
    Help,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        /// Always present: `let x = 1` lowers to a `Pattern::Binding`, so every
        /// binding in the body — simple or destructured — has one identity.
        pat: PatId,
        ty: HirTypeRef,
        ty_range: Option<TextRange>,
        init: Option<ExprId>,
        /// The diverging block of `let PAT = init else { .. };`. The bindings
        /// escape to the enclosing scope, so this stays on the statement
        /// instead of desugaring to a `match`.
        else_: Option<ExprId>,
    },
    Expr {
        expr: ExprId,
    },
    Return {
        value: Option<ExprId>,
    },
    Break {
        value: Option<ExprId>,
    },
    Continue,
    /// `mod inner { ... }` or `use foo::bar;` inside a function body.
    /// All such items are promoted to the global `ItemTree`, so we only
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
        value: u64,
        suffix: Option<String>,
    },
    FloatLiteral {
        value: f64,
        suffix: Option<String>,
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
    Loop {
        body: ExprId,
    },
    For {
        pat: PatId,
        iterable: ExprId,
        body: ExprId,
    },
    Match {
        scrutinee: ExprId,
        arms: Vec<MatchArm>,
    },
    Array {
        elements: Vec<ExprId>,
    },
    Tuple {
        elements: Vec<ExprId>,
    },
    ArrayRepeat {
        value: ExprId,
        len: ExprId,
    },
    Struct {
        path: HirPath,
        fields: Vec<StructExprField>,
        resolved: Option<ResolvedName>,
    },
    Call {
        callee: ExprId,
        args: Vec<ExprId>,
        type_args: Vec<HirTypeRef>,
    },
    Lambda {
        is_move: bool,
        generics: Vec<Name>,
        generic_bounds: Vec<crate::item_tree::HirGenericBound>,
        params: Vec<LambdaParam>,
        ret_type: HirTypeRef,
        ret_type_range: Option<TextRange>,
        body: ExprId,
    },
    FieldAccess {
        base: ExprId,
        field: Name,
    },
    IndexAccess {
        base: ExprId,
        index: ExprId,
    },
    Unsafe {
        body: ExprId,
    },
    Cast {
        base: ExprId,
        target: HirTypeRef,
    },
    Try {
        operand: ExprId,
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

#[derive(Debug, Clone)]
pub struct LambdaParam {
    pub name: Name,
    pub name_range: Option<TextRange>,
    pub is_mut: bool,
    pub pat: Option<PatId>,
    pub ty: HirTypeRef,
    pub ty_range: Option<TextRange>,
}

/// Lowered pattern. Bindings introduced by patterns become locals in the arm body.
#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard,
    /// A literal pattern such as `1` or `"x"`.
    Literal(LiteralPattern),
    /// A bare identifier that binds a new name, e.g. `x` in `match v { x => ... }`.
    Binding {
        name: Name,
        /// `mut` sits on the binding, as in Rust: `let (mut a, b) = pair`.
        is_mut: bool,
    },
    /// An explicit reference pattern, e.g. `&x` or `&mut x`.
    Reference {
        mutable: bool,
        pattern: PatId,
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
pub enum LiteralPattern {
    Int {
        value: u64,
        suffix: Option<String>,
        valid: bool,
    },
    Float {
        value: f64,
        suffix: Option<String>,
        valid: bool,
    },
    String(String),
    Char(String),
    Bool(bool),
}

#[derive(Debug, Clone)]
pub struct FieldPat {
    pub name: Name,
    pub pat: Option<PatId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PatternBindingId {
    pub pattern: PatId,
    pub field: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum ResolvedName {
    PatternBinding(PatternBindingId),
    Param(usize),
    LambdaParam { lambda: ExprId, index: usize },
    Function(FunctionId),
    Struct(StructId),
    Enum(item_tree::EnumId),
    EnumVariant(item_tree::EnumId, usize),
    Trait(item_tree::TraitId),
    Const(item_tree::ConstId),
    TypeAlias(item_tree::TypeAliasId),
    Module(ModuleId),
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    ShlAssign,
    ShrAssign,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Neq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

impl BinaryOp {
    #[must_use]
    pub const fn is_assignment(self) -> bool {
        matches!(
            self,
            Self::Assign
                | Self::AddAssign
                | Self::SubAssign
                | Self::MulAssign
                | Self::DivAssign
                | Self::ModAssign
                | Self::BitAndAssign
                | Self::BitOrAssign
                | Self::BitXorAssign
                | Self::ShlAssign
                | Self::ShrAssign
        )
    }

    #[must_use]
    pub const fn compound_base(self) -> Option<Self> {
        match self {
            Self::AddAssign => Some(Self::Add),
            Self::SubAssign => Some(Self::Sub),
            Self::MulAssign => Some(Self::Mul),
            Self::DivAssign => Some(Self::Div),
            Self::ModAssign => Some(Self::Mod),
            Self::BitAndAssign => Some(Self::BitAnd),
            Self::BitOrAssign => Some(Self::BitOr),
            Self::BitXorAssign => Some(Self::BitXor),
            Self::ShlAssign => Some(Self::Shl),
            Self::ShrAssign => Some(Self::Shr),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Pos,
    Ref,
    MutRef,
    Deref,
    Not,
}

impl Body {
    #[must_use]
    pub const fn pretty<'a>(&'a self, hir: &'a super::HirFile) -> PrettyBody<'a> {
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
            Stmt::Let {
                pat,
                ty,
                init,
                else_,
                ..
            } => {
                let mut out = format!("let {}", self.print_pat(*pat));
                if !matches!(ty, HirTypeRef::Unknown) {
                    out.push_str(": ");
                    out.push_str(&Self::type_text(ty));
                }
                if let Some(init) = init {
                    out.push_str(" = ");
                    out.push_str(&self.print_expr(*init, 0, indent));
                }
                if let Some(else_) = else_ {
                    out.push_str(" else ");
                    out.push_str(&self.print_expr(*else_, 0, indent));
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
            Stmt::Break { value } => {
                let mut out = String::from("break");
                if let Some(v) = value {
                    out.push(' ');
                    out.push_str(&self.print_expr(*v, 0, indent));
                }
                out.push(';');
                out
            }
            Stmt::Continue => String::from("continue;"),
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

    fn use_tree_text(t: &item_tree::HirUseTree) -> String {
        use super::item_tree::HirUseTreeKind::{Glob, List, Simple};
        let prefix = t.prefix.display();
        match &t.kind {
            Simple { alias: None } => prefix,
            Simple { alias: Some(a) } => format!("{} as {}", prefix, a.0),
            Glob => {
                if prefix.is_empty() {
                    "*".into()
                } else {
                    format!("{prefix}::*")
                }
            }
            List(children) => {
                let inner = children
                    .iter()
                    .map(Self::use_tree_text)
                    .collect::<Vec<_>>()
                    .join(", ");
                if prefix.is_empty() {
                    format!("{{{inner}}}")
                } else {
                    format!("{prefix}::{{{inner}}}")
                }
            }
        }
    }

    fn print_expr(&self, expr: ExprId, parent_prec: u8, indent: usize) -> String {
        let current_prec = self.expr_prec(expr);
        let out = match &self.body.exprs[expr] {
            Expr::Missing => "<missing>".to_string(),
            Expr::IntLiteral { value, suffix } => {
                format!("{}{}", value, suffix.as_deref().unwrap_or(""))
            }
            Expr::FloatLiteral { value, suffix } => {
                format!("{}{}", value, suffix.as_deref().unwrap_or(""))
            }
            Expr::StringLiteral { value } => format!("\"{value}\""),
            Expr::CharLiteral { value } => format!("'{value}'"),
            Expr::BoolLiteral { value } => value.to_string(),
            Expr::Path { path, resolved } => match resolved {
                Some(ResolvedName::Unresolved) => format!("{}/*?*/", path.display()),
                Some(_) | None => path.display(),
            },
            Expr::Unary { operand, op } => {
                let operand = self.print_expr(*operand, current_prec, indent);
                format!("({}{})", Self::unary_op_text(*op), operand)
            }
            Expr::Binary { lhs, rhs, op } => {
                let lhs = self.print_expr(*lhs, current_prec, indent);
                let rhs = self.print_expr(*rhs, current_prec + 1, indent);
                format!("({} {} {})", lhs, Self::binary_op_text(*op), rhs)
            }
            Expr::Call { callee, args, .. } => {
                let callee = self.print_expr(*callee, current_prec, indent);
                let args = args
                    .iter()
                    .map(|a| self.print_expr(*a, 0, indent))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{callee}({args})")
            }
            Expr::Lambda {
                params,
                ret_type,
                body,
                ..
            } => self.print_lambda(params, ret_type, *body, indent),
            Expr::FieldAccess { base, field } => {
                let base = self.print_expr(*base, current_prec, indent);
                format!("({}.{})", base, field.0)
            }
            Expr::IndexAccess { base, index } => {
                let base = self.print_expr(*base, current_prec, indent);
                let index = self.print_expr(*index, 0, indent);
                format!("({base}[{index}])")
            }
            Expr::Block { stmts, tail } => self.print_block(stmts, *tail, indent),
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => self.print_if(*cond, *then_branch, *else_branch, indent),
            Expr::While { condition, body } => self.print_while(*condition, *body, indent),
            Expr::Loop { body } => format!("loop {}", self.print_block_like(*body, indent)),
            Expr::For {
                pat,
                iterable,
                body,
            } => self.print_for(*pat, *iterable, *body, indent),
            Expr::Match { scrutinee, arms } => self.print_match(*scrutinee, arms, indent),
            Expr::Array { elements } => {
                let items = elements
                    .iter()
                    .map(|e| self.print_expr(*e, 0, indent))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{items}]")
            }
            Expr::Tuple { elements } => {
                let items = elements
                    .iter()
                    .map(|e| self.print_expr(*e, 0, indent))
                    .collect::<Vec<_>>()
                    .join(", ");
                let trailing = if elements.len() == 1 { "," } else { "" };
                format!("({items}{trailing})")
            }
            Expr::ArrayRepeat { value, len } => self.print_array_repeat(*value, *len, indent),
            Expr::Unsafe { body } => {
                format!("unsafe {}", self.print_block_like(*body, indent))
            }
            Expr::Cast { base, target } => {
                let base = self.print_expr(*base, current_prec, indent);
                format!("({} as {})", base, Self::type_text(target))
            }
            Expr::Try { operand } => {
                format!("{}?", self.print_expr(*operand, current_prec, indent))
            }
            Expr::Struct { path, fields, .. } => self.print_struct(path, fields, indent),
        };
        if current_prec < parent_prec {
            format!("({out})")
        } else {
            out
        }
    }

    fn print_lambda(
        &self,
        params: &[LambdaParam],
        ret_type: &HirTypeRef,
        body: ExprId,
        indent: usize,
    ) -> String {
        let params = params
            .iter()
            .map(|param| {
                if matches!(param.ty, HirTypeRef::Unknown) {
                    param.name.0.clone()
                } else {
                    format!("{}: {}", param.name.0, param.ty.display())
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let ret = if matches!(ret_type, HirTypeRef::Unknown) {
            String::new()
        } else {
            format!(" -> {}", ret_type.display())
        };
        format!("fun({params}){ret} {}", self.print_block_like(body, indent))
    }

    fn print_if(
        &self,
        cond: ExprId,
        then_branch: ExprId,
        else_branch: Option<ExprId>,
        indent: usize,
    ) -> String {
        let mut out = format!("if {} ", self.print_expr(cond, 0, indent));
        out.push_str(&self.print_block_like(then_branch, indent));
        if let Some(else_branch) = else_branch {
            out.push_str(" else ");
            match &self.body.exprs[else_branch] {
                Expr::If { .. } => out.push_str(&self.print_expr(else_branch, 0, indent)),
                _ => out.push_str(&self.print_block_like(else_branch, indent)),
            }
        }
        out
    }

    fn print_for(&self, pat: PatId, iterable: ExprId, body: ExprId, indent: usize) -> String {
        format!(
            "for {} in {} {}",
            self.print_pat(pat),
            self.print_expr(iterable, 0, indent),
            self.print_block_like(body, indent)
        )
    }

    fn print_while(&self, condition: ExprId, body: ExprId, indent: usize) -> String {
        format!(
            "while {} {}",
            self.print_expr(condition, 0, indent),
            self.print_block_like(body, indent)
        )
    }

    fn print_array_repeat(&self, value: ExprId, len: ExprId, indent: usize) -> String {
        format!(
            "[{}; {}]",
            self.print_expr(value, 0, indent),
            self.print_expr(len, 0, indent)
        )
    }

    fn print_match(&self, scrutinee: ExprId, arms: &[MatchArm], indent: usize) -> String {
        let mut out = format!("match {} {{\n", self.print_expr(scrutinee, 0, indent));
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

    fn print_struct(&self, path: &HirPath, fields: &[StructExprField], indent: usize) -> String {
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
        format!("{} {{{fields}}}", path.display())
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
            | Expr::Array { .. }
            | Expr::Tuple { .. }
            | Expr::ArrayRepeat { .. } => 100,
            Expr::Call { .. }
            | Expr::FieldAccess { .. }
            | Expr::IndexAccess { .. }
            | Expr::Try { .. } => 90,
            Expr::Lambda { .. } => 70,
            Expr::Cast { .. } => 85,
            Expr::Unary { .. } => 80,
            Expr::Binary { op, .. } => Self::binary_prec(*op),
            Expr::Block { .. }
            | Expr::If { .. }
            | Expr::While { .. }
            | Expr::Loop { .. }
            | Expr::For { .. }
            | Expr::Match { .. }
            | Expr::Unsafe { .. } => 0,
        }
    }

    const fn binary_prec(op: BinaryOp) -> u8 {
        match op {
            BinaryOp::Assign
            | BinaryOp::AddAssign
            | BinaryOp::SubAssign
            | BinaryOp::MulAssign
            | BinaryOp::DivAssign
            | BinaryOp::ModAssign
            | BinaryOp::BitAndAssign
            | BinaryOp::BitOrAssign
            | BinaryOp::BitXorAssign
            | BinaryOp::ShlAssign
            | BinaryOp::ShrAssign => 5,
            BinaryOp::Or => 10,
            BinaryOp::And => 20,
            BinaryOp::Eq | BinaryOp::Neq => 30,
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => 40,
            BinaryOp::BitOr
            | BinaryOp::BitXor
            | BinaryOp::BitAnd
            | BinaryOp::Shl
            | BinaryOp::Shr => 45,
            BinaryOp::Add | BinaryOp::Sub => 50,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 60,
        }
    }

    const fn binary_op_text(op: BinaryOp) -> &'static str {
        match op {
            BinaryOp::Assign => "=",
            BinaryOp::AddAssign => "+=",
            BinaryOp::SubAssign => "-=",
            BinaryOp::MulAssign => "*=",
            BinaryOp::DivAssign => "/=",
            BinaryOp::ModAssign => "%=",
            BinaryOp::BitAndAssign => "&=",
            BinaryOp::BitOrAssign => "|=",
            BinaryOp::BitXorAssign => "^=",
            BinaryOp::ShlAssign => "<<=",
            BinaryOp::ShrAssign => ">>=",
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
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

    const fn unary_op_text(op: UnaryOp) -> &'static str {
        match op {
            UnaryOp::Pos => "+",
            UnaryOp::Neg => "-",
            UnaryOp::Ref => "&",
            UnaryOp::MutRef => "&mut ",
            UnaryOp::Deref => "*",
            UnaryOp::Not => "!",
        }
    }

    fn type_text(ty: &HirTypeRef) -> String {
        match ty {
            HirTypeRef::Never => "!".to_string(),
            HirTypeRef::Unknown => "_".to_string(),
            HirTypeRef::Error => "<error>".to_string(),
            HirTypeRef::Named(p) => p.display(),
            HirTypeRef::Ref(inner, mutable) => {
                let kw = if *mutable { "&mut " } else { "&" };
                format!("{}{}", kw, Self::type_text(inner))
            }
            HirTypeRef::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", Self::type_text(inner))
            }
            HirTypeRef::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(Self::type_text)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            }
            HirTypeRef::Slice(elem) => format!("[{}]", Self::type_text(elem)),
            HirTypeRef::Array(elem, len) => {
                format!("[{}; {}]", Self::type_text(elem), len.display())
            }
            HirTypeRef::Const(value) => value.display(),
            HirTypeRef::ImplTrait { .. } | HirTypeRef::DynTrait { .. } => ty.display(),
        }
    }

    fn print_pat(&self, pat: PatId) -> String {
        match &self.body.pats[pat] {
            Pattern::Wildcard => "_".to_string(),
            Pattern::Literal(literal) => match literal {
                LiteralPattern::Int { value, suffix, .. } => {
                    format!("{}{}", value, suffix.as_deref().unwrap_or_default())
                }
                LiteralPattern::Float { value, suffix, .. } => {
                    format!("{}{}", value, suffix.as_deref().unwrap_or_default())
                }
                LiteralPattern::String(value) => value.clone(),
                LiteralPattern::Char(value) => format!("'{value}'"),
                LiteralPattern::Bool(value) => value.to_string(),
            },
            Pattern::Binding { name, is_mut } => {
                if *is_mut {
                    format!("mut {}", name.0)
                } else {
                    name.0.clone()
                }
            }
            Pattern::Reference { mutable, pattern } => {
                let prefix = if *mutable { "&mut " } else { "&" };
                format!("{prefix}{}", self.print_pat(*pattern))
            }
            Pattern::Path { path } => path.display(),
            Pattern::Tuple { elements } => {
                let inner = elements
                    .iter()
                    .map(|p| self.print_pat(*p))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
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
                    .map(|fp| {
                        fp.pat.as_ref().map_or_else(
                            || fp.name.0.clone(),
                            |p| format!("{}: {}", fp.name.0, self.print_pat(*p)),
                        )
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
