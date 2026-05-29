use super::{
    lexer,
    parser::Parser,
    tree_builder::{self, Parse},
};

pub struct IncrementalParser {
    current: Option<Parse>,
    source: String,
}

impl IncrementalParser {
    pub fn new() -> Self {
        Self {
            current: None,
            source: String::new(),
        }
    }

    /// Full Parsing (First or Backward)
    pub fn set_source(&mut self, source: &str) -> &Parse {
        self.source = source.to_string();
        let tokens = lexer::lex(source);
        let parser = Parser::new(tokens);
        let (events, tokens, errors) = parser.parse();
        let parse = tree_builder::build_tree(events, tokens, errors);
        self.current = Some(parse);
        self.current.as_ref().unwrap()
    }

    /// Apply editing and redirection
    ///
    /// **Note**: The old Parse is dropped here,
    /// But if the external holder holds references to old SyntaxNodes, they remain valid (Arc)
    pub fn apply_edit(&mut self, offset: usize, delete_len: usize, insert: &str) -> &Parse {
        // Apply editing
        let mut new_source = String::with_capacity(self.source.len() - delete_len + insert.len());
        new_source.push_str(&self.source[..offset]);
        new_source.push_str(insert);
        new_source.push_str(&self.source[offset + delete_len..]);

        // Full re-parsing
        // Rowan interning automatically reuses the same subtree
        self.set_source(&new_source.clone());
        // old Parser will be droped
        self.source = new_source;
        self.current.as_ref().unwrap()
    }

    pub fn current_parse(&self) -> Option<&Parse> {
        self.current.as_ref()
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}
