use super::support::{self, AstNode};
use frontend::syntax_kind::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

// ── ast_node! macro ────────────────────────────────────────────────────

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Debug, Clone)]
        pub struct $name {
            syntax: SyntaxNode,
        }

        impl AstNode for $name {
            fn cast(node: SyntaxNode) -> Option<Self> {
                if node.kind() == SyntaxKind::$kind {
                    Some(Self { syntax: node })
                } else {
                    None
                }
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.syntax
            }
        }
    };
}

// ── AST node type definitions ──────────────────────────────────────────
//
// Sorted roughly by category: top-level → statements → expressions →
// types → patterns → paths → extern / unsafe.

// top-level
ast_node!(Root, Root);
ast_node!(ModDecl, ModDecl);
ast_node!(UseDecl, UseDecl);
ast_node!(UseTree, UseTree);
ast_node!(UseTreeList, UseTreeList);
ast_node!(Attribute, Attribute);

// statements / declarations
ast_node!(VarDecl, VarDecl);
ast_node!(FuncDecl, FuncDecl);
ast_node!(ReturnStmt, ReturnStmt);
ast_node!(ExprStmt, ExprStmt);
ast_node!(StructDecl, StructDecl);
ast_node!(EnumDecl, EnumDecl);
ast_node!(EnumVariant, EnumVariant);
ast_node!(TraitDecl, TraitDecl);
ast_node!(ImplDecl, ImplDecl);
ast_node!(ConstDecl, ConstDecl);
ast_node!(TypeAliasDecl, TypeAliasDecl);
ast_node!(GenericParams, GenericParams);

// expressions
ast_node!(Block, Block);
ast_node!(BinaryExpr, BinaryExpr);
ast_node!(UnaryExpr, UnaryExpr);
ast_node!(ParenExpr, ParenExpr);
ast_node!(CallExpr, CallExpr);
ast_node!(ArgList, ArgList);
ast_node!(FieldExpr, FieldExpr);
ast_node!(IndexExpr, IndexExpr);
ast_node!(StructExpr, StructExpr);
ast_node!(StructExprField, StructExprField);
ast_node!(IfStmt, IfStmt);
ast_node!(WhileStmt, WhileStmt);
ast_node!(ForExpr, ForExpr);
ast_node!(MatchExpr, MatchExpr);
ast_node!(MatchArm, MatchArm);
ast_node!(ArrayExpr, ArrayExpr);
ast_node!(NumberExpr, NumberLit);
ast_node!(FloatLitExpr, FloatLit);
ast_node!(StringLitExpr, StringLit);
ast_node!(CharLitExpr, CharLit);
ast_node!(BoolLitExpr, BoolLit);
ast_node!(NameRefExpr, NameRef);
ast_node!(UnsafeExpr, UnsafeExpr);
ast_node!(CastExpr, CastExpr);

// paths
ast_node!(Path, Path);
ast_node!(PathSegment, PathSegment);

// types
ast_node!(NamedType, NamedType);
ast_node!(TypeArgList, TypeArgList);
ast_node!(RefType, RefType);
ast_node!(PtrType, PtrType);
ast_node!(TupleType, TupleType);
ast_node!(ArrayType, ArrayType);
ast_node!(ConstType, ConstType);

// patterns
ast_node!(WildcardPat, WildcardPattern);
ast_node!(LiteralPat, LiteralPattern);
ast_node!(TuplePat, TuplePattern);
ast_node!(StructPattern, StructPattern);
ast_node!(EnumPattern, EnumPattern);

// params
ast_node!(ParamList, ParamList);
ast_node!(Param, Param);
ast_node!(StructFieldList, StructFieldList);
ast_node!(StructField, StructField);

// extern
ast_node!(ExternBlock, ExternBlock);
ast_node!(ExternFnDecl, ExternFnDecl);

// ── Top-level ──────────────────────────────────────────────────────────

impl Root {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        support::children(&self.syntax)
    }
}

// ── Statements ─────────────────────────────────────────────────────────

impl ModDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }

    /// Returns `None` for `mod foo;` and the nested items for `mod foo { ... }`.
    pub fn items(&self) -> Option<impl Iterator<Item = Stmt> + '_> {
        let has_brace = self
            .syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|t| t.kind() == SyntaxKind::LBrace);
        if has_brace {
            Some(support::children::<Stmt>(&self.syntax))
        } else {
            None
        }
    }
}

