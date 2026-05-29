use rowan::{GreenNodeBuilder, Language};

use super::{
    lexer::Token,
    parser::Event,
    syntax_kind::{Lang, SyntaxKind, SyntaxNode},
};

pub struct Parse {
    pub green: rowan::GreenNode,
    pub errors: Vec<String>,
}

impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }

    pub fn debug_tree(&self) -> String {
        format!("{:#?}", self.syntax())
    }
}

pub fn build_tree(events: Vec<Event>, tokens: Vec<Token>, errors: Vec<String>) -> Parse {
    let mut builder = GreenNodeBuilder::new();
    let mut token_idx: usize = 0;

    // Processing forward_parent to reorder the order of StartNode.
    let mut reordered: Vec<Option<Event>> = events.into_iter().map(Some).collect();
    let len = reordered.len();

    let mut i = 0;
    while i < len {
        if let Some(Event::StartNode {
            forward_parent: Some(_),
            ..
        }) = &reordered[i]
        {
            let mut chain = Vec::new();
            let mut current = i;
            loop {
                match &reordered[current] {
                    Some(Event::StartNode {
                        forward_parent: Some(offset),
                        ..
                    }) => {
                        chain.push(current);
                        current += offset;
                    }
                    Some(Event::StartNode {
                        forward_parent: None,
                        ..
                    }) => {
                        chain.push(current);
                        break;
                    }
                    _ => break,
                }
            }

            // reverses
            for &idx in chain.iter().rev() {
                if let Some(Event::StartNode { kind, .. }) = &reordered[idx] {
                    if *kind != SyntaxKind::Tombstone {
                        builder.start_node(Lang::kind_to_raw(*kind));
                    }
                }
                if idx != i {
                    reordered[idx] = None;
                }
            }
            reordered[i] = None;
            i += 1;
            continue;
        }

        match reordered[i].take() {
            Some(Event::StartNode {
                kind,
                forward_parent: None,
            }) => {
                if kind != SyntaxKind::Tombstone {
                    builder.start_node(Lang::kind_to_raw(kind));
                }
            }
            Some(Event::StartNode {
                forward_parent: Some(_),
                ..
            }) => unreachable!("forward_parent chain should have been handled above"),
            Some(Event::FinishNode) => {
                builder.finish_node();
            }
            Some(Event::AddToken) => {
                if token_idx < tokens.len() {
                    let tok = &tokens[token_idx];
                    builder.token(Lang::kind_to_raw(tok.kind), &tok.text);
                    token_idx += 1;
                }
            }
            None => {}
        }
        i += 1;
    }

    Parse {
        green: builder.finish(),
        errors,
    }
}
