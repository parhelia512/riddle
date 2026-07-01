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
    /// Functions declared in `extern "C"` blocks (no body).
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
    pub fn is_public(&self) -> bool {
        matches!(self, Visibility::Public)
    }
}

#[derive(Debug, Clone)]
pub struct HirAttr {
    pub name: Name,
    pub value: Option<String>,
    pub raw: String,
}

#[derive(Debug, Clone)]
pub struct HirFunction {
    pub name: Name,
    pub visibility: Visibility,
    pub generics: Vec<Name>,
    pub generic_bounds: Vec<HirGenericBound>,
    pub params: Vec<HirParam>,
    pub ret_type: Option<HirTypeRef>,
    pub has_body: bool,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirGenericBound {
    pub param: Name,
    pub trait_ty: HirTypeRef,
    pub assoc_constraints: Vec<HirAssocTypeConstraint>,
}

#[derive(Debug, Clone)]
pub struct HirAssocTypeConstraint {
    pub name: Name,
    pub ty: HirTypeRef,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub name: Name,
    pub ty: HirTypeRef,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirStruct {
    pub name: Name,
    pub visibility: Visibility,
    pub name_range: TextRange,
    pub generics: Vec<Name>,
    pub fields: Vec<HirStructField>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirStructField {
    pub name: Name,
    pub ty: HirTypeRef,
    pub ty_range: TextRange,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirEnum {
    pub name: Name,
    pub visibility: Visibility,
    pub generics: Vec<Name>,
    pub variants: Vec<HirEnumVariant>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub name: Name,
    pub kind: HirVariantKind,
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
    pub visibility: Visibility,
    pub methods: Vec<HirFunction>,
    pub type_aliases: Vec<HirTypeAlias>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirImpl {
    /// The implementing type's path (`T` in `impl T` / `impl Trait for T`).
    pub self_ty: HirTypeRef,
    /// The trait being implemented, if any (`Trait` in `impl Trait for T`).
    pub trait_ty: Option<HirTypeRef>,
    pub generics: Vec<Name>,
    pub generic_bounds: Vec<HirGenericBound>,
    pub methods: Vec<FunctionId>,
    pub consts: Vec<ConstId>,
    pub type_aliases: Vec<TypeAliasId>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirConst {
    pub name: Name,
    pub visibility: Visibility,
    pub ty: HirTypeRef,
    pub has_value: bool,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirTypeAlias {
    pub name: Name,
    pub visibility: Visibility,
    pub ty: Option<HirTypeRef>,
    pub attrs: Vec<HirAttr>,
}

#[derive(Debug, Clone)]
pub struct HirModule {
    pub name: Name,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HirPath {
    pub anchor: PathAnchor,
    pub segments: Vec<Name>,
    pub type_args: Vec<HirTypeRef>,
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
    Ref(Box<HirTypeRef>, bool), // (inner, mutable)
    /// Raw pointer type: `*const T` or `*mut T`.
    Ptr {
        mutable: bool,
        inner: Box<HirTypeRef>,
    },
    Tuple(Vec<HirTypeRef>),
    Array(Box<HirTypeRef>, usize),
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

    /// `crate`, `super`, `self`, and `::xxx` are all considered non-pure simple names.
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
            HirTypeRef::Named(path) => path.display(),
            HirTypeRef::Ref(inner, mutable) => {
                let kw = if *mutable { "&mut " } else { "&" };
                format!("{}{}", kw, inner.display())
            }
            HirTypeRef::Ptr { mutable, inner } => {
                let kind = if *mutable { "*mut" } else { "*const" };
                format!("{kind} {}", inner.display())
            }
            HirTypeRef::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(HirTypeRef::display)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            }
            HirTypeRef::Array(inner, len) => format!("[{}; {}]", inner.display(), len),
            HirTypeRef::Unknown => "_".to_string(),
            HirTypeRef::Error => "<error>".to_string(),
        }
    }
}
