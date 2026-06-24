use hir::{
    HirFile, Name,
    body::{BinaryOp, Body, BodyId, BodyItem, Expr, ExprId, ResolvedName, Stmt, StmtId, UnaryOp},
    item_tree::{
        HirFunction, HirTypeRef, HirUseTree, HirUseTreeKind, HirVariantKind, TopLevelItem,
    },
};
use scope_graph::{DefRef, EdgeId, EdgeKind, Node, NodeId, ScopeGraph};

pub fn node_signature(id: NodeId, node: &Node) -> String {
    match node {
        Node::Scope(kind) => format!("n{} Scope::{kind:?}", node_no(id)),
        Node::PushSymbol { name } => format!("n{} PushSymbol({})", node_no(id), name.0),
        Node::PopSymbol { name, define } => {
            format!(
                "n{} PopSymbol({}) {}",
                node_no(id),
                name.0,
                format_def(define)
            )
        }
        Node::JumpToScope { target } => {
            format!("n{} JumpToScope(n{})", node_no(id), node_no(*target))
        }
        Node::Reference {
            segments, anchor, ..
        } => format!(
            "n{} Reference({}) anchor n{}",
            node_no(id),
            path_text(segments),
            node_no(*anchor)
        ),
        Node::Tombstone => format!("n{} Tombstone", node_no(id)),
    }
}

pub fn node_label(node: &Node) -> String {
    match node {
        Node::Scope(kind) => format!("Scope::{kind:?}"),
        Node::PushSymbol { name } => format!("push {}", name.0),
        Node::PopSymbol { name, define } => format!("def {}\n{}", name.0, short_def(define)),
        Node::JumpToScope { target } => format!("jump n{}", node_no(*target)),
        Node::Reference {
            segments, anchor, ..
        } => {
            format!("ref {}\nanchor n{}", path_text(segments), node_no(*anchor))
        }
        Node::Tombstone => "tombstone".into(),
    }
}

pub fn node_class(node: &Node) -> &'static str {
    match node {
        Node::Scope(_) => "scope",
        Node::PushSymbol { .. } => "push",
        Node::PopSymbol { .. } => "pop",
        Node::JumpToScope { .. } => "jump",
        Node::Reference { .. } => "reference",
        Node::Tombstone => "tombstone",
    }
}

pub fn reference_count(graph: &ScopeGraph) -> usize {
    graph
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node, Node::Reference { .. }))
        .count()
}

