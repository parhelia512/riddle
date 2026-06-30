use frontend::syntax_kind::{SyntaxKind, SyntaxNode, SyntaxToken};
use rowan::TextRange;

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

pub fn trimmed_range(node: &SyntaxNode) -> TextRange {
    let Some(first) = first_non_trivia_token(node) else {
        return node.text_range();
    };
    let Some(last) = last_non_trivia_token(node) else {
        return node.text_range();
    };
    TextRange::new(first.text_range().start(), last.text_range().end())
}

pub trait AstNode: Sized {
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

fn first_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let node_range = node.text_range();
    let mut token = node.first_token()?;

    loop {
        if !token.kind().is_trivia() {
            return Some(token);
        }
        let next = token.next_token()?;
        if !range_contains(node_range, next.text_range()) {
            return None;
        }
        token = next;
    }
}

fn last_non_trivia_token(node: &SyntaxNode) -> Option<SyntaxToken> {
    let node_range = node.text_range();
    let mut token = node.last_token()?;

    loop {
        if !token.kind().is_trivia() {
            return Some(token);
        }
        let previous = token.prev_token()?;
        if !range_contains(node_range, previous.text_range()) {
            return None;
        }
        token = previous;
    }
}

fn range_contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start() <= inner.start() && inner.end() <= outer.end()
}
