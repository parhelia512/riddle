use std::{
    fmt::Write as _,
    hash::{Hash, Hasher},
};

use la_arena::{Arena, Idx};
use rowan::TextRange;

use super::Name;

pub type FunctionId = Idx<HirFunction>;
pub type StructId = Idx<HirStruct>;
pub type ModuleId = Idx<HirModule>;
pub type UseId = Idx<HirUse>;
pub type EnumId = Idx<HirEnum>;
pub type TraitId = Idx<HirTrait>;
pub type ImplId = Idx<HirImpl>;
pub type ConstId = Idx<HirConst>;
pub type TypeAliasId = Idx<HirTypeAlias>;

#[derive(Debug)]
pub struct ItemTree {
    pub functions: Arena<HirFunction>,
    pub structs: Arena<HirStruct>,
    pub modules: Arena<HirModule>,
    pub uses: Arena<HirUse>,
    pub enums: Arena<HirEnum>,
    pub traits: Arena<HirTrait>,
    pub impls: Arena<HirImpl>,
    pub consts: Arena<HirConst>,
    pub type_aliases: Arena<HirTypeAlias>,
    pub top_level: Vec<TopLevelItem>,
    /// Functions declared or defined with the `extern "C"` ABI.
    pub extern_function_ids: Vec<FunctionId>,
}

#[derive(Debug, Clone, Copy)]
pub enum TopLevelItem {
    Function(FunctionId),
    Struct(StructId),
    Module(ModuleId),
    Use(UseId),
    Enum(EnumId),
    Trait(TraitId),
    Impl(ImplId),
    Const(ConstId),
    TypeAlias(TypeAliasId),
}

#[derive(Debug, Clone)]
pub enum Visibility {
    Private,
    Public,
}

impl Visibility {
    #[must_use]
    pub const fn is_public(&self) -> bool {
        matches!(self, Self::Public)
    }
}

