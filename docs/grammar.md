```
program = statement*;

statement = var_decl | func_decl | block | expr_stmt;

var_decl = "let" ident (":" ty)? ("=" expression)?;

param = ident ":" ty;

func_decl = "fun" ident "(" (param ("," param)*)? ")" ("->" ty) (block | ";");

block = "{" statement* "}";

expr_stmt = expression ";";

expression = unary | binary | block | ident | number;

unary = ("+" | "-" | "&" | "*") expression

// Top-down priority order 
binary = 
      expression ("*" | "/") expression
    | expression ("+" | "-") expression

ty = ident | "&" ty;

ident = [a-zA-Z_][a-zA-Z0-9_]*;
number = [1-9][0-9]* | "0";
```