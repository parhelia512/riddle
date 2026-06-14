use la_arena::{Arena, Idx};

pub type ScopeId = Idx<Scope>;

pub struct SourceGraph {
    pub scopes: Arena<Scope>,
}

#[derive(Debug)]
pub struct Scope {
    pub parent: Option<ScopeId>,
}

pub struct Symbol{
    
}