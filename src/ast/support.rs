use crate::frontend::syntax_kind::{SyntaxKind, SyntaxNode, SyntaxToken};

pub fn token_of(parent: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    parent
        .children_with_tokens()
        .filter_map(|it| it.into_token())
        .find(|t| t.kind() == kind)
}

pub fn child<N: AstNode>(parent: &SyntaxNode) -> Option<N> {
    parent.children().find_map(N::cast)
}

pub fn children<'a, N: AstNode + 'a>(parent: &SyntaxNode) -> impl Iterator<Item = N> + 'a {
    parent.children().filter_map(N::cast)
}

pub trait AstNode: Sized {
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}