impl UseDecl {
    pub fn use_tree(&self) -> Option<UseTree> {
        support::child(&self.syntax)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }
}

impl UseTree {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    pub fn alias(&self) -> Option<SyntaxToken> {
        let mut iter = self
            .syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token());
        while let Some(t) = iter.next() {
            if t.kind() == SyntaxKind::As {
                return iter.find(|t| t.kind() == SyntaxKind::Ident);
            }
        }
        None
    }

    pub fn is_glob(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|t| t.kind() == SyntaxKind::Star)
    }

    pub fn subtree_list(&self) -> Option<UseTreeList> {
        support::child(&self.syntax)
    }
}

impl UseTreeList {
    pub fn trees(&self) -> impl Iterator<Item = UseTree> + '_ {
        support::children(&self.syntax)
    }
}

impl Attribute {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn string_value(&self) -> Option<String> {
        let mut after_eq = false;
        for token in self
            .syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
        {
            if token.kind() == SyntaxKind::Eq {
                after_eq = true;
                continue;
            }
            if after_eq && token.kind() == SyntaxKind::String {
                return Some(unquote_string(token.text()));
            }
        }
        None
    }

    pub fn raw_text(&self) -> String {
        self.syntax.text().to_string()
    }
}

pub fn attrs_for_node(node: &SyntaxNode) -> Vec<Attribute> {
    let Some(parent) = node.parent() else {
        return Vec::new();
    };

    let mut pending = Vec::new();
    for element in parent.children_with_tokens() {
        match element {
            rowan::NodeOrToken::Node(candidate) if candidate == *node => return pending,
            rowan::NodeOrToken::Node(candidate) if candidate.kind() == SyntaxKind::Attribute => {
                pending.push(Attribute { syntax: candidate });
            }
            rowan::NodeOrToken::Node(_) => pending.clear(),
            rowan::NodeOrToken::Token(token) if token.kind().is_trivia() => {}
            rowan::NodeOrToken::Token(_) => pending.clear(),
        }
    }

    Vec::new()
}

fn unquote_string(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
        .to_string()
}

impl VarDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_mut(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::Mut)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn init(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl FuncDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }

    pub fn generic_params(&self) -> Option<GenericParams> {
        support::child(&self.syntax)
    }

    pub fn param_list(&self) -> Option<ParamList> {
        support::child(&self.syntax)
    }

    pub fn return_type(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl ReturnStmt {
    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl ExprStmt {
    pub fn expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl StructDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }

    pub fn generic_params(&self) -> Option<GenericParams> {
        support::child(&self.syntax)
    }

    pub fn field_list(&self) -> Option<StructFieldList> {
        support::child(&self.syntax)
    }
}

impl EnumDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }

    pub fn generic_params(&self) -> Option<GenericParams> {
        support::child(&self.syntax)
    }

    pub fn variants(&self) -> impl Iterator<Item = EnumVariant> + '_ {
        support::children(&self.syntax)
    }
}

impl EnumVariant {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn tuple_types(&self) -> impl Iterator<Item = Type> + '_ {
        support::children(&self.syntax)
    }

    pub fn field_list(&self) -> Option<StructFieldList> {
        support::child(&self.syntax)
    }
}

impl TraitDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }

    pub fn methods(&self) -> impl Iterator<Item = FuncDecl> + '_ {
        support::children(&self.syntax)
    }

    pub fn type_aliases(&self) -> impl Iterator<Item = TypeAliasDecl> + '_ {
        support::children(&self.syntax)
    }
}

impl ImplDecl {
    pub fn generic_params(&self) -> Option<GenericParams> {
        support::child(&self.syntax)
    }

    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    pub fn self_type(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn trait_type(&self) -> Option<Type> {
        support::nth_child(&self.syntax, 1)
    }

    pub fn has_for(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(|it| it.into_token())
            .any(|token| token.kind() == SyntaxKind::For)
    }

    pub fn methods(&self) -> impl Iterator<Item = FuncDecl> + '_ {
        support::children(&self.syntax)
    }

    pub fn consts(&self) -> impl Iterator<Item = ConstDecl> + '_ {
        support::children(&self.syntax)
    }

    pub fn type_aliases(&self) -> impl Iterator<Item = TypeAliasDecl> + '_ {
        support::children(&self.syntax)
    }
}