pub fn path_text(segments: &[Name]) -> String {
    segments
        .iter()
        .map(|name| name.0.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

pub fn short_def(def: &DefRef) -> String {
    match def {
        DefRef::Function(_) => "Function".into(),
        DefRef::Struct(_) => "Struct".into(),
        DefRef::Enum(_) => "Enum".into(),
        DefRef::Trait(_) => "Trait".into(),
        DefRef::Const(_) => "Const".into(),
        DefRef::TypeAlias(_) => "TypeAlias".into(),
        DefRef::Module { enter, .. } => format!("Module -> n{}", node_no(*enter)),
        DefRef::Local { .. } => "Local".into(),
        DefRef::PatternBinding { .. } => "PatternBinding".into(),
        DefRef::Param { index, .. } => format!("Param #{index}"),
        DefRef::UseAlias { rewrite_to, anchor } => {
            format!("UseAlias {} @ n{}", path_text(rewrite_to), node_no(*anchor))
        }
    }
}

pub fn format_def(def: &DefRef) -> String {
    match def {
        DefRef::Function(id) => format!("Function({id:?})"),
        DefRef::Struct(id) => format!("Struct({id:?})"),
        DefRef::Enum(id) => format!("Enum({id:?})"),
        DefRef::Trait(id) => format!("Trait({id:?})"),
        DefRef::Const(id) => format!("Const({id:?})"),
        DefRef::TypeAlias(id) => format!("TypeAlias({id:?})"),
        DefRef::Module { id, enter } => format!("Module({id:?}, enter n{})", node_no(*enter)),
        DefRef::Local { stmt } => format!("Local({stmt:?})"),
        DefRef::PatternBinding { name } => format!("PatternBinding({})", name.0),
        DefRef::Param { fn_id, index } => format!("Param({fn_id:?}, {index})"),
        DefRef::UseAlias { rewrite_to, anchor } => {
            format!(
                "UseAlias({} @ n{})",
                path_text(rewrite_to),
                node_no(*anchor)
            )
        }
    }
}

pub fn edge_kind(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Lex => "Lex",
        EdgeKind::Def => "Def",
        EdgeKind::Export => "Export",
    }
}

pub fn node_no(id: NodeId) -> u32 {
    id.into_raw().into_u32()
}

pub fn edge_no(id: EdgeId) -> u32 {
    id.into_raw().into_u32()
}

pub fn body_no(id: BodyId) -> u32 {
    id.into_raw().into_u32()
}

pub fn expr_no(id: ExprId) -> u32 {
    id.into_raw().into_u32()
}

pub fn function_signature(function: &HirFunction) -> String {
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
    format!("fun {}({params}) -> {ret}", function.name.0)
}

pub fn type_ref_text(ty: &HirTypeRef) -> String {
    match ty {
        HirTypeRef::Named(path) => path.display(),
        HirTypeRef::Ref(inner) => format!("&{}", type_ref_text(inner)),
        HirTypeRef::Tuple(elements) => {
            let inner = elements
                .iter()
                .map(type_ref_text)
                .collect::<Vec<_>>()
                .join(", ");
            format!("({inner})")
        }
        HirTypeRef::Array(inner) => format!("[{}]", type_ref_text(inner)),
        HirTypeRef::Unknown => "_".to_string(),
        HirTypeRef::Error => "<error>".to_string(),
    }
}

pub fn item_symbol_signature(hir: &HirFile, item: &TopLevelItem, module_path: &str) -> String {
    match item {
        TopLevelItem::Function(fid) => {
            let function = &hir.item_tree.functions[*fid];
            format!("{module_path}::{}", function_signature(function))
        }
        TopLevelItem::Struct(sid) => {
            let strukt = &hir.item_tree.structs[*sid];
            let fields = strukt
                .fields
                .iter()
                .map(|field| format!("{}: {}", field.name.0, type_ref_text(&field.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{module_path}::struct {} {{ {fields} }}", strukt.name.0)
        }
        TopLevelItem::Enum(eid) => {
            let enm = &hir.item_tree.enums[*eid];
            let variants = enm
                .variants
                .iter()
                .map(|variant| match &variant.kind {
                    HirVariantKind::Unit => variant.name.0.clone(),
                    HirVariantKind::Tuple(types) => {
                        let inner = types
                            .iter()
                            .map(type_ref_text)
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{}({inner})", variant.name.0)
                    }
                    HirVariantKind::Struct(fields) => {
                        let inner = fields
                            .iter()
                            .map(|field| format!("{}: {}", field.name.0, type_ref_text(&field.ty)))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{} {{ {inner} }}", variant.name.0)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{module_path}::enum {} {{ {variants} }}", enm.name.0)
        }
        TopLevelItem::Trait(tid) => {
            let tr = &hir.item_tree.traits[*tid];
            let methods = tr
                .methods
                .iter()
                .map(function_signature)
                .collect::<Vec<_>>()
                .join("; ");
            let aliases = tr
                .type_aliases
                .iter()
                .map(|alias| {
                    alias
                        .ty
                        .as_ref()
                        .map(|ty| format!("type {} = {}", alias.name.0, type_ref_text(ty)))
                        .unwrap_or_else(|| format!("type {}", alias.name.0))
                })
                .collect::<Vec<_>>()
                .join("; ");
            let mut members = Vec::new();
            if !methods.is_empty() {
                members.push(methods);
            }
            if !aliases.is_empty() {
                members.push(aliases);
            }
            format!(
                "{module_path}::trait {} {{ {} }}",
                tr.name.0,
                members.join("; ")
            )
        }
        TopLevelItem::Const(cid) => {
            let konst = &hir.item_tree.consts[*cid];
            format!(
                "{module_path}::const {}: {}",
                konst.name.0,
                type_ref_text(&konst.ty)
            )
        }
        TopLevelItem::TypeAlias(tid) => {
            let alias = &hir.item_tree.type_aliases[*tid];
            alias
                .ty
                .as_ref()
                .map(|ty| {
                    format!(
                        "{module_path}::type {} = {}",
                        alias.name.0,
                        type_ref_text(ty)
                    )
                })
                .unwrap_or_else(|| format!("{module_path}::type {}", alias.name.0))
        }
        TopLevelItem::Module(mid) => {
            let module = &hir.item_tree.modules[*mid];
            format!("{module_path}::module {}", module.name.0)
        }
        TopLevelItem::Use(uid) => {
            let import = &hir.item_tree.uses[*uid];
            format!("{module_path}::use {}", use_tree_text(&import.tree))
        }
        TopLevelItem::Impl(iid) => {
            let imp = &hir.item_tree.impls[*iid];
            let trait_prefix = imp
                .trait_ty
                .as_ref()
                .map(|ty| format!("{} for ", type_ref_text(ty)))
                .unwrap_or_default();
            let methods = imp
                .methods
                .iter()
                .map(|fid| function_signature(&hir.item_tree.functions[*fid]))
                .collect::<Vec<_>>()
                .join("; ");
            let consts = imp
                .consts
                .iter()
                .map(|cid| {
                    let konst = &hir.item_tree.consts[*cid];
                    format!("const {}: {}", konst.name.0, type_ref_text(&konst.ty))
                })
                .collect::<Vec<_>>()
                .join("; ");
            let aliases = imp
                .type_aliases
                .iter()
                .map(|tid| {
                    let alias = &hir.item_tree.type_aliases[*tid];
                    alias
                        .ty
                        .as_ref()
                        .map(|ty| format!("type {} = {}", alias.name.0, type_ref_text(ty)))
                        .unwrap_or_else(|| format!("type {}", alias.name.0))
                })
                .collect::<Vec<_>>()
                .join("; ");
            let mut members = Vec::new();
            if !methods.is_empty() {
                members.push(methods);
            }
            if !consts.is_empty() {
                members.push(consts);
            }
            if !aliases.is_empty() {
                members.push(aliases);
            }
            format!(
                "{module_path}::impl {trait_prefix}{} {{ {} }}",
                type_ref_text(&imp.self_ty),
                members.join("; ")
            )
        }
    }
}

pub fn use_tree_text(t: &HirUseTree) -> String {
    let prefix = t.prefix.display();
    match &t.kind {
        HirUseTreeKind::Simple { alias: None } => prefix,
        HirUseTreeKind::Simple { alias: Some(alias) } => format!("{prefix} as {}", alias.0),
        HirUseTreeKind::Glob => {
            if prefix.is_empty() {
                "*".to_string()
            } else {
                format!("{prefix}::*")
            }
        }
        HirUseTreeKind::List(children) => {
            let inner = children
                .iter()
                .map(use_tree_text)
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

pub fn stmt_label(hir: &HirFile, body: &Body, stmt: StmtId) -> String {
    match &body.stmts[stmt] {
        Stmt::Let { name, ty, init } => {
            let mut out = format!("let {}", name.0);
            if !matches!(ty, HirTypeRef::Unknown) {
                out.push_str(": ");
                out.push_str(&type_ref_text(ty));
            }
            if let Some(init) = init {
                out.push_str(" = ");
                out.push_str(&expr_label(body, *init));
            }
            out
        }
        Stmt::Expr { expr } => expr_label(body, *expr),
        Stmt::Return { value } => value
            .map(|expr| format!("return {}", expr_label(body, expr)))
            .unwrap_or_else(|| "return".to_string()),
        Stmt::Item { item } => match item {
            BodyItem::Module(mid) => {
                let module = &hir.item_tree.modules[*mid];
                if module.items.is_some() {
                    format!("mod {} {{ ... }}", module.name.0)
                } else {
                    format!("mod {};", module.name.0)
                }
            }
            BodyItem::Use(uid) => format!("use {}", use_tree_text(&hir.item_tree.uses[*uid].tree)),
        },
    }
}

pub fn expr_label(body: &Body, expr: ExprId) -> String {
    match &body.exprs[expr] {
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
        Expr::Path { path, .. } => path.display(),
        Expr::Binary { lhs, rhs, op } => format!(
            "({} {} {})",
            expr_label(body, *lhs),
            binary_op_text(*op),
            expr_label(body, *rhs)
        ),
        Expr::Unary { operand, op } => {
            format!("({}{})", unary_op_text(*op), expr_label(body, *operand))
        }
        Expr::Block { stmts, tail } => {
            let tail = tail
                .map(|tail| format!("; {}", expr_label(body, tail)))
                .unwrap_or_default();
            format!("{{ {} stmt(s){tail} }}", stmts.len())
        }
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let else_text = else_branch
                .map(|expr| format!(" else {}", expr_label(body, expr)))
                .unwrap_or_default();
            format!(
                "if {} {}{}",
                expr_label(body, *cond),
                expr_label(body, *then_branch),
                else_text
            )
        }
        Expr::While { condition, body: b } => {
            format!(
                "while {} {}",
                expr_label(body, *condition),
                expr_label(body, *b)
            )
        }
        Expr::Match { scrutinee, arms } => {
            format!(
                "match {} {{ {} arm(s) }}",
                expr_label(body, *scrutinee),
                arms.len()
            )
        }
        Expr::Array { elements } => {
            let elements = elements
                .iter()
                .map(|element| expr_label(body, *element))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{elements}]")
        }
        Expr::Struct { path, fields, .. } => {
            let fields = fields
                .iter()
                .map(|field| format!("{}: {}", field.name.0, expr_label(body, field.value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {fields} }}", path.display())
        }
        Expr::Call { callee, args } => {
            let args = args
                .iter()
                .map(|arg| expr_label(body, *arg))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({args})", expr_label(body, *callee))
        }
        Expr::FieldAccess { base, field } => {
            format!("{}.{}", expr_label(body, *base), field.0)
        }
    }
}

pub fn resolved_name_text(hir: &HirFile, resolved: Option<&ResolvedName>) -> String {
    match resolved {
        Some(ResolvedName::Local(stmt)) => format!("Local({stmt:?})"),
        Some(ResolvedName::Param(index)) => format!("Param #{index}"),
        Some(ResolvedName::Function(id)) => {
            format!("Function({id:?}, {})", hir.item_tree.functions[*id].name.0)
        }
        Some(ResolvedName::Struct(id)) => {
            format!("Struct({id:?}, {})", hir.item_tree.structs[*id].name.0)
        }
        Some(ResolvedName::Enum(id)) => {
            format!("Enum({id:?}, {})", hir.item_tree.enums[*id].name.0)
        }
        Some(ResolvedName::Trait(id)) => {
            format!("Trait({id:?}, {})", hir.item_tree.traits[*id].name.0)
        }
        Some(ResolvedName::Const(id)) => {
            format!("Const({id:?}, {})", hir.item_tree.consts[*id].name.0)
        }
        Some(ResolvedName::TypeAlias(id)) => {
            format!(
                "TypeAlias({id:?}, {})",
                hir.item_tree.type_aliases[*id].name.0
            )
        }
        Some(ResolvedName::Module(id)) => {
            format!("Module({id:?}, {})", hir.item_tree.modules[*id].name.0)
        }
        Some(ResolvedName::Unresolved) => "Unresolved".to_string(),
        None => "not resolved".to_string(),
    }
}

pub fn binary_op_text(op: BinaryOp) -> &'static str {
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

pub fn unary_op_text(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Pos => "+",
        UnaryOp::Ref => "&",
        UnaryOp::Deref => "*",
        UnaryOp::Not => "!",
    }
}

pub fn wrap_label(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut current = String::new();
        for word in line.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.len() + word.len() + 1 <= max_chars {
                current.push(' ');
                current.push_str(word);
            } else {
                out.push(current);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}
