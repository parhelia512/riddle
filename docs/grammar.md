program = statement*;

statement =
    use_decl
  | mod_decl
  | enum_decl
  | trait_decl
  | impl_decl
  | var_decl
  | func_decl
  | struct_decl
  | return_stmt
  | expr_stmt;

// == module / use ==

mod_decl = "mod" ident (";" | "{" statement* "}");

use_decl = "use" use_tree ";";

use_tree =
    path (("as" ident) | ("::" "*") | ("::" "{" use_tree ("," use_tree)* ","? "}"))?
  | "{" use_tree ("," use_tree)* ","? "}";

// == items ==

enum_decl = "enum" ident "{" (enum_variant ("," enum_variant)* ","?)? "}";
enum_variant = ident ("(" type_list? ")")? ("{" struct_field_list? "}")?;

trait_decl = "trait" ident "{" trait_item* "}";
trait_item = func_sig ";" | type_alias_decl ";";

impl_decl = "impl" generic_params? path ("for" ty)? "{" impl_item* "}";
impl_item = func_decl | assoc_type_decl | const_decl;

func_sig = "fun" ident "(" (param ("," param)*)? ")" ("->" ty)?;
assoc_type_decl = "type" ident ("=" ty)? ";";
const_decl = "const" ident ":" ty ("=" expression)? ";";

generic_params = "<" ident ("," ident)* ">";
type_list = ty ("," ty)* ","?;

// == normal statements ==

var_decl = "let" ident (":" ty)? ("=" expression)? ";";

param = ident ":" ty;

func_decl = "fun" ident "(" (param ("," param)*)? ")" ("->" ty)? (block | ";");

block = "{" statement* expression? "}";

struct_param = ident ":" ty;

struct_decl = "struct" ident "{" (struct_param ("," struct_param)* ","?)? "}";

return_stmt = "return" expression? ";";

expr_stmt = expr_without_block ";" | expr_with_block ";"?;

// == expression ==

expression = expr_with_block | expr_without_block;

expr_with_block = block | if_expr | while_expr | match_expr;

if_expr = "if" expression block ("else" (if_expr | block))?;

while_expr = "while" expression block;

match_expr = "match" expression "{" match_arm ("," match_arm)* ","? "}";
match_arm = pattern ("if" expression)? "=>" expression;

expr_without_block = unary (binop unary)*;

unary = prefix_op unary | postfix;

postfix = primary ( "(" arg_list ")" | "." ident | struct_expr_fields | "." ident "(" arg_list ")" )*;

arg_list = (expression ("," expression)*)?;

primary = literal | path | array_expr | "(" expression ")";

array_expr = "[" (expression ("," expression)* ","?)? "]";

struct_expr = path struct_expr_fields;
struct_expr_fields = "{" (struct_expr_field ("," struct_expr_field)* ","?)? "}";
struct_expr_field = ident (":" expression)?;

pattern = "_" | ident | literal | path | tuple_pattern | struct_pattern | enum_pattern;

tuple_pattern = "(" (pattern ("," pattern)* ","?)? ")";
struct_pattern = path "{" (field_pattern ("," field_pattern)* ","?)? "}";
field_pattern = ident (":" pattern)?;
enum_pattern = path | path "(" (pattern ("," pattern)* ","?)? ")" | path "{" (field_pattern ("," field_pattern)* ","?)? "}";

// Precedence & Associativity (Pratt binding powers)
//
// Assignment:
//   =             right-assoc       (lbp=1,  rbp=1)
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
//   &&             left-assoc        (lbp=4,  rbp=5)
//   ||             left-assoc        (lbp=2,  rbp=3)
//
// In `if`, `while`, and `match` heads, struct expressions are disabled so
// `if Foo { ... }` keeps parsing `{ ... }` as the control-flow block.

// == path / type ==

path = ("::")? (ident | "self" | "super" | "crate")
       ("::" (ident | "self" | "super" | "crate"))*;

ty = path | ("&" | "&&") ty | "[" ty ";" expression? "]" | "(" (ty ("," ty)* ","?)? ")";

// == operators ==

prefix_op = "+" | "-" | "&" | "&&" | "*" | "!";

binop = "=" | "||" | "&&" | "==" | "!=" | "<" | ">" | "<=" | ">="
      | "+" | "-" | "*" | "/" | "%";

// == literals ==

literal = int_lit | float_lit | string_lit | char_lit | bool_lit;

int_lit = [0-9]+ ("i8" | "i16" | "i32" | "i64" | "i128" | "isize"
                | "u8" | "u16" | "u32" | "u64" | "u128" | "usize")?;
float_lit = [0-9]+ ("." [0-9]+)? ([eE] [+-]? [0-9]+)? ("f16" | "f32" | "f64" | "f128")?;
string_lit = "\"" ... "\"" | raw_string_lit;
char_lit = "'" ... "'";
bool_lit = "true" | "false";
raw_string_lit = "r" "#"* "\"" ... "\"" "#"*;

ident = [a-zA-Z_][a-zA-Z0-9_]*;