impl ConstDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl TypeAliasDecl {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn is_pub(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Pub).is_some()
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

impl GenericParams {
    pub fn params(&self) -> impl Iterator<Item = GenericParam> + '_ {
        let mut current: Vec<SyntaxElement> = Vec::new();
        let mut params = Vec::new();
        let mut depth = 0usize;
        let mut seen_outer_less = false;

        for element in self.syntax.children_with_tokens() {
            match element.as_token().map(|token| token.kind()) {
                Some(SyntaxKind::Less) if !seen_outer_less => {
                    seen_outer_less = true;
                }
                Some(SyntaxKind::Less) => {
                    depth += 1;
                    current.push(element);
                }
                Some(SyntaxKind::Greater) if depth == 0 => {}
                Some(SyntaxKind::Greater) => {
                    depth -= 1;
                    current.push(element);
                }
                Some(SyntaxKind::Comma) if depth == 0 => {
                    if let Some(param) = GenericParam::from_tokens(&current) {
                        params.push(param);
                    }
                    current.clear();
                }
                _ => current.push(element),
            }
        }
        if let Some(param) = GenericParam::from_tokens(&current) {
            params.push(param);
        }

        params.into_iter()
    }
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub is_const: bool,
    pub bounds: Vec<GenericBound>,
}

#[derive(Debug, Clone)]
pub struct GenericBound {
    pub trait_path: Path,
    pub assoc_constraints: Vec<GenericAssocConstraint>,
}

#[derive(Debug, Clone)]
pub struct GenericAssocConstraint {
    pub name: String,
    pub ty: Type,
}

impl GenericParam {
    fn from_tokens(elements: &[SyntaxElement]) -> Option<Self> {
        let is_const = elements
            .iter()
            .any(|element| matches!(element.as_token(), Some(token) if token.kind() == SyntaxKind::Const));
        let name = elements
            .iter()
            .filter_map(|element| element.as_token())
            .find(|token| token.kind() == SyntaxKind::Ident)
            .map(|token| token.text().to_string())?;
        if is_const {
            return Some(Self {
                name,
                is_const,
                bounds: Vec::new(),
            });
        }
        let colon = elements
            .iter()
            .position(|element| matches!(element.as_token(), Some(token) if token.kind() == SyntaxKind::Colon));
        let bounds = colon
            .map(|index| parse_generic_bounds(&elements[index + 1..]))
            .unwrap_or_default();
        Some(Self {
            name,
            is_const,
            bounds,
        })
    }
}

fn parse_generic_bounds(elements: &[SyntaxElement]) -> Vec<GenericBound> {
    split_elements(elements, SyntaxKind::Plus)
        .into_iter()
        .filter_map(|bound| {
            let trait_path = bound
                .iter()
                .find_map(|element| element.as_node().and_then(|node| Path::cast(node.clone())))?;
            let assoc_constraints = parse_assoc_constraints(&bound);
            Some(GenericBound {
                trait_path,
                assoc_constraints,
            })
        })
        .collect()
}

fn parse_assoc_constraints(elements: &[SyntaxElement]) -> Vec<GenericAssocConstraint> {
    let mut constraints = Vec::new();
    let mut i = 0;
    while i < elements.len() {
        let Some(token) = elements[i].as_token() else {
            i += 1;
            continue;
        };
        if token.kind() != SyntaxKind::Ident {
            i += 1;
            continue;
        }
        let Some(eq_index) = next_non_trivia(elements, i + 1) else {
            i += 1;
            continue;
        };
        if !matches!(elements[eq_index].as_token(), Some(eq) if eq.kind() == SyntaxKind::Eq) {
            i += 1;
            continue;
        }
        let Some(type_index) = next_non_trivia(elements, eq_index + 1) else {
            i += 1;
            continue;
        };
        if let Some(ty) = elements[type_index]
            .as_node()
            .and_then(|node| Type::cast(node.clone()))
        {
            constraints.push(GenericAssocConstraint {
                name: token.text().to_string(),
                ty,
            });
            i = type_index + 1;
        } else {
            i += 1;
        }
    }
    constraints
}

