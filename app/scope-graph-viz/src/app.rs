use ast::support::AstNode;
use frontend::incremental::{IncrementalParser, ReparseMode};
use scope_graph::builder::build_scope_graph;

use crate::{
    edit::single_edit,
    render::{self, Delta, Snapshot},
};

pub struct AppState {
    parser: IncrementalParser,
    initialized: bool,
    previous: Option<Snapshot>,
    initial_source: String,
}

impl AppState {
    pub fn new(initial_source: String) -> Self {
        Self {
            parser: IncrementalParser::new(),
            initialized: false,
            previous: None,
            initial_source,
        }
    }

    pub fn current_source(&mut self) -> &str {
        if !self.initialized {
            self.parser.set_source(&self.initial_source);
            self.initialized = true;
        }
        self.parser.source()
    }

    pub fn render_scope_graph(&mut self, source: &str) -> String {
        let reparse = self.update_parser(source);
        let Some(parse) = self.parser.current_parse() else {
            return render::render_error("parser did not produce a current parse", &[]);
        };

        let syntax = parse.syntax();
        let Some(root) = ast::Root::cast(syntax.clone()) else {
            return render::render_error("root syntax node could not be cast", &parse.errors);
        };

        let hir = hir::lower_root(root);
        let graph = build_scope_graph(&hir, &syntax);
        let snapshot = Snapshot::from_graph(&graph);
        let delta = Delta::new(self.previous.as_ref(), &snapshot);
        let html = render::render_graph(&graph, &delta, &reparse, &parse.errors);
        self.previous = Some(snapshot);
        html
    }

    fn update_parser(&mut self, source: &str) -> ReparseStatus {
        if !self.initialized {
            self.parser.set_source(source);
            self.initialized = true;
            return ReparseStatus::Initial;
        }

        if self.parser.source() == source {
            return ReparseStatus::Unchanged;
        }

        let old_source = self.parser.source().to_string();
        let edit = single_edit(&old_source, source);
        match self
            .parser
            .try_apply_edit(edit.offset, edit.delete_len, &edit.insert)
        {
            Ok(_) => match self.parser.last_reparse_mode() {
                ReparseMode::Full => ReparseStatus::Full {
                    edit: edit.summary(),
                },
                ReparseMode::Incremental(kind) => ReparseStatus::Incremental {
                    kind: format!("{kind:?}"),
                    edit: edit.summary(),
                },
            },
            Err(err) => {
                self.parser.set_source(source);
                ReparseStatus::Fallback {
                    reason: err.to_string(),
                }
            }
        }
    }
}

pub enum ReparseStatus {
    Initial,
    Unchanged,
    Incremental { kind: String, edit: String },
    Full { edit: String },
    Fallback { reason: String },
}

impl ReparseStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Initial => "initial full parse".into(),
            Self::Unchanged => "unchanged".into(),
            Self::Incremental { kind, edit } => format!("incremental {kind} ({edit})"),
            Self::Full { edit } => format!("full parse ({edit})"),
            Self::Fallback { reason } => format!("full parse fallback: {reason}"),
        }
    }

    pub fn class(&self) -> &'static str {
        match self {
            Self::Incremental { .. } => "ok",
            Self::Full { .. } | Self::Fallback { .. } => "warn",
            Self::Initial | Self::Unchanged => "neutral",
        }
    }
}
