```
program = statement*;

statement = var_decl | func_decl | struct_decl | return_stmt | expr_stmt;

var_decl = "let" ident (":" ty)? ("=" expression)? ";";

param = ident ":" ty;

func_decl = "fun" ident "(" (param ("," param)*)? ")" ("->" ty)? (block | ";");

block = "{" statement* expression? "}";

struct_param = ident ":" ty;

struct_decl = "struct" ident "{" (struct_param ("," struct_param)* ","?)? "}";

return_stmt = "return" (expression)? ";";

expr_stmt = expr_without_block ";" | expr_with_block ";"?;

// == expression ==

expression = expr_with_block | expr_without_block;

expr_with_block = block | if_expr | while_expr;

if_expr = "if" expression block ("else" (if_expr | block))?;

while_expr = "while" expression block;

expr_without_block = unary (binop unary)*;

unary = prefix_op unary | postfix;

postfix = primary ( "(" arg_list ")" | "." ident )*;

arg_list = (expression ("," expression)*)?;

primary = number | ident | "(" expression ")";

prefix_op = "+" | "-" | "&" | "&&" | "*" | "!";

binop = "||" | "&&" | "==" | "!=" | "<" | ">" | "<=" | ">="
      | "+" | "-" | "*" | "/" | "%";

// Precedence & Associativity (Pratt binding powers)
//
// Prefix (right):  + - & && * !       rbp = 13
//
// Postfix:
//   () .           left-assoc        (lbp = 15)
//
// Infix:
//   *  /  %        left-assoc        (lbp=11, rbp=12)
//   +  -           left-assoc        (lbp=9,  rbp=10)
//   <  >  <=  >=   left-assoc        (lbp=7,  rbp=8)
//   ==  !=         left-assoc        (lbp=5,  rbp=6)
//   &&             left-assoc        (lbp=3,  rbp=4)
//   ||             left-assoc        (lbp=1,  rbp=2)

ty = ident | ("&" | "&&") ty;

ident = [a-zA-Z_][a-zA-Z0-9_]*;
number = [1-9][0-9]* | "0";
```