fn split_elements(elements: &[SyntaxElement], separator: SyntaxKind) -> Vec<Vec<SyntaxElement>> {
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0usize;
    for element in elements {
        match element.as_token().map(|token| token.kind()) {
            Some(SyntaxKind::Less) => {
                depth += 1;
                current.push(element.clone());
            }
            Some(SyntaxKind::Greater) => {
                depth = depth.saturating_sub(1);
                current.push(element.clone());
            }
            Some(kind) if kind == separator && depth == 0 => {
                if !current.is_empty() {
                    result.push(current);
                    current = Vec::new();
                }
            }
            _ => current.push(element.clone()),
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn next_non_trivia(elements: &[SyntaxElement], start: usize) -> Option<usize> {
    elements
        .iter()
        .enumerate()
        .skip(start)
        .find_map(|(index, element)| {
            let is_trivia = element
                .as_token()
                .map(|token| token.kind().is_trivia())
                .unwrap_or(false);
            (!is_trivia).then_some(index)
        })
}

// ── Expressions ────────────────────────────────────���───────────────────

impl Block {
    pub fn stmts(&self) -> impl Iterator<Item = Stmt> + '_ {
        support::children(&self.syntax)
    }

    pub fn tail_expr(&self) -> Option<Expr> {
        support::last_child(&self.syntax)
    }
}

impl IfStmt {
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn then_branch(&self) -> Option<Block> {
        support::child(&self.syntax)
    }

    pub fn else_branch(&self) -> Option<ElseBranch> {
        if let Some(else_block) = support::nth_child::<Block>(&self.syntax, 1) {
            return Some(ElseBranch::Block(else_block));
        }
        let if_stmt: Option<IfStmt> = support::child(&self.syntax);
        if_stmt.map(ElseBranch::IfStmt)
    }
}

#[derive(Debug, Clone)]
pub enum ElseBranch {
    Block(Block),
    IfStmt(IfStmt),
}

impl WhileStmt {
    pub fn condition(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl ForExpr {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn iterable(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl BinaryExpr {
    pub fn lhs(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn rhs(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 1)
    }

    pub fn op_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |kind| {
            matches!(
                kind,
                SyntaxKind::Eq
                    | SyntaxKind::PlusEq
                    | SyntaxKind::MinusEq
                    | SyntaxKind::StarEq
                    | SyntaxKind::SlashEq
                    | SyntaxKind::PercentEq
                    | SyntaxKind::AmpEq
                    | SyntaxKind::PipeEq
                    | SyntaxKind::CaretEq
                    | SyntaxKind::ShlEq
                    | SyntaxKind::ShrEq
                    | SyntaxKind::Plus
                    | SyntaxKind::Minus
                    | SyntaxKind::Star
                    | SyntaxKind::Slash
                    | SyntaxKind::Percent
                    | SyntaxKind::Amp
                    | SyntaxKind::Pipe
                    | SyntaxKind::Caret
                    | SyntaxKind::Shl
                    | SyntaxKind::Shr
                    | SyntaxKind::EqEq
                    | SyntaxKind::BangEq
                    | SyntaxKind::Less
                    | SyntaxKind::Greater
                    | SyntaxKind::LessEq
                    | SyntaxKind::GreaterEq
                    | SyntaxKind::AmpAmp
                    | SyntaxKind::PipePipe
            )
        })
    }
}

impl UnaryExpr {
    pub fn operand(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn op_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |kind| {
            matches!(
                kind,
                SyntaxKind::Plus
                    | SyntaxKind::Minus
                    | SyntaxKind::Amp
                    | SyntaxKind::AmpAmp
                    | SyntaxKind::Star
                    | SyntaxKind::Bang
            )
        })
    }

    pub fn is_mut(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::Mut)
    }
}

impl ParenExpr {
    pub fn inner(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl CallExpr {
    pub fn callee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn arg_list(&self) -> Option<ArgList> {
        support::child(&self.syntax)
    }
}

impl ArgList {
    pub fn args(&self) -> impl Iterator<Item = Expr> + '_ {
        support::children(&self.syntax)
    }
}

impl FieldExpr {
    pub fn base(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn field_name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }
}

impl IndexExpr {
    pub fn base(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn index(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 1)
    }
}

impl StructExpr {
    pub fn path(&self) -> Option<Path> {
        support::child::<NameRefExpr>(&self.syntax)?.path()
    }

    pub fn fields(&self) -> impl Iterator<Item = StructExprField> + '_ {
        support::children(&self.syntax)
    }
}

impl StructExprField {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn value(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl MatchExpr {
    pub fn scrutinee(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn arms(&self) -> impl Iterator<Item = MatchArm> + '_ {
        support::children(&self.syntax)
    }
}

impl MatchArm {
    pub fn pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }

    pub fn guard(&self) -> Option<Expr> {
        let mut exprs = support::children::<Expr>(&self.syntax);
        let first = exprs.next();
        if exprs.next().is_some() { first } else { None }
    }

    pub fn body(&self) -> Option<Expr> {
        support::last_child(&self.syntax)
    }
}

impl ArrayExpr {
    pub fn elements(&self) -> impl Iterator<Item = Expr> + '_ {
        support::children(&self.syntax)
    }

    pub fn is_repeat(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Semi).is_some()
    }

    pub fn repeat_value(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 0)
    }

    pub fn repeat_len(&self) -> Option<Expr> {
        support::nth_child(&self.syntax, 1)
    }
}

