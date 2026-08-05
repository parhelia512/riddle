use rowan::{GreenNodeBuilder, GreenToken, Language, NodeOrToken, TextRange, TextSize};

use super::{
    lexer::Token,
    parser::{Event, ParseError},
};
use syntax::{RiddleLang, SyntaxKind, SyntaxNode};

#[derive(Debug, Clone)]
pub struct Parse {
    pub green: rowan::GreenNode,
    pub errors: Vec<ParseError>,
}

impl Parse {
    #[must_use]
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    #[must_use]
    pub fn debug_tree(&self) -> String {
        format!("{:#?}", self.syntax())
    }
}

#[must_use]
pub fn append_parse(left: &Parse, separator: &str, right: &Parse) -> Parse {
    let index = left.green.children().count();
    let children = std::iter::once(NodeOrToken::Token(GreenToken::new(
        RiddleLang::kind_to_raw(SyntaxKind::Whitespace),
        separator,
    )))
    .chain(right.green.children().map(rowan::NodeOrToken::to_owned));
    let offset = left.green.text_len() + TextSize::of(separator);
    let mut errors = left.errors.clone();
    errors.extend(right.errors.iter().cloned().map(|mut error| {
        error.span = TextRange::new(error.span.start() + offset, error.span.end() + offset);
        error
    }));
    Parse {
        green: left.green.splice_children(index..index, children),
        errors,
    }
}

#[must_use]
pub fn build_tree(
    events: &[Event],
    tokens: &[Token],
    source: &str,
    errors: Vec<ParseError>,
) -> Parse {
    let mut builder = GreenNodeBuilder::new();
    let mut token_idx: usize = 0;
    let mut forward_parents = vec![];
    let mut visited = vec![false; events.len()];

    for i in 0..events.len() {
        if visited[i] {
            continue;
        }

        match &events[i] {
            Event::StartNode { .. } => {
                // collect forward_parent chain
                forward_parents.clear();
                let mut cur = i;
                loop {
                    match &events[cur] {
                        Event::StartNode {
                            kind,
                            forward_parent,
                        } => {
                            forward_parents.push(*kind);
                            visited[cur] = true;
                            match forward_parent {
                                Some(offset) => cur += offset,
                                None => break,
                            }
                        }
                        _ => unreachable!(),
                    }
                }

                // reverses
                for &kind in forward_parents.iter().rev() {
                    if kind != SyntaxKind::Tombstone {
                        builder.start_node(RiddleLang::kind_to_raw(kind));
                    }
                }
            }
            Event::FinishNode => {
                builder.finish_node();
            }
            Event::AddToken => {
                if token_idx < tokens.len() {
                    let tok = &tokens[token_idx];
                    builder.token(RiddleLang::kind_to_raw(tok.kind), tok.text(source));
                    token_idx += 1;
                }
            }
            Event::AddSyntheticToken {
                kind,
                text,
                consume,
            } => {
                builder.token(RiddleLang::kind_to_raw(*kind), text);
                if *consume {
                    token_idx += 1;
                }
            }
            Event::Placeholder => {}
        }
    }

    Parse {
        green: builder.finish(),
        errors,
    }
}