#[derive(Debug, Clone)]
pub struct HirAttr {
    pub name: Name,
    pub value: Option<String>,
    pub raw: String,
    pub range: TextRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalAttrTarget {
    Trait,
    FundamentalType,
    Other,
}

#[derive(Debug, Clone)]
pub struct HirInternalAttr {
    pub attr: HirAttr,
    pub target: InternalAttrTarget,
}

#[derive(Debug, Clone)]
pub struct HirFunction {
    pub name: Name,
    pub name_range: TextRange,
    pub visibility: Visibility,
    pub is_unsafe: bool,
    pub generics: Vec<Name>,
    pub implicit_generics: Vec<Name>,
    pub const_generics: Vec<Name>,
    pub generic_bounds: Vec<HirGenericBound>,
    pub params: Vec<HirParam>,
    pub ret_type: Option<HirTypeRef>,
    pub ret_type_range: Option<TextRange>,
    pub has_body: bool,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirGenericBound {
    pub param: Name,
    pub target_ty: HirTypeRef,
    pub target_range: TextRange,
    pub trait_ty: HirTypeRef,
    pub trait_range: TextRange,
    pub callable: Option<HirCallableSignature>,
    pub assoc_constraints: Vec<HirAssocTypeConstraint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HirCallableSignature {
    pub params: Vec<HirTypeRef>,
    pub ret: Box<HirTypeRef>,
}

#[derive(Debug, Clone)]
pub struct HirAssocTypeConstraint {
    pub name: Name,
    pub ty: HirTypeRef,
    pub range: TextRange,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub name: Name,
    pub name_range: TextRange,
    pub is_mut: bool,
    pub ty: HirTypeRef,
    pub ty_range: TextRange,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirStruct {
    pub name: Name,
    pub visibility: Visibility,
    pub name_range: TextRange,
    pub generics: Vec<Name>,
    pub const_generics: Vec<Name>,
    pub generic_bounds: Vec<HirGenericBound>,
    pub fields: Vec<HirStructField>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirStructField {
    pub name: Name,
    pub name_range: TextRange,
    pub visibility: Visibility,
    pub ty: HirTypeRef,
    pub ty_range: TextRange,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirEnum {
    pub name: Name,
    pub name_range: TextRange,
    pub visibility: Visibility,
    pub generics: Vec<Name>,
    pub const_generics: Vec<Name>,
    pub generic_bounds: Vec<HirGenericBound>,
    pub variants: Vec<HirEnumVariant>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub name: Name,
    pub name_range: TextRange,
    pub kind: HirVariantKind,
    pub field_ranges: Vec<TextRange>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub enum HirVariantKind {
    /// `Foo`
    Unit,
    /// `Foo(A, B)`
    Tuple(Vec<HirTypeRef>),
    /// `Foo { x: T }`
    Struct(Vec<HirStructField>),
}

#[derive(Debug, Clone)]
pub struct HirTrait {
    pub name: Name,
    pub name_range: TextRange,
    pub visibility: Visibility,
    pub generics: Vec<Name>,
    pub generic_defaults: Vec<Option<HirTypeRef>>,
    pub generic_bounds: Vec<HirGenericBound>,
    pub supertraits: Vec<HirGenericBound>,
    pub methods: Vec<HirFunction>,
    pub default_methods: Vec<FunctionId>,
    pub type_aliases: Vec<HirTypeAlias>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirImpl {
    /// The implementing type's path (`T` in `impl T` / `impl Trait for T`).
    pub self_ty: HirTypeRef,
    pub self_ty_range: TextRange,
    /// The trait being implemented, if any (`Trait` in `impl Trait for T`).
    pub trait_ty: Option<HirTypeRef>,
    pub trait_ty_range: Option<TextRange>,
    pub generics: Vec<Name>,
    pub const_generics: Vec<Name>,
    pub generic_bounds: Vec<HirGenericBound>,
    pub methods: Vec<FunctionId>,
    pub consts: Vec<ConstId>,
    pub type_aliases: Vec<TypeAliasId>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirConst {
    pub name: Name,
    pub name_range: TextRange,
    pub visibility: Visibility,
    pub ty: HirTypeRef,
    pub ty_range: TextRange,
    pub has_value: bool,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirTypeAlias {
    pub name: Name,
    pub name_range: TextRange,
    pub visibility: Visibility,
    pub ty: Option<HirTypeRef>,
    pub ty_range: Option<TextRange>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirModule {
    pub name: Name,
    pub name_range: TextRange,
    pub visibility: Visibility,
    /// `mod foo;` → None; `mod foo { ... }` → Some(items)
    pub items: Option<Vec<TopLevelItem>>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirUse {
    pub tree: HirUseTree,
    pub visibility: Visibility,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirUseTree {
    /// Prefix path, which may be an empty segment (top-level `{a, b}` form).
    pub prefix: HirPath,
    pub kind: HirUseTreeKind,
    pub range: TextRange,
}

#[derive(Debug, Clone)]
pub enum HirUseTreeKind {
    /// `use foo::bar;` / `use foo::bar as baz;`
    Simple { alias: Option<Name> },
    /// `use foo::*;`
    Glob,
    /// `use foo::{a, b as c};`
    List(Vec<HirUseTree>),
}

#[derive(Debug, Clone)]
pub struct HirPath {
    pub anchor: PathAnchor,
    pub segments: Vec<Name>,
    pub segment_type_args: Vec<(usize, Vec<HirTypeRef>)>,
    pub type_args: Vec<HirTypeRef>,
    pub range: TextRange,
}

impl PartialEq for HirPath {
    fn eq(&self, other: &Self) -> bool {
        self.anchor == other.anchor
            && self.segments == other.segments
            && self.segment_type_args == other.segment_type_args
            && self.type_args == other.type_args
    }
}

impl Eq for HirPath {}

impl Hash for HirPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.anchor.hash(state);
        self.segments.hash(state);
        self.segment_type_args.hash(state);
        self.type_args.hash(state);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathAnchor {
    Plain,    // foo::bar
    Crate,    // crate::foo
    Super,    // super::foo
    SelfMod,  // self::foo
    Absolute, // ::foo
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HirTypeRef {
    Named(HirPath),
    Never,
    Ref(Box<Self>, bool), // (inner, mutable)
    /// Raw pointer type: `*const T` or `*mut T`.
    Ptr {
        mutable: bool,
        inner: Box<Self>,
    },
    Tuple(Vec<Self>),
    Slice(Box<Self>),
    Array(Box<Self>, HirConstArg),
    Const(HirConstArg),
    ImplTrait {
        trait_ty: Box<Self>,
        trait_range: TextRange,
        callable: Option<HirCallableSignature>,
        hidden: Option<Name>,
    },
    Unknown,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HirConstArg {
    Value(usize),
    Param(Name),
    Unknown,
    Error,
}

impl HirPath {
    pub fn display(&self) -> String {
        let mut s = String::new();
        match self.anchor {
            PathAnchor::Absolute => s.push_str("::"),
            PathAnchor::Crate => s.push_str("crate"),
            PathAnchor::Super => s.push_str("super"),
            PathAnchor::SelfMod => s.push_str("self"),
            PathAnchor::Plain => {}
        }
        for (i, seg) in self.segments.iter().enumerate() {
            let need_sep = i > 0
                || matches!(
                    self.anchor,
                    PathAnchor::Crate | PathAnchor::Super | PathAnchor::SelfMod
                );
            if need_sep {
                s.push_str("::");
            }
            s.push_str(&seg.0);
            if let Some((_, args)) = self
                .segment_type_args
                .iter()
                .find(|(segment, _)| *segment == i)
            {
                s.push_str("::<");
                s.push_str(
                    &args
                        .iter()
                        .map(HirTypeRef::display)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                s.push('>');
            }
        }
        if !self.type_args.is_empty() {
            let args = self
                .type_args
                .iter()
                .map(HirTypeRef::display)
                .collect::<Vec<_>>()
                .join(", ");
            s.push('<');
            s.push_str(&args);
            s.push('>');
        }
        s
    }

    #[must_use]
    pub fn type_args_for_segment(&self, index: usize) -> &[HirTypeRef] {
        self.segment_type_args
            .iter()
            .find_map(|(segment, args)| (*segment == index).then_some(args.as_slice()))
            .unwrap_or_default()
    }

    /// `crate`, `super`, `self`, and `::xxx` are all considered non-pure simple names.
    #[must_use]
    pub fn as_single_name(&self) -> Option<&Name> {
        if matches!(self.anchor, PathAnchor::Plain) && self.segments.len() == 1 {
            Some(&self.segments[0])
        } else {
            None
        }
    }
}

impl HirTypeRef {
    pub fn display(&self) -> String {
        match self {
            Self::Named(path) => path.display(),
            Self::Never => "!".to_string(),
            Self::Ref(inner, mutable) => {
                let kw = if *mutable { "&mut " } else { "&" };
                format!("{}{}", kw, inner.display())
            }
            Self::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", inner.display())
            }
            Self::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(Self::display)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            }
            Self::Slice(inner) => format!("[{}]", inner.display()),
            Self::Array(inner, len) => format!("[{}; {}]", inner.display(), len.display()),
            Self::Const(value) => value.display(),
            Self::ImplTrait {
                trait_ty, callable, ..
            } => {
                let mut display = format!("impl {}", trait_ty.display());
                if let Some(signature) = callable {
                    let params = signature
                        .params
                        .iter()
                        .map(Self::display)
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(display, "({params}) -> {}", signature.ret.display())
                        .expect("writing to a String cannot fail");
                }
                display
            }
            Self::Unknown => "_".to_string(),
            Self::Error => "<error>".to_string(),
        }
    }
}

impl HirConstArg {
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Value(value) => value.to_string(),
            Self::Param(name) => name.0.clone(),
            Self::Unknown => "_".to_string(),
            Self::Error => "<error>".to_string(),
        }
    }
}