impl NumberExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Number)
    }

    pub fn value(&self) -> Option<i64> {
        let text = self.value_token()?;
        let text = text.text();
        let text_no_underscores: String = text.chars().filter(|&c| c != '_').collect();
        let text = &text_no_underscores;
        let (radix, digits) = if let Some(rest) = text.strip_prefix("0x") {
            (16, rest)
        } else if let Some(rest) = text.strip_prefix("0o") {
            (8, rest)
        } else if let Some(rest) = text.strip_prefix("0b") {
            (2, rest)
        } else {
            (10, text.as_str())
        };
        let is_digit = |ch: char| match radix {
            16 => ch.is_ascii_hexdigit(),
            _ => ch.is_ascii_digit(),
        };
        let suffix_start = digits
            .find(|ch: char| !is_digit(ch))
            .unwrap_or(digits.len());
        i64::from_str_radix(&digits[..suffix_start], radix).ok()
    }
}

impl FloatLitExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Float)
    }

    pub fn value(&self) -> Option<f64> {
        let text = self.value_token()?;
        let text = text.text();
        let trimmed = ["f16", "f32", "f64", "f128"]
            .iter()
            .find_map(|suffix| text.strip_suffix(suffix))
            .unwrap_or(text);
        trimmed.parse().ok()
    }
}

impl StringLitExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::String)
    }
}

impl CharLitExpr {
    pub fn value_token(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Char)
    }
}

impl BoolLitExpr {
    pub fn value(&self) -> Option<bool> {
        let t = support::token(&self.syntax, |k| {
            matches!(k, SyntaxKind::True | SyntaxKind::False)
        })?;
        Some(t.kind() == SyntaxKind::True)
    }
}

impl NameRefExpr {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }
}

impl UnsafeExpr {
    pub fn body(&self) -> Option<Block> {
        support::child(&self.syntax)
    }
}

impl CastExpr {
    pub fn base(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

// ── Paths ──────────────────────────────────────────────────────────────

impl Path {
    pub fn segments(&self) -> impl Iterator<Item = PathSegment> + '_ {
        support::children(&self.syntax)
    }

    pub fn is_absolute(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .find(|it| match it {
                rowan::NodeOrToken::Node(_) => true,
                rowan::NodeOrToken::Token(t) => !t.kind().is_trivia(),
            })
            .and_then(|it| it.into_token())
            .map(|t| t.kind() == SyntaxKind::ColonColon)
            .unwrap_or(false)
    }
}

impl PathSegment {
    pub fn name_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(
                k,
                SyntaxKind::Ident | SyntaxKind::SelfKw | SyntaxKind::SuperKw | SyntaxKind::CrateKw
            )
        })
    }
}

// ── Types ──────────────────────────────────────────────────────────────

impl NamedType {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    pub fn type_args(&self) -> Vec<Type> {
        self.syntax
            .children()
            .find_map(TypeArgList::cast)
            .map(|list| list.types().collect())
            .unwrap_or_default()
    }
}

impl TypeArgList {
    pub fn types(&self) -> impl Iterator<Item = Type> + '_ {
        support::children(&self.syntax)
    }
}

impl RefType {
    pub fn inner(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn is_mut(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::Mut)
    }
}

impl PtrType {
    pub fn inner(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn is_mut(&self) -> bool {
        self.syntax
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .any(|t| t.kind() == SyntaxKind::Mut)
    }
}

impl TupleType {
    pub fn elements(&self) -> impl Iterator<Item = Type> + '_ {
        support::children(&self.syntax)
    }
}

