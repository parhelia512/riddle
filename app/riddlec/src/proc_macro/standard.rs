use super::{document::classify_literal, *};

pub(super) fn expand_standard_derive_macro(
    name: &str,
    item: &SyntaxNode,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, String> {
    let source = if let Some(item) = ast::StructDecl::cast(item.clone()) {
        match name {
            "Debug" => expand_standard_debug_struct(&item)?,
            "Clone" => expand_standard_clone_struct(&item)?,
            "Copy" => expand_standard_marker_struct(&item, "crate::std::marker::Copy")?,
            "Default" => expand_standard_default_struct(&item)?,
            "Hash" => expand_standard_hash_struct(&item)?,
            "PartialEq" => expand_standard_partial_eq_struct(&item)?,
            "Eq" => expand_standard_marker_struct(&item, "crate::std::cmp::Eq")?,
            "PartialOrd" => expand_standard_partial_ord_struct(&item)?,
            "Ord" => expand_standard_ord_struct(&item)?,
            _ => return Err(format!("unknown standard derive macro `{name}`")),
        }
    } else if let Some(item) = ast::EnumDecl::cast(item.clone()) {
        match name {
            "Debug" => expand_standard_debug_enum(&item)?,
            "Clone" => expand_standard_clone_enum(&item)?,
            "Copy" => expand_standard_marker_enum(&item, "crate::std::marker::Copy")?,
            "Default" => expand_standard_default_enum(&item)?,
            "Hash" => expand_standard_hash_enum(&item)?,
            "PartialEq" => expand_standard_partial_eq_enum(&item)?,
            "Eq" => expand_standard_marker_enum(&item, "crate::std::cmp::Eq")?,
            "PartialOrd" => expand_standard_partial_ord_enum(&item)?,
            "Ord" => expand_standard_ord_enum(&item)?,
            _ => return Err(format!("unknown standard derive macro `{name}`")),
        }
    } else {
        return Err(format!("{name} can only be derived for structs and enums"));
    };
    let mut output = ProcMacroTokenStream::from_source(&source, 0)
        .map_err(|message| format!("failed to build {name} implementation: {message}"))?;
    output.set_span(call_site.clone());
    Ok(output)
}

fn expand_standard_marker_struct(
    item: &ast::StructDecl,
    trait_path: &str,
) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "standard derive input is missing a struct name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    Ok(format!(
        "{} {{}}",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            trait_path,
            false,
        )
    ))
}

fn expand_standard_marker_enum(item: &ast::EnumDecl, trait_path: &str) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "standard derive input is missing an enum name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    Ok(format!(
        "{} {{}}",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            trait_path,
            false,
        )
    ))
}

fn expand_standard_default_struct(item: &ast::StructDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Default derive input is missing a struct name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = format!(
        "{} {{ fun default() -> Self {{ {name} {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::default::Default",
            false,
        )
    );
    for field in struct_field_names(item.field_list().as_ref()) {
        let _ = write!(output, "{field}: crate::std::default::Default::default(),");
    }
    output.push_str("} } }");
    Ok(output)
}

fn expand_standard_hash_struct(item: &ast::StructDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Hash derive input is missing a struct name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = format!(
        "{} {{ fun hash(&self) -> usize {{ let mut __riddle_hash = 0usize;",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::hash::Hash",
            false,
        )
    );
    append_hash_steps(
        &mut output,
        struct_field_names(item.field_list().as_ref())
            .into_iter()
            .map(|field| format!("self.{field}")),
    );
    output.push_str("__riddle_hash } }");
    Ok(output)
}

fn expand_standard_partial_ord_struct(item: &ast::StructDecl) -> Result<String, String> {
    expand_standard_ordering_struct(item, true)
}

fn expand_standard_ord_struct(item: &ast::StructDecl) -> Result<String, String> {
    expand_standard_ordering_struct(item, false)
}

fn expand_standard_ordering_struct(
    item: &ast::StructDecl,
    partial: bool,
) -> Result<String, String> {
    let derive = if partial { "PartialOrd" } else { "Ord" };
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| format!("{derive} derive input is missing a struct name"))?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let trait_path = if partial {
        "crate::std::cmp::PartialOrd"
    } else {
        "crate::std::cmp::Ord"
    };
    let method = if partial { "partial_cmp" } else { "cmp" };
    let return_type = if partial {
        "crate::std::option::Option<crate::std::cmp::Ordering>"
    } else {
        "crate::std::cmp::Ordering"
    };
    let mut output = format!(
        "{} {{ fun {method}(&self, other: &Self) -> {return_type} {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            trait_path,
            partial,
        )
    );
    append_ordering_steps(
        &mut output,
        struct_field_names(item.field_list().as_ref())
            .into_iter()
            .map(|field| (format!("self.{field}"), format!("&other.{field}"))),
        partial,
    );
    output.push_str(" } }");
    Ok(output)
}

