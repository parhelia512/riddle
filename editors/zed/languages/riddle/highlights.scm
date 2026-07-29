(identifier) @variable

(metavariable) @variable

(type_identifier) @type

(fragment_specifier) @type

(primitive_type) @type.builtin

(self) @variable.special

(field_identifier) @property

(shorthand_field_identifier) @property

(trait_item
  name: (type_identifier) @type.interface)

(impl_item
  trait: (type_identifier) @type.interface)

(abstract_type
  trait: (type_identifier) @type.interface)

(dynamic_type
  trait: (type_identifier) @type.interface)

(trait_bounds
  (type_identifier) @type.interface)

(call_expression
  function: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
    (field_expression
      field: (field_identifier) @function.method)
  ])

(generic_function
  function: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
    (field_expression
      field: (field_identifier) @function.method)
  ])

(function_item
  name: (identifier) @function.definition)

(function_signature_item
  name: (identifier) @function.definition)

((identifier) @type
  (#match? @type "^[A-Z]"))

((identifier) @constant
  (#match? @constant "^_*[A-Z][A-Z\\d_]*$"))

(enum_variant
  name: (identifier) @type)

[
  "("
  ")"
  "{"
  "}"
  "["
  "]"
] @punctuation.bracket

(_
  .
  "<" @punctuation.bracket
  ">" @punctuation.bracket)

[
  "."
  ";"
  ","
  "::"
] @punctuation.delimiter

"#" @punctuation.special

[
  "as"
  "const"
  "enum"
  "extern"
  "impl"
  "let"
  "mod"
  "move"
  "pub"
  "struct"
  "for"
  "trait"
  "type"
  "unsafe"
  "use"
  "where"
  (crate)
  (mutable_specifier)
  (super)
] @keyword

[
  "break"
  "continue"
  "else"
  "if"
  "in"
  "match"
  "return"
  "while"
] @keyword.control

(for_expression
  "for" @keyword.control)

((identifier) @keyword.control
  (#any-of? @keyword.control
    "if" "else" "while" "for" "in" "match" "break" "continue" "return"))

((identifier) @keyword
  (#any-of? @keyword
    "let" "fun" "struct" "as" "self" "mod" "use" "mut" "pub" "super" "crate"
    "enum" "trait" "impl" "const" "type" "extern" "unsafe" "safe" "where" "move"))

((identifier) @boolean
  (#any-of? @boolean "true" "false"))

[
  (string_literal)
  (raw_string_literal)
  (char_literal)
] @string

(escape_sequence) @string.escape

[
  (integer_literal)
  (float_literal)
] @number

(boolean_literal) @boolean

(line_comment) @comment

[
  "!="
  "%"
  "%="
  "&"
  "&="
  "&&"
  "*"
  "*="
  "+"
  "+="
  "-"
  "-="
  "->"
  "/="
  "<<"
  "<<="
  "<"
  "<="
  "="
  "=="
  "=>"
  ">"
  ">="
  ">>"
  ">>="
  "^"
  "^="
  "|"
  "|="
  "||"
] @operator

(unary_expression
  "!" @operator)

operator: "/" @operator

(parameter
  (identifier) @variable.parameter)

(attribute_item
  (attribute
    [
      (identifier) @attribute
      (scoped_identifier
        name: (identifier) @attribute)
      (token_tree
        (identifier) @attribute
        (#match? @attribute "^[a-z\\d_]*$"))
      (token_tree
        (identifier) @none
        "::"
        (#match? @none "^[a-z\\d_]*$"))
    ]))

(inner_attribute_item
  (attribute
    [
      (identifier) @attribute
      (scoped_identifier
        name: (identifier) @attribute)
      (token_tree
        (identifier) @attribute
        (#match? @attribute "^[a-z\\d_]*$"))
      (token_tree
        (identifier) @none
        "::"
        (#match? @none "^[a-z\\d_]*$"))
    ]))
