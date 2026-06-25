use hir::Name;
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
