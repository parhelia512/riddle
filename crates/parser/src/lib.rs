use ast::Node;
use chumsky::prelude::*;
use lexer::Token;

fn parser() -> impl Parser<Token, Vec<Node>, Error = Simple<Token>> {
    let ident = select! {
        Token::Ident(name) => name,
    };

    let ty = recursive(|ty| {
        let symbol = ident.clone().map(|name| Node::Symbol { name });

        let ref_ty = just(Token::Amp)
            .ignore_then(ty.clone())
            .map(|inner| Node::Unary {
                op: "&".into(),
                expr: Box::new(inner),
            });

        ref_ty.or(symbol)
    });

    recursive(|stmt| {
        let block = stmt
            .clone()
            .repeated()
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map(Node::Block);

        let expr = recursive(|expr| {
            let atom = select! {
                Token::Number(n) => Node::Number(n),
                Token::Ident(name) => Node::Symbol { name },
            }
            .or(block.clone())
            .or(expr
                .clone()
                .delimited_by(just(Token::LParen), just(Token::RParen)));

            let unary = just(Token::Plus)
                .to("+")
                .or(just(Token::Minus).to("-"))
                .or(just(Token::Amp).to("&"))
                .or(just(Token::Star).to("*"))
                .repeated()
                .then(atom)
                .map(|(ops, expr)| {
                    ops.into_iter().rev().fold(expr, |expr, op| Node::Unary {
                        op: op.into(),
                        expr: Box::new(expr),
                    })
                });

            let product = unary
                .clone()
                .then(
                    just(Token::Star)
                        .to("*")
                        .or(just(Token::Slash).to("/"))
                        .then(unary)
                        .repeated(),
                )
                .foldl(|lhs, (op, rhs)| Node::Binary {
                    op: op.into(),
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                });

            product
                .clone()
                .then(
                    just(Token::Plus)
                        .to("+")
                        .or(just(Token::Minus).to("-"))
                        .then(product)
                        .repeated(),
                )
                .foldl(|lhs, (op, rhs)| Node::Binary {
                    op: op.into(),
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                })
        });

        let var_decl = just(Token::Let)
            .ignore_then(ident.clone())
            .then(just(Token::Colon).ignore_then(ty.clone()).or_not())
            .then(just(Token::Equal).ignore_then(expr.clone()).or_not())
            .then_ignore(just(Token::Semi))
            .map(|((name, ty), init)| Node::VarDecl {
                name,
                ty: ty.map(Box::new),
                init: init.map(Box::new),
            });

        let param = ident
            .clone()
            .then_ignore(just(Token::Colon))
            .then(ty.clone());

        let params = param
            .separated_by(just(Token::Colon).not().ignore_then(just(Token::Comma)))
            .allow_trailing()
            .delimited_by(just(Token::LParen), just(Token::RParen));

        let func_decl = just(Token::Fun)
            .ignore_then(ident.clone())
            .then(params)
            .then(just(Token::Allow).ignore_then(ty.clone()).or_not())
            .then(block.clone().map(Some).or(just(Token::Semi).to(None)))
            .map(|(((name, params), ret), body)| Node::FuncDecl {
                name,
                params,
                ret: ret.map(Box::new),
                body: body.map(Box::new),
            });

        let expr_stmt = expr
            .then_ignore(just(Token::Semi))
            .map(|expr| Node::ExprStmt(Box::new(expr)));

        var_decl.or(func_decl).or(block).or(expr_stmt)
    })
    .repeated()
    .then_ignore(end())
}

pub fn parse(tokens: Vec<Token>) -> Result<Vec<Node>, Vec<chumsky::error::Simple<Token>>> {
    parser().parse(tokens)
}