fn expand_standard_clone_struct(item: &ast::StructDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Clone derive input is missing a struct name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let fields = struct_field_names(item.field_list().as_ref());
    let mut output = format!(
        "{} {{ fun clone(&self) -> Self {{ {name} {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::clone::Clone",
            false,
        )
    );
    for field in fields {
        let _ = write!(output, "{field}: self.{field}.clone(),");
    }
    output.push_str("} } }");
    Ok(output)
}

fn expand_standard_partial_eq_struct(item: &ast::StructDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "PartialEq derive input is missing a struct name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let fields = struct_field_names(item.field_list().as_ref());
    let mut output = format!(
        "{} {{ fun eq(&self, other: &Self) -> bool {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::cmp::PartialEq",
            true,
        )
    );
    for field in fields {
        let _ = write!(
            output,
            "if !self.{field}.eq(&other.{field}) {{ return false; }}"
        );
    }
    output.push_str("true } }");
    Ok(output)
}

fn expand_standard_clone_enum(item: &ast::EnumDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Clone derive input is missing an enum name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = format!(
        "{} {{ fun clone(&self) -> Self {{ match self {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::clone::Clone",
            false,
        )
    );
    for variant in item.variants() {
        let shape = StandardEnumVariant::new(&variant)?;
        shape.append_clone_arm(&mut output, &name);
    }
    output.push_str("} } }");
    Ok(output)
}

fn expand_standard_partial_eq_enum(item: &ast::EnumDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "PartialEq derive input is missing an enum name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = format!(
        "{} {{ fun eq(&self, other: &Self) -> bool {{ match self {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::cmp::PartialEq",
            true,
        )
    );
    for variant in item.variants() {
        let shape = StandardEnumVariant::new(&variant)?;
        shape.append_partial_eq_arm(&mut output, &name);
    }
    output.push_str("} } }");
    Ok(output)
}

fn expand_standard_default_enum(item: &ast::EnumDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Default derive input is missing an enum name".to_string())?;
    let mut defaults = item.variants().filter(|variant| {
        ast::attrs_for_node(variant.syntax()).iter().any(|attr| {
            attr.name()
                .is_some_and(|attribute_name| attribute_name.text() == "default")
        })
    });
    let variant = defaults.next().ok_or_else(|| {
        "Default derive for enums requires exactly one `#[default]` variant".to_string()
    })?;
    if defaults.next().is_some() {
        return Err(
            "Default derive for enums requires exactly one `#[default]` variant".to_string(),
        );
    }
    let shape = StandardEnumVariant::new(&variant)?;
    if shape.has_braces || shape.has_parentheses {
        return Err("the `#[default]` variant must be a unit variant".to_string());
    }
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    Ok(format!(
        "{} {{ fun default() -> Self {{ {name}::{} }} }}",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::default::Default",
            false,
        ),
        shape.name,
    ))
}

fn expand_standard_hash_enum(item: &ast::EnumDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Hash derive input is missing an enum name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = format!(
        "{} {{ fun hash(&self) -> usize {{ match self {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            "crate::std::hash::Hash",
            false,
        )
    );
    for (index, variant) in item.variants().enumerate() {
        let shape = StandardEnumVariant::new(&variant)?;
        let pattern = shape.pattern(&name, Some("__riddle_hash_field"));
        let _ = write!(
            output,
            "{pattern} => {{ let mut __riddle_hash = {}usize;",
            index + 1
        );
        append_hash_steps(
            &mut output,
            (0..shape.field_count()).map(|field| format!("__riddle_hash_field_{field}")),
        );
        output.push_str("__riddle_hash },");
    }
    output.push_str("} } }");
    Ok(output)
}

fn expand_standard_partial_ord_enum(item: &ast::EnumDecl) -> Result<String, String> {
    expand_standard_ordering_enum(item, true)
}

fn expand_standard_ord_enum(item: &ast::EnumDecl) -> Result<String, String> {
    expand_standard_ordering_enum(item, false)
}

