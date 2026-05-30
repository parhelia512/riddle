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

pub fn build_tree(
    events: Vec<Event>,
    tokens: Vec<Token>,
    source: &str,
    errors: Vec<String>,
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
                        Event::StartNode { kind, forward_parent } => {
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
                        builder.start_node(Lang::kind_to_raw(kind));
                    }
                }
            }
            Event::FinishNode => {
                builder.finish_node();
            }
            Event::AddToken => {
                if token_idx < tokens.len() {
                    let tok = &tokens[token_idx];
                    builder.token(Lang::kind_to_raw(tok.kind), tok.text(source));
                    token_idx += 1;
                }
            }
        }
    }

    Parse {
        green: builder.finish(),
        errors,
    }
}
