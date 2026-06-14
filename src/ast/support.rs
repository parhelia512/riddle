use crate::frontend::syntax_kind::{SyntaxKind, SyntaxNode, SyntaxToken};

pub fn token(parent: &SyntaxNode, predicate: impl Fn(SyntaxKind) -> bool) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| predicate(t.kind()))
}

pub fn token_of(parent: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    token(parent, |it| it == kind)
}

pub fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    children(parent).next()
}

pub fn nth_child<N: AstNode>(parent: &SyntaxNode, n: usize) -> Option<N> {
    children(parent).nth(n)
}

pub fn last_child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    children(parent).last()
}

pub fn children<'a, N: AstNode + 'a>(parent: &SyntaxNode) -> impl Iterator<Item = N> + 'a {
    parent.children().filter_map(N::cast)
}

pub trait AstNode: Sized {
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}