fn expand_standard_ordering_enum(item: &ast::EnumDecl, partial: bool) -> Result<String, String> {
    let derive = if partial { "PartialOrd" } else { "Ord" };
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| format!("{derive} derive input is missing an enum name"))?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let trait_path = if partial {
        "crate::std::cmp::PartialOrd"
    } else {
        "crate::std::cmp::Ord"
    };
    let method = if partial { "partial_cmp" } else { "cmp" };
    let return_type = if partial {
        "crate::std::option::Option<crate::std::cmp::Ordering>"
    } else {
        "crate::std::cmp::Ordering"
    };
    let variants = item
        .variants()
        .map(|variant| StandardEnumVariant::new(&variant))
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = format!(
        "{} {{ fun {method}(&self, other: &Self) -> {return_type} {{ match self {{",
        standard_impl_header(
            &name,
            generic_params.as_ref(),
            where_clause.as_ref(),
            trait_path,
            partial,
        )
    );
    for (left_index, left) in variants.iter().enumerate() {
        let left_pattern = left.pattern(&name, Some("__riddle_order_left"));
        let _ = write!(output, "{left_pattern} => match other {{");
        for (right_index, right) in variants.iter().enumerate() {
            let right_pattern = if left_index == right_index {
                right.pattern(&name, Some("__riddle_order_right"))
            } else {
                right.pattern(&name, None)
            };
            let _ = write!(output, "{right_pattern} => ");
            if left_index == right_index {
                output.push('{');
                append_ordering_steps(
                    &mut output,
                    (0..left.field_count()).map(|field| {
                        (
                            format!("__riddle_order_left_{field}"),
                            format!("__riddle_order_right_{field}"),
                        )
                    }),
                    partial,
                );
                output.push_str("},");
            } else {
                output.push_str(ordering_value(left_index < right_index, partial));
                output.push(',');
            }
        }
        output.push_str("},");
    }
    output.push_str("} } }");
    Ok(output)
}

fn struct_field_names(field_list: Option<&ast::StructFieldList>) -> Vec<String> {
    field_list
        .into_iter()
        .flat_map(ast::StructFieldList::fields)
        .filter_map(|field| field.name().map(|name| name.text().to_string()))
        .collect()
}

struct StandardEnumVariant {
    name: String,
    fields: Vec<String>,
    tuple_fields: usize,
    has_braces: bool,
    has_parentheses: bool,
}

impl StandardEnumVariant {
    fn new(variant: &ast::EnumVariant) -> Result<Self, String> {
        Ok(Self {
            name: variant
                .name()
                .map(|name| name.text().to_string())
                .ok_or_else(|| {
                    "standard derive input contains an unnamed enum variant".to_string()
                })?,
            fields: struct_field_names(variant.field_list().as_ref()),
            tuple_fields: variant.tuple_types().count(),
            has_braces: variant.field_list().is_some(),
            has_parentheses: variant
                .syntax()
                .children_with_tokens()
                .any(|element| element.kind() == SyntaxKind::LParen),
        })
    }