impl ArrayType {
    pub fn element(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn len_expr(&self) -> Option<Expr> {
        support::child(&self.syntax)
    }
}

impl ConstType {
    pub fn value(&self) -> Option<usize> {
        support::token_of(&self.syntax, SyntaxKind::Number)?
            .text()
            .parse::<usize>()
            .ok()
    }
}

// ── Patterns ───────────────────────────────────────────────────────────

impl LiteralPat {
    pub fn literal_token(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(
                k,
                SyntaxKind::Number
                    | SyntaxKind::Float
                    | SyntaxKind::String
                    | SyntaxKind::Char
                    | SyntaxKind::True
                    | SyntaxKind::False
            )
        })
    }
}

impl TuplePat {
    pub fn elements(&self) -> impl Iterator<Item = Pattern> + '_ {
        support::children(&self.syntax)
    }
}

impl StructPattern {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn sub_pattern(&self) -> Option<Pattern> {
        support::child(&self.syntax)
    }
}

impl EnumPattern {
    pub fn path(&self) -> Option<Path> {
        support::child(&self.syntax)
    }

    pub fn elements(&self) -> impl Iterator<Item = Pattern> + '_ {
        support::children(&self.syntax)
    }

    pub fn fields(&self) -> impl Iterator<Item = StructPattern> + '_ {
        support::children(&self.syntax)
    }
}

// ── Params / Struct fields ─────────────────────────────────────────────

impl ParamList {
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        support::children(&self.syntax)
    }
}

impl Param {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token(&self.syntax, |k| {
            matches!(k, SyntaxKind::Ident | SyntaxKind::SelfKw)
        })
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }

    pub fn is_self_receiver(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::SelfKw).is_some()
    }

    pub fn is_ref(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Amp).is_some()
    }

    pub fn is_mut(&self) -> bool {
        support::token_of(&self.syntax, SyntaxKind::Mut).is_some()
    }
}

impl StructFieldList {
    pub fn fields(&self) -> impl Iterator<Item = StructField> + '_ {
        support::children(&self.syntax)
    }
}

impl StructField {
    pub fn name(&self) -> Option<SyntaxToken> {
        support::token_of(&self.syntax, SyntaxKind::Ident)
    }

    pub fn ty(&self) -> Option<Type> {
        support::child(&self.syntax)
    }
}

// ── Extern ─────────────────────────────────────────────────────────────

impl ExternBlock {
    pub fn functions(&self) -> impl Iterator<Item = FuncDecl> + '_ {
        support::children(&self.syntax)
    }
}

impl ExternFnDecl {
    pub fn func_decl(&self) -> Option<FuncDecl> {
        support::child(&self.syntax)
    }
}

// ── Sum-type enums ─────────────────────────────────────────────────────
//
// Each variant enum: definition → AstNode impl → inherent cast().

// ── Pattern ──

#[derive(Debug, Clone)]
pub enum Pattern {
    Wildcard(WildcardPat),
    Literal(LiteralPat),
    Tuple(TuplePat),
    Struct(StructPattern),
    Enum(EnumPattern),
}

