use la_arena::{Arena, Idx};
use rowan::ast::SyntaxNodePtr;
use std::collections::HashMap;

use frontend::syntax_kind::RiddleLang;
use hir::{
    Name,
    body::{BodyId, ExprId, StmtId},
    item_tree::{ConstId, EnumId, FunctionId, ModuleId, StructId, TraitId, TypeAliasId},
};

pub mod builder;
pub mod resolve;

pub type NodeId = Idx<Node>;
pub type EdgeId = Idx<Edge>;
pub type FragId = Idx<Fragment>;

#[derive(Debug)]
pub struct ScopeGraph {
    pub nodes: Arena<Node>,
    pub edges: Arena<Edge>,
    pub fragments: Arena<Fragment>,

    /// Global root scope. `PathAnchor::Absolute` and `PathAnchor::Crate` resolve here.
    pub root: NodeId,

    /// Maps each syntax pointer to the fragment that owns it for incremental invalidation.
    pub by_ptr: HashMap<SyntaxNodePtr<RiddleLang>, FragId>,

    /// Outgoing edge index: `from -> [edge ids]`.
    pub out_edges: HashMap<NodeId, Vec<EdgeId>>,

    /// Partial path cache indexed by start node.
    pub partial_index: HashMap<NodeId, Vec<PartialPath>>,
}

#[derive(Debug, Clone)]
pub enum Node {
    /// A scope container.
    Scope(ScopeKind),

    /// `PushSymbol(x)` pushes `x` onto the symbol stack before following outgoing edges.
    PushSymbol { name: Name },

    /// `PopSymbol(x) -> def` matches and pops `x` from the stack top, then yields `def`.
    PopSymbol { name: Name, define: DefRef },

    /// Moves the cursor to another scope without changing the symbol stack.
    JumpToScope { target: NodeId },

    /// Reference entry point for an `Expr::Path` query.
    Reference {
        segments: Vec<Name>,
        anchor: NodeId,
        origin: RefOrigin,
    },

    /// Tombstone left after a fragment is removed.
    Tombstone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefOrigin {
    Expr { body: BodyId, expr: ExprId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Root,
    Block,
    /// Internal module scope.
    ModInternal,
    /// Exported module scope used by external lookups.
    ModExported,
    /// Shared scope for function parameters and the body root.
    FunctionScope,
}

#[derive(Debug, Clone)]
pub enum DefRef {
    Function(FunctionId),
    Struct(StructId),
    Enum(EnumId),
    Trait(TraitId),
    Const(ConstId),
    TypeAlias(TypeAliasId),
    Module {
        id: ModuleId,
        /// Scope used when resolving the remaining path inside the module.
        enter: NodeId,
    },
    Local {
        stmt: StmtId,
    },
    /// Binding introduced by a match-arm pattern, e.g. `x` in `x => ...`.
    PatternBinding {
        name: Name,
    },
    Param {
        fn_id: FunctionId,
        index: usize,
    },
    /// Alias introduced by `use foo::bar as X;`.
    ///
    /// After matching `X`, resolution restarts at `anchor` with the remaining path segments
    /// rewritten through `rewrite_to`.
    UseAlias {
        rewrite_to: Vec<Name>,
        anchor: NodeId,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub precedence: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Lexical parent edge used when a name is not found in the current scope.
    Lex,
    /// Edge from a scope to a definition pop node.
    Def,
    /// Edge used to expose a child scope, such as `exported -> internal` for modules.
    Export,
}

#[derive(Debug)]
pub struct Fragment {
    pub ptr: SyntaxNodePtr<RiddleLang>,
    pub nodes: Vec<NodeId>,
    pub edges: Vec<EdgeId>,
    /// Entry scope exposed by this fragment.
    pub entry_scope: NodeId,
    /// Parent anchor scope connected through a lexical edge.
    pub parent_anchor: Option<NodeId>,
}

#[derive(Debug, Clone)]
pub struct PartialPath {
    pub start: NodeId,
    pub end: NodeId,
    /// Symbols pushed along the path, in traversal order.
    pub push_syms: Vec<Name>,
    /// Symbols popped along the path, in traversal order.
    pub pop_syms: Vec<Name>,
    /// Edges traversed by this path, used for cache invalidation.
    pub edges: Vec<EdgeId>,
}

impl ScopeGraph {
    pub fn new() -> Self {
        let mut nodes = Arena::new();
        let root = nodes.alloc(Node::Scope(ScopeKind::Root));
        let mut out_edges = HashMap::new();
        out_edges.insert(root, vec![]);
        Self {
            nodes,
            edges: Arena::new(),
            fragments: Arena::new(),
            root,
            by_ptr: HashMap::new(),
            out_edges,
            partial_index: HashMap::new(),
        }
    }

    pub fn alloc_node(&mut self, n: Node) -> NodeId {
        let id = self.nodes.alloc(n);
        self.out_edges.insert(id, vec![]);
        id
    }

    pub fn add_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind, prec: i8) -> EdgeId {
        let id = self.edges.alloc(Edge {
            from,
            to,
            kind,
            precedence: prec,
        });
        self.out_edges.entry(from).or_default().push(id);
        id
    }

    pub fn add_partial(&mut self, p: PartialPath) {
        self.partial_index.entry(p.start).or_default().push(p);
    }

    /// Invalidates the fragment for a syntax pointer.
    ///
    /// Nodes are replaced with tombstones and their outgoing edges are removed from indexes.
    pub fn invalidate(&mut self, ptr: &SyntaxNodePtr<RiddleLang>) -> Option<NodeId> {
        let fid = self.by_ptr.remove(ptr)?;

        // Swap the fragment out because `Fragment` does not implement `Default`.
        let frag = std::mem::replace(
            &mut self.fragments[fid],
            Fragment {
                ptr: ptr.clone(),
                nodes: vec![],
                edges: vec![],
                entry_scope: self.root, // Placeholder.
                parent_anchor: None,
            },
        );

        // 1. Replace nodes with tombstones.
        for nid in &frag.nodes {
            self.nodes[*nid] = Node::Tombstone;
            self.partial_index.remove(nid);
            self.out_edges.remove(nid);
        }

        // 2. Remove edges from `out_edges`. The source node may already be a tombstone.
        for eid in &frag.edges {
            let from = self.edges[*eid].from;
            if let Some(v) = self.out_edges.get_mut(&from) {
                v.retain(|x| x != eid);
            }
        }

        // 3. Remove partial paths that traverse dead edges. This is a simple full scan;
        //    larger graphs can replace it with a reverse edge index later.
        let dead_edges: std::collections::HashSet<EdgeId> = frag.edges.iter().copied().collect();
        for vec in self.partial_index.values_mut() {
            vec.retain(|p| !p.edges.iter().any(|e| dead_edges.contains(e)));
        }

        Some(frag.entry_scope)
    }
}