    fn append_clone_arm(&self, output: &mut String, type_name: &str) {
        if !self.fields.is_empty() {
            let bindings = self
                .fields
                .iter()
                .enumerate()
                .map(|(index, field)| format!("{field}: __riddle_clone_{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let values = self
                .fields
                .iter()
                .enumerate()
                .map(|(index, field)| format!("{field}: __riddle_clone_{index}.clone()"))
                .collect::<Vec<_>>()
                .join(",");
            let _ = write!(
                output,
                "{type_name}::{} {{{bindings}}} => {type_name}::{} {{{values}}},",
                self.name, self.name
            );
        } else if self.tuple_fields > 0 {
            let bindings = (0..self.tuple_fields)
                .map(|index| format!("__riddle_clone_{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let values = (0..self.tuple_fields)
                .map(|index| format!("__riddle_clone_{index}.clone()"))
                .collect::<Vec<_>>()
                .join(",");
            let _ = write!(
                output,
                "{type_name}::{}({bindings}) => {type_name}::{}({values}),",
                self.name, self.name
            );
        } else {
            let suffix = self.shape_suffix();
            let _ = write!(
                output,
                "{type_name}::{}{suffix} => {type_name}::{}{suffix},",
                self.name, self.name
            );
        }
    }

    fn append_partial_eq_arm(&self, output: &mut String, type_name: &str) {
        let suffix = self.shape_suffix();
        if !self.fields.is_empty() {
            let left = self
                .fields
                .iter()
                .enumerate()
                .map(|(index, field)| format!("{field}: __riddle_eq_left_{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let right = self
                .fields
                .iter()
                .enumerate()
                .map(|(index, field)| format!("{field}: __riddle_eq_right_{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let comparisons =
                equality_expression(self.fields.len(), "__riddle_eq_left", "__riddle_eq_right");
            let _ = write!(
                output,
                "{type_name}::{} {{{left}}} => match other {{ {type_name}::{} {{{right}}} => {comparisons}, _ => false }},",
                self.name, self.name
            );
        } else if self.tuple_fields > 0 {
            let left = (0..self.tuple_fields)
                .map(|index| format!("__riddle_eq_left_{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let right = (0..self.tuple_fields)
                .map(|index| format!("__riddle_eq_right_{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let comparisons =
                equality_expression(self.tuple_fields, "__riddle_eq_left", "__riddle_eq_right");
            let _ = write!(
                output,
                "{type_name}::{}({left}) => match other {{ {type_name}::{}({right}) => {comparisons}, _ => false }},",
                self.name, self.name
            );
        } else {
            let _ = write!(
                output,
                "{type_name}::{}{suffix} => match other {{ {type_name}::{}{suffix} => true, _ => false }},",
                self.name, self.name
            );
        }
    }

    fn shape_suffix(&self) -> &'static str {
        if self.has_braces {
            " {}"
        } else if self.has_parentheses {
            "()"
        } else {
            ""
        }
    }

    fn field_count(&self) -> usize {
        self.fields.len().max(self.tuple_fields)
    }

    fn pattern(&self, type_name: &str, binding_prefix: Option<&str>) -> String {
        if !self.fields.is_empty() {
            let fields = self
                .fields
                .iter()
                .enumerate()
                .map(|(index, field)| {
                    let binding = binding_prefix
                        .map_or_else(|| "_".to_string(), |prefix| format!("{prefix}_{index}"));
                    format!("{field}: {binding}")
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{type_name}::{} {{{fields}}}", self.name)
        } else if self.tuple_fields > 0 {
            let fields = (0..self.tuple_fields)
                .map(|index| {
                    binding_prefix
                        .map_or_else(|| "_".to_string(), |prefix| format!("{prefix}_{index}"))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{type_name}::{}({fields})", self.name)
        } else {
            format!("{type_name}::{}{}", self.name, self.shape_suffix())
        }
    }
}

fn append_hash_steps(output: &mut String, fields: impl IntoIterator<Item = String>) {
    for field in fields {
        let _ = write!(
            output,
            "__riddle_hash = __riddle_hash * 31usize + {field}.hash();"
        );
    }
}

fn append_ordering_steps(
    output: &mut String,
    fields: impl IntoIterator<Item = (String, String)>,
    partial: bool,
) {
    let method = if partial { "partial_cmp" } else { "cmp" };
    for (index, (left, right)) in fields.into_iter().enumerate() {
        let equal = if partial {
            "crate::std::option::Option::Some(crate::std::cmp::Ordering::Equal)"
        } else {
            "crate::std::cmp::Ordering::Equal"
        };
        let _ = write!(
            output,
            "match {left}.{method}({right}) {{ {equal} => {{}}, __riddle_ordering_{index} => {{ return __riddle_ordering_{index}; }}, }}"
        );
    }
    output.push_str(if partial {
        "crate::std::option::Option::Some(crate::std::cmp::Ordering::Equal)"
    } else {
        "crate::std::cmp::Ordering::Equal"
    });
}

fn ordering_value(less: bool, partial: bool) -> &'static str {
    match (less, partial) {
        (true, true) => "crate::std::option::Option::Some(crate::std::cmp::Ordering::Less)",
        (false, true) => "crate::std::option::Option::Some(crate::std::cmp::Ordering::Greater)",
        (true, false) => "crate::std::cmp::Ordering::Less",
        (false, false) => "crate::std::cmp::Ordering::Greater",
    }
}

fn equality_expression(count: usize, left_prefix: &str, right_prefix: &str) -> String {
    if count == 0 {
        return "true".into();
    }
    (0..count)
        .map(|index| format!("{left_prefix}_{index}.eq({right_prefix}_{index})"))
        .collect::<Vec<_>>()
        .join(" && ")
}

pub(super) fn expand_standard_debug_struct(item: &ast::StructDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Debug derive input is missing a struct name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = debug_impl_header(&name, generic_params.as_ref(), where_clause.as_ref());
    let fields = item
        .field_list()
        .map(|fields| {
            fields
                .fields()
                .filter_map(|field| field.name().map(|name| name.text().to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if fields.is_empty() {
        let _ = write!(output, "formatter.write_str({name:?})");
    } else {
        let _ = write!(output, "formatter.write_str({:?})?;", format!("{name} {{ "));
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                output.push_str("formatter.write_str(\", \")?;");
            }
            let _ = write!(output, "formatter.write_str({:?})?;", format!("{field}: "));
            let _ = write!(
                output,
                "crate::std::fmt::write_debug(&self.{field}, &mut *formatter)?;"
            );
        }
        output.push_str("formatter.write_str(\" }\")");
    }
    output.push_str(" } }");
    Ok(output)
}

pub(super) fn expand_standard_debug_enum(item: &ast::EnumDecl) -> Result<String, String> {
    let name = item
        .name()
        .map(|name| name.text().to_string())
        .ok_or_else(|| "Debug derive input is missing an enum name".to_string())?;
    let generic_params = item.generic_params();
    let where_clause = item.where_clause();
    let mut output = debug_impl_header(&name, generic_params.as_ref(), where_clause.as_ref());
    output.push_str("match self {");
    for variant in item.variants() {
        let variant_name = variant
            .name()
            .map(|name| name.text().to_string())
            .ok_or_else(|| "Debug derive input contains an unnamed enum variant".to_string())?;
        let fields = variant
            .field_list()
            .map(|fields| {
                fields
                    .fields()
                    .filter_map(|field| field.name().map(|name| name.text().to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let tuple_fields = variant.tuple_types().count();
        let has_tuple_fields = variant
            .syntax()
            .children_with_tokens()
            .any(|element| element.kind() == SyntaxKind::LParen);

        if !fields.is_empty() {
            let _ = write!(output, "{name}::{variant_name} {{");
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                let _ = write!(output, "{field}:__riddle_debug_{index}");
            }
            let _ = write!(
                output,
                "}}=>{{formatter.write_str({:?})?;",
                format!("{variant_name} {{ ")
            );
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    output.push_str("formatter.write_str(\", \")?;");
                }
                let _ = write!(output, "formatter.write_str({:?})?;", format!("{field}: "));
                let _ = write!(
                    output,
                    "crate::std::fmt::write_debug(__riddle_debug_{index}, &mut *formatter)?;"
                );
            }
            output.push_str("formatter.write_str(\" }\")},");
        } else if has_tuple_fields && tuple_fields > 0 {
            let _ = write!(output, "{name}::{variant_name}(");
            for index in 0..tuple_fields {
                if index > 0 {
                    output.push(',');
                }
                let _ = write!(output, "__riddle_debug_{index}");
            }
            let _ = write!(
                output,
                ")=>{{formatter.write_str({:?})?;",
                format!("{variant_name}(")
            );
            for index in 0..tuple_fields {
                if index > 0 {
                    output.push_str("formatter.write_str(\", \")?;");
                }
                let _ = write!(
                    output,
                    "crate::std::fmt::write_debug(__riddle_debug_{index}, &mut *formatter)?;"
                );
            }
            output.push_str("formatter.write_str(\")\")},");
        } else {
            let braces = variant.field_list().is_some();
            let tuple = has_tuple_fields;
            let _ = write!(output, "{name}::{variant_name}");
            if braces {
                output.push_str(" {}");
            } else if tuple {
                output.push_str("()");
            }
            let _ = write!(output, "=>formatter.write_str({variant_name:?}),");
        }
    }
    output.push_str("} } }");
    Ok(output)
}

pub(super) fn debug_impl_header(
    name: &str,
    generic_params: Option<&ast::GenericParams>,
    where_clause: Option<&ast::WhereClause>,
) -> String {
    format!(
        "{} {{ fun fmt(&self, formatter: &mut crate::std::fmt::Formatter) -> crate::std::fmt::Result {{",
        standard_impl_header(
            name,
            generic_params,
            where_clause,
            "crate::std::fmt::Debug",
            false,
        )
    )
}

fn standard_impl_header(
    name: &str,
    generic_params: Option<&ast::GenericParams>,
    where_clause: Option<&ast::WhereClause>,
    trait_path: &str,
    explicit_self_argument: bool,
) -> String {
    let declaration = generic_params
        .map(|params| params.syntax().text().to_string())
        .unwrap_or_default();
    let params = generic_params
        .map(|params| params.params().collect::<Vec<_>>())
        .unwrap_or_default();
    let type_arguments = if params.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            params
                .iter()
                .map(|param| param.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let derive_bounds = params
        .iter()
        .filter(|param| !param.is_const)
        .map(|param| format!("{}: {trait_path}", param.name))
        .collect::<Vec<_>>();
    let mut where_clause = where_clause
        .map(|clause| clause.syntax().text().to_string())
        .unwrap_or_default();
    if !derive_bounds.is_empty() {
        if where_clause.is_empty() {
            where_clause.push_str("where ");
        } else if where_clause.trim_end().ends_with(',') {
            where_clause.push(' ');
        } else {
            where_clause.push_str(", ");
        }
        where_clause.push_str(&derive_bounds.join(", "));
    }

    let implemented_trait = if explicit_self_argument {
        format!("{trait_path}<{name}{type_arguments}>")
    } else {
        trait_path.into()
    };
    format!("impl{declaration} {implemented_trait} for {name}{type_arguments} {where_clause}")
}

pub(super) fn expand_standard_print_macro(
    name: &str,
    input: &ProcMacroTokenStream,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, (Range<usize>, String)> {
    if input.is_empty() {
        let source = if name == "println" {
            "{ crate::std::io::println(&\"\"); }"
        } else {
            "{}"
        };
        let mut output = ProcMacroTokenStream::from_source(source, 0).map_err(|message| {
            (
                call_site.clone(),
                format!("failed to build print output: {message}"),
            )
        })?;
        output.set_span(call_site.clone());
        return Ok(output);
    }

    let (root, mut source) = standard_format_string_source(input, call_site)?;
    let function = if name == "println" {
        "println"
    } else {
        "print"
    };
    let _ = write!(source, "crate::std::io::{function}(&{root});");
    let source = format!("{{ {source} }}");
    let mut output = ProcMacroTokenStream::from_source(&source, 0).map_err(|message| {
        (
            call_site.clone(),
            format!("failed to build print output: {message}"),
        )
    })?;
    output.set_span(call_site.clone());
    Ok(output)
}

pub(super) fn expand_standard_format_macro(
    input: &ProcMacroTokenStream,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, (Range<usize>, String)> {
    let (root, mut source) = standard_format_string_source(input, call_site)?;
    let _ = write!(source, "{root}");
    let source = format!("{{ {source} }}");

    let mut output = ProcMacroTokenStream::from_source(&source, 0).map_err(|message| {
        (
            call_site.clone(),
            format!("failed to build format output: {message}"),
        )
    })?;
    output.set_span(call_site.clone());
    Ok(output)
}

fn standard_format_string_source(
    input: &ProcMacroTokenStream,
    call_site: &Range<usize>,
) -> Result<(String, String), (Range<usize>, String)> {
    let arguments = split_macro_arguments(input).map_err(|message| (call_site.clone(), message))?;
    if arguments.is_empty() {
        return Err((
            call_site.clone(),
            "format! requires a format string literal".into(),
        ));
    }
    let (format, values) = parse_standard_format_arguments(arguments, call_site)?;
    let root = format!("__riddle_format_{}_{}", call_site.start, call_site.end);
    let mut source = format!("let mut {root} = crate::std::string::String::new();");

    for (index, segment) in format.segments.iter().enumerate() {
        if !segment.is_empty() {
            let _ = write!(source, "{root}.push_str({segment:?});");
        }
        if let Some(value) = values.get(index) {
            let function = match format.arguments[index].trait_kind {
                StandardFormatTrait::Display => "append_display",
                StandardFormatTrait::Debug => "append_debug",
            };
            let _ = write!(
                source,
                "crate::std::fmt::{function}(&mut {root}, &({}));",
                value.to_source(),
            );
        }
    }
    Ok((root, source))
}

fn parse_standard_format_arguments(
    arguments: Vec<ProcMacroTokenStream>,
    call_site: &Range<usize>,
) -> Result<(StandardFormatString, Vec<ProcMacroTokenStream>), (Range<usize>, String)> {
    let format = &arguments[0];
    let format_span = token_stream_span(format).unwrap_or_else(|| call_site.clone());
    let [ProcMacroTokenTree::Literal { text, .. }] = format.trees.as_slice() else {
        return Err((
            format_span,
            "format argument must be a string literal".into(),
        ));
    };
    if classify_literal(text) != Some(SyntaxKind::String) {
        return Err((
            format_span,
            "format argument must be a string literal".into(),
        ));
    }
    let format = parse_format_literal(text).map_err(|message| (format_span.clone(), message))?;
    let values = arguments.into_iter().skip(1).collect::<Vec<_>>();
    let placeholders = format.arguments.len();
    if placeholders != values.len() {
        return Err((
            format_span,
            format!(
                "format string contains {placeholders} placeholder(s), but {} argument(s) were supplied",
                values.len()
            ),
        ));
    }
    Ok((format, values))
}

pub(super) fn expand_standard_quote_macro(
    input: &ProcMacroTokenStream,
    call_site: &Range<usize>,
) -> Result<ProcMacroTokenStream, (Range<usize>, String)> {
    let root = format!("__riddle_quote_{}_{}", call_site.start, call_site.end);
    let mut source = format!("{{ let mut {root} = crate::TokenStream::new();");
    let mut next_id = 0usize;
    append_quote_stream(&mut source, &root, input, &mut next_id, None)
        .map_err(|message| (call_site.clone(), message))?;
    let _ = write!(source, "{root} }}");
    let mut output = ProcMacroTokenStream::from_source(&source, 0).map_err(|message| {
        (
            call_site.clone(),
            format!("failed to build quote output: {message}"),
        )
    })?;
    output.set_span(call_site.clone());
    Ok(output)
}

fn append_quote_stream(
    source: &mut String,
    output: &str,
    input: &ProcMacroTokenStream,
    next_id: &mut usize,
    repeated: Option<(&[String], &str)>,
) -> Result<(), String> {
    let mut literal = ProcMacroTokenStream::default();
    let mut index = 0usize;
    while index < input.trees.len() {
        if matches!(
            input.trees.get(index),
            Some(ProcMacroTokenTree::Punct { value: '#', .. })
        ) && let Some(ProcMacroTokenTree::Group {
            delimiter: ProcMacroDelimiter::Parenthesis,
            stream,
            ..
        }) = input.trees.get(index + 1)
        {
            let (separator, marker) = if matches!(
                input.trees.get(index + 2),
                Some(ProcMacroTokenTree::Punct { value: '*', .. })
            ) {
                (None, index + 2)
            } else if matches!(
                input.trees.get(index + 3),
                Some(ProcMacroTokenTree::Punct { value: '*', .. })
            ) {
                (input.trees.get(index + 2).cloned(), index + 3)
            } else {
                (None, index)
            };
            if marker != index {
                append_quote_literal(source, output, &mut literal);
                let names = quote_repeat_idents(stream);
                let name = names
                    .first()
                    .ok_or_else(|| "quote repetition must contain `#name`".to_string())?;
                let id = *next_id;
                *next_id += 1;
                let repeat_index = format!("__riddle_quote_index_{id}");
                for other in names.iter().skip(1) {
                    let _ = write!(
                        source,
                        "if {name}.len() != {other}.len() {{ crate::std::panic::panic(\"quote repetition variables have different lengths\"); }}"
                    );
                }
                let _ = write!(
                    source,
                    "let mut {repeat_index} = 0usize;while {repeat_index} < {name}.len() {{"
                );
                if let Some(separator) = separator {
                    let separator = ProcMacroTokenStream {
                        trees: vec![separator],
                    }
                    .to_source();
                    let _ = write!(
                        source,
                        "if {repeat_index} > 0usize {{ {output}.extend(crate::TokenStream::from_str({separator:?}).unwrap_or(crate::TokenStream::new())); }}"
                    );
                }
                append_quote_stream(
                    source,
                    output,
                    stream,
                    next_id,
                    Some((names.as_slice(), repeat_index.as_str())),
                )?;
                let _ = write!(source, "{repeat_index} += 1usize;");
                source.push('}');
                index = marker + 1;
                continue;
            }
        }

        if matches!(
            input.trees.get(index),
            Some(ProcMacroTokenTree::Punct { value: '#', .. })
        ) && let Some(ProcMacroTokenTree::Ident { text, .. }) = input.trees.get(index + 1)
        {
            append_quote_literal(source, output, &mut literal);
            let value = repeated
                .filter(|(names, _)| names.contains(text))
                .map_or_else(
                    || text.clone(),
                    |(_, index)| format!("&{text}.as_slice()[{index}]"),
                );
            let _ = write!(source, "{value}.to_tokens(&mut {output});");
            index += 2;
            continue;
        }

        match &input.trees[index] {
            ProcMacroTokenTree::Group {
                delimiter, stream, ..
            } => {
                append_quote_literal(source, output, &mut literal);
                let id = *next_id;
                *next_id += 1;
                let inner = format!("__riddle_quote_inner_{id}");
                let group = format!("__riddle_quote_group_{id}");
                let _ = write!(source, "let mut {inner} = crate::TokenStream::new();");
                append_quote_stream(source, &inner, stream, next_id, repeated)?;
                let delimiter = match delimiter {
                    ProcMacroDelimiter::Parenthesis => "Parenthesis",
                    ProcMacroDelimiter::Brace => "Brace",
                    ProcMacroDelimiter::Bracket => "Bracket",
                    ProcMacroDelimiter::None => "None",
                };
                let _ = write!(
                    source,
                    "let {group} = crate::Group::new(crate::Delimiter::{delimiter}, {inner}.cloned());{group}.to_tokens(&mut {output});"
                );
            }
            tree => literal.trees.push(tree.clone()),
        }
        index += 1;
    }
    append_quote_literal(source, output, &mut literal);
    Ok(())
}

fn quote_repeat_idents(input: &ProcMacroTokenStream) -> Vec<String> {
    let mut names = Vec::new();
    collect_quote_repeat_idents(input, &mut names);
    names
}

fn collect_quote_repeat_idents(input: &ProcMacroTokenStream, names: &mut Vec<String>) {
    for trees in input.trees.windows(2) {
        if let [
            ProcMacroTokenTree::Punct { value: '#', .. },
            ProcMacroTokenTree::Ident { text, .. },
        ] = trees
            && !names.contains(text)
        {
            names.push(text.clone());
        }
    }
    for tree in &input.trees {
        if let ProcMacroTokenTree::Group { stream, .. } = tree {
            collect_quote_repeat_idents(stream, names);
        }
    }
}

fn append_quote_literal(source: &mut String, output: &str, literal: &mut ProcMacroTokenStream) {
    if literal.is_empty() {
        return;
    }
    let text = std::mem::take(literal).to_source();
    let _ = write!(
        source,
        "{output}.extend(crate::TokenStream::from_str({text:?}).unwrap_or(crate::TokenStream::new()));"
    );
}

pub(super) fn split_macro_arguments(
    input: &ProcMacroTokenStream,
) -> Result<Vec<ProcMacroTokenStream>, String> {
    let mut arguments = Vec::new();
    let mut current = ProcMacroTokenStream::default();
    for tree in &input.trees {
        if matches!(tree, ProcMacroTokenTree::Punct { value: ',', .. }) {
            if current.is_empty() {
                return Err("expected an argument before `,`".into());
            }
            arguments.push(current);
            current = ProcMacroTokenStream::default();
        } else {
            current.trees.push(tree.clone());
        }
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    Ok(arguments)
}

#[derive(Clone, Copy)]
enum StandardFormatTrait {
    Display,
    Debug,
}

struct StandardFormatString {
    segments: Vec<String>,
    arguments: Vec<StandardFormatArgument>,
}

struct StandardFormatArgument {
    trait_kind: StandardFormatTrait,
}

struct DecodedLiteralChar {
    value: char,
}

fn parse_format_literal(text: &str) -> Result<StandardFormatString, String> {
    let chars = decode_string_literal(text).ok_or("invalid format string literal")?;
    let mut segments = vec![String::new()];
    let mut arguments = Vec::new();
    let mut index = 0;
    while let Some(character) = chars.get(index) {
        let next = |offset: usize| chars.get(index + offset).map(|character| character.value);
        match character {
            DecodedLiteralChar { value: '{', .. } if next(1) == Some('{') => {
                segments.last_mut().unwrap().push('{');
                index += 2;
            }
            DecodedLiteralChar { value: '{', .. } if next(1) == Some('}') => {
                arguments.push(StandardFormatArgument {
                    trait_kind: StandardFormatTrait::Display,
                });
                segments.push(String::new());
                index += 2;
            }
            DecodedLiteralChar { value: '{', .. } if next(1) == Some(':') => {
                if next(2) != Some('?') || next(3) != Some('}') {
                    return Err(unsupported_format_placeholder_message());
                }
                arguments.push(StandardFormatArgument {
                    trait_kind: StandardFormatTrait::Debug,
                });
                segments.push(String::new());
                index += 4;
            }
            DecodedLiteralChar { value: '{', .. } => {
                return Err(unsupported_format_placeholder_message());
            }
            DecodedLiteralChar { value: '}', .. } if next(1) == Some('}') => {
                segments.last_mut().unwrap().push('}');
                index += 2;
            }
            DecodedLiteralChar { value: '}', .. } => {
                let close = '}';
                return Err(format!("unmatched `{close}` in format string"));
            }
            character => {
                segments.last_mut().unwrap().push(character.value);
                index += 1;
            }
        }
    }
    Ok(StandardFormatString {
        segments,
        arguments,
    })
}

fn unsupported_format_placeholder_message() -> String {
    let open = '{';
    let close = '}';
    format!("only `{open}{close}` and `{open}:?{close}` format placeholders are supported")
}

fn decode_string_literal(text: &str) -> Option<Vec<DecodedLiteralChar>> {
    let (body, raw) = raw_string_body_range(text)
        .map(|body| (body, true))
        .or_else(|| {
            (text.starts_with('"') && text.ends_with('"') && text.len() >= 2)
                .then_some((1..text.len() - 1, false))
        })?;
    let mut output = Vec::new();
    let mut chars = text[body.clone()].char_indices();
    while let Some((_, character)) = chars.next() {
        if raw || character != '\\' {
            output.push(DecodedLiteralChar { value: character });
            continue;
        }
        let value = match chars.next() {
            Some((_, 'n')) => '\n',
            Some((_, 'r')) => '\r',
            Some((_, 't')) => '\t',
            Some((_, '0')) => '\0',
            Some((_, '\\')) => '\\',
            Some((_, '\'')) => '\'',
            Some((_, '"')) => '"',
            Some((_, character)) => character,
            None => '\\',
        };
        output.push(DecodedLiteralChar { value });
    }
    Some(output)
}

pub(super) fn raw_string_body_range(text: &str) -> Option<Range<usize>> {
    let rest = text.strip_prefix('r')?;
    let hashes = rest.bytes().take_while(|&byte| byte == b'#').count();
    let opening_quote = 1 + hashes;
    if text.as_bytes().get(opening_quote) != Some(&b'"') {
        return None;
    }
    let suffix_start = text.len().checked_sub(1 + hashes)?;
    if suffix_start <= opening_quote || text.as_bytes().get(suffix_start) != Some(&b'"') {
        return None;
    }
    text.as_bytes()[suffix_start + 1..]
        .iter()
        .all(|&byte| byte == b'#')
        .then_some(opening_quote + 1..suffix_start)
}

pub(super) fn token_stream_span(stream: &ProcMacroTokenStream) -> Option<Range<usize>> {
    Some(stream.trees.first()?.span().start..stream.trees.last()?.span().end)
}