impl AstNode for Pattern {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::WildcardPattern => Some(Pattern::Wildcard(WildcardPat { syntax: node })),
            SyntaxKind::LiteralPattern => Some(Pattern::Literal(LiteralPat { syntax: node })),
            SyntaxKind::TuplePattern => Some(Pattern::Tuple(TuplePat { syntax: node })),
            SyntaxKind::StructPattern => Some(Pattern::Struct(StructPattern { syntax: node })),
            SyntaxKind::EnumPattern => Some(Pattern::Enum(EnumPattern { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Pattern::Wildcard(it) => it.syntax(),
            Pattern::Literal(it) => it.syntax(),
            Pattern::Tuple(it) => it.syntax(),
            Pattern::Struct(it) => it.syntax(),
            Pattern::Enum(it) => it.syntax(),
        }
    }
}

impl Pattern {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}

// ── Stmt ──

#[derive(Debug, Clone)]
pub enum Stmt {
    VarDecl(VarDecl),
    FuncDecl(FuncDecl),
    StructDecl(StructDecl),
    EnumDecl(EnumDecl),
    TraitDecl(TraitDecl),
    ImplDecl(ImplDecl),
    ConstDecl(ConstDecl),
    TypeAliasDecl(TypeAliasDecl),
    ReturnStmt(ReturnStmt),
    ExprStmt(ExprStmt),
    ModDecl(ModDecl),
    UseDecl(UseDecl),
    ExternBlock(ExternBlock),
    ExternFnDecl(ExternFnDecl),
}

impl AstNode for Stmt {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::VarDecl => Some(Stmt::VarDecl(VarDecl { syntax: node })),
            SyntaxKind::FuncDecl => Some(Stmt::FuncDecl(FuncDecl { syntax: node })),
            SyntaxKind::StructDecl => Some(Stmt::StructDecl(StructDecl { syntax: node })),
            SyntaxKind::EnumDecl => Some(Stmt::EnumDecl(EnumDecl { syntax: node })),
            SyntaxKind::TraitDecl => Some(Stmt::TraitDecl(TraitDecl { syntax: node })),
            SyntaxKind::ImplDecl => Some(Stmt::ImplDecl(ImplDecl { syntax: node })),
            SyntaxKind::ConstDecl => Some(Stmt::ConstDecl(ConstDecl { syntax: node })),
            SyntaxKind::TypeAliasDecl => Some(Stmt::TypeAliasDecl(TypeAliasDecl { syntax: node })),
            SyntaxKind::ReturnStmt => Some(Stmt::ReturnStmt(ReturnStmt { syntax: node })),
            SyntaxKind::ExprStmt => Some(Stmt::ExprStmt(ExprStmt { syntax: node })),
            SyntaxKind::ModDecl => Some(Stmt::ModDecl(ModDecl { syntax: node })),
            SyntaxKind::UseDecl => Some(Stmt::UseDecl(UseDecl { syntax: node })),
            SyntaxKind::ExternBlock => Some(Stmt::ExternBlock(ExternBlock { syntax: node })),
            SyntaxKind::ExternFnDecl => Some(Stmt::ExternFnDecl(ExternFnDecl { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Stmt::VarDecl(it) => it.syntax(),
            Stmt::FuncDecl(it) => it.syntax(),
            Stmt::StructDecl(it) => it.syntax(),
            Stmt::EnumDecl(it) => it.syntax(),
            Stmt::TraitDecl(it) => it.syntax(),
            Stmt::ImplDecl(it) => it.syntax(),
            Stmt::ConstDecl(it) => it.syntax(),
            Stmt::TypeAliasDecl(it) => it.syntax(),
            Stmt::ReturnStmt(it) => it.syntax(),
            Stmt::ExprStmt(it) => it.syntax(),
            Stmt::ModDecl(it) => it.syntax(),
            Stmt::UseDecl(it) => it.syntax(),
            Stmt::ExternBlock(it) => it.syntax(),
            Stmt::ExternFnDecl(it) => it.syntax(),
        }
    }
}

impl Stmt {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}

// ── Expr ──

#[derive(Debug, Clone)]
pub enum Expr {
    BinaryExpr(BinaryExpr),
    UnaryExpr(UnaryExpr),
    ParenExpr(ParenExpr),
    CallExpr(CallExpr),
    FieldExpr(FieldExpr),
    IndexExpr(IndexExpr),
    StructExpr(StructExpr),
    Block(Block),
    IfStmt(IfStmt),
    WhileStmt(WhileStmt),
    ForExpr(ForExpr),
    MatchExpr(MatchExpr),
    ArrayExpr(ArrayExpr),
    Number(NumberExpr),
    Float(FloatLitExpr),
    StringLit(StringLitExpr),
    CharLit(CharLitExpr),
    BoolLit(BoolLitExpr),
    NameRef(NameRefExpr),
    UnsafeExpr(UnsafeExpr),
    CastExpr(CastExpr),
}

impl AstNode for Expr {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::BinaryExpr => Some(Expr::BinaryExpr(BinaryExpr { syntax: node })),
            SyntaxKind::UnaryExpr => Some(Expr::UnaryExpr(UnaryExpr { syntax: node })),
            SyntaxKind::ParenExpr => Some(Expr::ParenExpr(ParenExpr { syntax: node })),
            SyntaxKind::CallExpr => Some(Expr::CallExpr(CallExpr { syntax: node })),
            SyntaxKind::FieldExpr => Some(Expr::FieldExpr(FieldExpr { syntax: node })),
            SyntaxKind::IndexExpr => Some(Expr::IndexExpr(IndexExpr { syntax: node })),
            SyntaxKind::StructExpr => Some(Expr::StructExpr(StructExpr { syntax: node })),
            SyntaxKind::Block => Some(Expr::Block(Block { syntax: node })),
            SyntaxKind::IfStmt => Some(Expr::IfStmt(IfStmt { syntax: node })),
            SyntaxKind::WhileStmt => Some(Expr::WhileStmt(WhileStmt { syntax: node })),
            SyntaxKind::ForExpr => Some(Expr::ForExpr(ForExpr { syntax: node })),
            SyntaxKind::MatchExpr => Some(Expr::MatchExpr(MatchExpr { syntax: node })),
            SyntaxKind::ArrayExpr => Some(Expr::ArrayExpr(ArrayExpr { syntax: node })),
            SyntaxKind::NumberLit => Some(Expr::Number(NumberExpr { syntax: node })),
            SyntaxKind::FloatLit => Some(Expr::Float(FloatLitExpr { syntax: node })),
            SyntaxKind::StringLit => Some(Expr::StringLit(StringLitExpr { syntax: node })),
            SyntaxKind::CharLit => Some(Expr::CharLit(CharLitExpr { syntax: node })),
            SyntaxKind::BoolLit => Some(Expr::BoolLit(BoolLitExpr { syntax: node })),
            SyntaxKind::UnsafeExpr => Some(Expr::UnsafeExpr(UnsafeExpr { syntax: node })),
            SyntaxKind::CastExpr => Some(Expr::CastExpr(CastExpr { syntax: node })),
            SyntaxKind::NameRef => Some(Expr::NameRef(NameRefExpr { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Expr::BinaryExpr(it) => it.syntax(),
            Expr::UnaryExpr(it) => it.syntax(),
            Expr::ParenExpr(it) => it.syntax(),
            Expr::CallExpr(it) => it.syntax(),
            Expr::FieldExpr(it) => it.syntax(),
            Expr::IndexExpr(it) => it.syntax(),
            Expr::StructExpr(it) => it.syntax(),
            Expr::Block(it) => it.syntax(),
            Expr::IfStmt(it) => it.syntax(),
            Expr::WhileStmt(it) => it.syntax(),
            Expr::ForExpr(it) => it.syntax(),
            Expr::MatchExpr(it) => it.syntax(),
            Expr::ArrayExpr(it) => it.syntax(),
            Expr::Number(it) => it.syntax(),
            Expr::Float(it) => it.syntax(),
            Expr::StringLit(it) => it.syntax(),
            Expr::CharLit(it) => it.syntax(),
            Expr::BoolLit(it) => it.syntax(),
            Expr::NameRef(it) => it.syntax(),
            Expr::UnsafeExpr(it) => it.syntax(),
            Expr::CastExpr(it) => it.syntax(),
        }
    }
}

impl Expr {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}

// ── Type ──

#[derive(Debug, Clone)]
pub enum Type {
    Named(NamedType),
    Ref(RefType),
    Ptr(PtrType),
    Tuple(TupleType),
    Array(ArrayType),
    Const(ConstType),
}

impl AstNode for Type {
    fn cast(node: SyntaxNode) -> Option<Self> {
        match node.kind() {
            SyntaxKind::RefType => Some(Type::Ref(RefType { syntax: node })),
            SyntaxKind::NamedType => Some(Type::Named(NamedType { syntax: node })),
            SyntaxKind::PtrType => Some(Type::Ptr(PtrType { syntax: node })),
            SyntaxKind::TupleType => Some(Type::Tuple(TupleType { syntax: node })),
            SyntaxKind::ArrayType => Some(Type::Array(ArrayType { syntax: node })),
            SyntaxKind::ConstType => Some(Type::Const(ConstType { syntax: node })),
            _ => None,
        }
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Type::Named(it) => it.syntax(),
            Type::Ref(it) => it.syntax(),
            Type::Ptr(it) => it.syntax(),
            Type::Tuple(it) => it.syntax(),
            Type::Array(it) => it.syntax(),
            Type::Const(it) => it.syntax(),
        }
    }
}

impl Type {
    pub fn cast(node: SyntaxNode) -> Option<Self> {
        <Self as AstNode>::cast(node)
    }
}
