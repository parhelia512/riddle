//! Deterministic parser fuzzing.
//!
//! Seeded rounds of token soup, corpus mutations, and incremental edits that
//! must never panic, hang, or lose source text. The RNG is a tiny xorshift so
//! failures print one reproducible seed instead of a byte dump, and the whole
//! suite stays on stable Rust with no fuzzing toolchain.

use frontend::incremental::{EditError, IncrementalParser, parse_fragment};
use frontend::parser::ReparseEntry;
use syntax::SyntaxKind;

/// Deterministic xorshift64* — reproducible across platforms.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Zero would cycle forever; nudge it like the interpreter's seed.
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        assert!(bound > 0);
        (self.next() % bound as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

/// Token inventory spanning every lexer class: keywords, contextual idents,
/// literals (including raw and numeric-suffix forms), delimiters, and every
/// operator family (single, double, and triple glyph).
const TOKENS: &[&str] = &[
    "fun",
    "let",
    "mut",
    "if",
    "else",
    "while",
    "loop",
    "for",
    "in",
    "match",
    "return",
    "break",
    "continue",
    "struct",
    "enum",
    "trait",
    "impl",
    "use",
    "mod",
    "pub",
    "unsafe",
    "extern",
    "dyn",
    "as",
    "where",
    "type",
    "const",
    "true",
    "false",
    "r#a",
    "value",
    "Self",
    "0",
    "42",
    "1_000",
    "0x1F",
    "1i32",
    "2u64",
    "-7",
    "1.5",
    "2.25f32",
    "'c'",
    "'\\n'",
    "\"text\"",
    "\"a\\tb\"",
    "r\"raw\"",
    "r#\"raw\"#",
    "b\"bytes\"",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ",",
    ";",
    ":",
    "::",
    ".",
    "..",
    "..=",
    "=",
    "==",
    "!",
    "!=",
    "&",
    "&&",
    "|",
    "||",
    "^",
    "+",
    "-",
    "*",
    "/",
    "%",
    "<",
    ">",
    "<=",
    ">=",
    "<<",
    ">>",
    "->",
    "=>",
    "@",
    "#",
    "?",
    "$",
    "\\",
    "~",
    "fun fun",
    "))",
    "((()))",
    "{{}}",
    "[[",
    "]]",
    "..=",
    "@attr",
    "#[",
    // Whitespace and comment glyphs. The parser's lossless tree must survive
    // line breaks, and `//` is the one token whose right edge can run past any
    // node — the incremental splicer special-cases it, so it has to be fuzzed.
    "\n",
    "\r\n",
    "\r",
    "\t",
    "//",
    "// c\n",
    "///",
    "//<",
    "/*",
    "*/",
    "/**",
    "/* */",
];

/// Insertion payloads for incremental edits beyond [`TOKENS`]: multi-byte text,
/// so edit offsets must be re-aligned to character boundaries the way an LSP's
/// would be.
const UTF8_TOKENS: &[&str] = &["中", "é", "\u{1F600}", "你好", "__", "//", "x"];

/// Valid programs whose mutations should mostly stay parseable and must never
/// crash the parser when they do not.
const CORPUS: &[&str] = &[
    "fun main() { println!(\"hello\"); }",
    "fun fib(n: i32) -> i32 { if n < 2 { n } else { fib(n - 2) + fib(n - 1) } }",
    "struct Point { x: i32, y: i32 } fun main() { let p = Point { x: 1, y: 2 }; }",
    "fun main() { let list = vec![1, 2, 3]; let r = 0..5; for v in &list { } }",
    "fun main() { let add = [a: i32, b: i32 -> a + b]; add(1, 2); }",
    "match value { Option::Some(x) => x, Option::None => 0, }",
    "use std::collections::{HashMap, HashSet};",
    "#[lang = \"drop\"] trait Drop { fun drop(&mut self); }",
    "fun main() { let s = r#\"raw \\ string\"#; let c = 'x'; }",
    // Multi-line programs with comments: editing one line must not let a `//`
    // token's edge escape the fragment the splicer replaces.
    "fun main() {\n    let x = 1; // one\n    let y = 2;\n}\n",
    "/// Doc.\nfun main() {\n    /* block */\n    println!(\"{}\");\n}\n",
    "mod m {\n    pub struct S { pub f: i32 }\n\n    impl S {\n        pub fun f(&self) -> i32 { self.f }\n    }\n}\n",
];

fn parse_fresh(source: &str) -> frontend::tree_builder::Parse {
    let mut parser = IncrementalParser::new();
    parser.set_source(source).clone()
}

/// Largest character boundary at or before `offset`. `str::floor_char_boundary`
/// is unstable, and the fuzzer needs it to place edits that never split a
/// code point after the alphabet grew to multi-byte insertions.
fn floor_char_boundary(source: &str, offset: usize) -> usize {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[test]
fn parser_survives_token_soup() {
    let mut rng = Rng::new(0x5EED_0001);
    for round in 0..2000 {
        let length = 1 + rng.below(120);
        let mut source = String::new();
        for i in 0..length {
            if i > 0 {
                source.push(' ');
            }
            source.push_str(rng.pick(TOKENS));
        }
        let parse = parse_fresh(&source);
        // The green tree is lossless: every byte of input, errors included,
        // survives a round trip through the syntax tree.
        assert_eq!(
            parse.syntax().to_string(),
            source,
            "text loss in round {round}"
        );
    }
}

#[test]
fn parser_survives_corpus_mutations() {
    let mut rng = Rng::new(0x5EED_0002);
    for round in 0..2000 {
        let base = rng.pick(CORPUS);
        let mut source = base.to_string();
        for _ in 0..(1 + rng.below(4)) {
            match rng.below(3) {
                // Insert a random token at a random boundary.
                0 => {
                    let byte = rng.below(source.len() + 1);
                    let token = rng.pick(TOKENS);
                    source.insert_str(byte, token);
                }
                // Delete a random span.
                1 => {
                    if !source.is_empty() {
                        let start = rng.below(source.len());
                        let end = (start + 1 + rng.below(12)).min(source.len());
                        source.replace_range(start..end, "");
                    }
                }
                // Splice a corpus fragment into the source.
                _ => {
                    let byte = rng.below(source.len() + 1);
                    let fragment = rng.pick(CORPUS);
                    source.insert_str(byte, fragment);
                }
            }
        }
        let parse = parse_fresh(&source);
        assert_eq!(
            parse.syntax().to_string(),
            source,
            "text loss in round {round}"
        );
    }
}

#[test]
fn parser_survives_unicode_noise() {
    let mut rng = Rng::new(0x5EED_0003);
    let alphabet: Vec<char> = "αβγδ\u{4e2d}\u{6587}\u{1F600}\"'\\#.0abfun{}()"
        .chars()
        .collect();
    for round in 0..1000 {
        let length = 1 + rng.below(90);
        let source: String = (0..length)
            .map(|_| alphabet[rng.below(alphabet.len())])
            .collect();
        let parse = parse_fresh(&source);
        assert_eq!(
            parse.syntax().to_string(),
            source,
            "text loss in round {round}"
        );
    }
}

#[test]
fn incremental_edits_stay_equivalent_to_full_reparse() {
    let mut rng = Rng::new(0x5EED_0004);
    for round in 0..2000 {
        let base = rng.pick(CORPUS);
        let mut source = base.to_string();
        let mut parser = IncrementalParser::new();
        parser.set_source(&source);
        for edit in 0..12 {
            if source.is_empty() {
                break;
            }
            // Edits land on random byte offsets, floored to a character
            // boundary so multi-byte insertions stay legal. `delete_len` then
            // crosses whole code points only.
            let offset = floor_char_boundary(&source, rng.below(source.len()));
            let delete_span = rng.below((source.len() - offset).min(8) + 1);
            let delete_len = floor_char_boundary(&source, offset + delete_span) - offset;
            let insert = if rng.below(TOKENS.len() + UTF8_TOKENS.len()) < TOKENS.len() {
                rng.pick(TOKENS)
            } else {
                rng.pick(UTF8_TOKENS)
            };
            let mutated = format!(
                "{}{}{}",
                &source[..offset],
                insert,
                &source[offset + delete_len..]
            );
            let incremental = parser
                .try_apply_edit(offset, delete_len, insert)
                .unwrap()
                .clone();
            let fresh = parse_fresh(&mutated);
            assert_eq!(
                incremental.syntax().to_string(),
                mutated,
                "incremental text drift in round {round} edit {edit}"
            );
            assert_eq!(
                format!("{:?}", incremental.syntax()),
                format!("{:?}", fresh.syntax()),
                "incremental tree diverged from full reparse in round {round} edit {edit}"
            );
            source = mutated;
        }
    }
}

/// An edit that splits a UTF-8 code point is rejected, and rejecting it must
/// leave the parser's own source and tree untouched so a caller can retry.
#[test]
fn incremental_edits_reject_split_code_points() {
    let source = "fun main() { let s = \"\u{4e2d}\u{1F600}\"; }";
    let mut parser = IncrementalParser::new();
    parser.set_source(source);

    let mid = source
        .char_indices()
        .find(|&(_, c)| c == '\u{4e2d}')
        .map(|(i, _)| i + 1)
        .expect("multi-byte char present");
    assert!(
        !source.is_char_boundary(mid),
        "probe must land inside a code point"
    );

    // Splitting at the start offset, and splitting at the end offset while the
    // start is aligned, are the two arms of `validate_edit`.
    for (offset, delete_len, expected) in [
        (mid, 0, EditError::NotCharBoundary { offset: mid }),
        (mid - 1, 2, EditError::NotCharBoundary { offset: mid + 1 }),
    ] {
        let err = parser
            .try_apply_edit(offset, delete_len, "x")
            .expect_err("edit splits a code point");
        assert_eq!(err, expected);
        assert_eq!(parser.source(), source, "rejected edit mutated state");
    }

    // The same offset aligned to a boundary still parses.
    let aligned = floor_char_boundary(source, mid);
    let retry = parser
        .try_apply_edit(aligned, 0, "//")
        .expect("boundary-aligned edit");
    assert_eq!(
        retry.syntax().to_string(),
        format!("{}//{}", &source[..aligned], &source[aligned..])
    );
}

/// Regression: replacing the head of a `for` header glues `for` into one
/// identifier, so the `ForExpr` fragment reparses with a different root kind
/// (`StructExpr`). `SyntaxNode::replace_with` asserts kinds match, which used to
/// be a hard panic reachable from LSP typing; the splicer must skip that node
/// and let an ancestor (or a full reparse) produce the canonical tree.
#[test]
fn incremental_edit_changing_node_kind_falls_back_cleanly() {
    let source = "fun main() { for i in 0..10 { } }";
    let mut parser = IncrementalParser::new();
    parser.set_source(source);

    // Replace " i in 0..10 " after `for` with `return`: `forreturn{ }` is no
    // longer a `for` loop to the fragment parser.
    let (offset, delete_len, insert) = (16, 12, "return");

    // Why this is the hard case: the edited slice is still lexically valid, but
    // the fragment parser reads it as a struct expression, so the splicer's
    // replacement node kind no longer matches the `ForExpr` it replaces.
    let divergent = parse_fragment("forreturn{ }", ReparseEntry::Expression)
        .expect("the edited fragment parses");
    assert_eq!(divergent.syntax().kind(), SyntaxKind::StructExpr);

    let edited = parser.apply_edit(offset, delete_len, insert).clone();
    let mutated = format!(
        "{}{}{}",
        &source[..offset],
        insert,
        &source[offset + delete_len..]
    );
    assert_eq!(
        mutated, "fun main() { forreturn{ } }",
        "the probe must build the intended source"
    );

    let fresh = parse_fresh(&mutated);
    assert_eq!(edited.syntax().to_string(), mutated);
    assert_eq!(
        format!("{:?}", edited.syntax()),
        format!("{:?}", fresh.syntax()),
        "fallback must still produce the canonical tree"
    );
}

/// Inserting text at the boundary before `v` glues `for` and the insertion into
/// one identifier. The previous tree is error-free, so this reaches the splicer
/// rather than its error bailout, and must still land on the canonical tree.
#[test]
fn incremental_edit_merging_tokens_falls_back_cleanly() {
    let source = "fun main() { for v in &list { } }";
    let mut parser = IncrementalParser::new();
    parser.set_source(source);
    assert!(
        parse_fresh(source).errors.is_empty(),
        "the setup must be error-free to reach the splicer"
    );
    let offset = source.find(" v").expect("`for v` present");
    let edited = parser.apply_edit(offset, 0, "const").clone();
    let mutated = format!("{}const{}", &source[..offset], &source[offset..]);
    let fresh = parse_fresh(&mutated);
    assert_eq!(edited.syntax().to_string(), mutated);
    assert_eq!(
        format!("{:?}", edited.syntax()),
        format!("{:?}", fresh.syntax())
    );
}
