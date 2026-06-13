use la_arena::{Arena, Idx};

use super::Name;

pub type FunctionId = Idx<HirFunction>;
pub type StructId = Idx<HirStruct>;

// there are only has define without body
#[derive(Debug)]
pub struct ItemTree {
    pub functions: Arena<HirFunction>,
    pub structs: Arena<HirStruct>,
    pub top_level: Vec<TopLevelItem>,
}

#[derive(Debug)]
pub enum TopLevelItem {
    Function(FunctionId),
    Struct(StructId),
}

#[derive(Debug, Clone)]
pub struct HirFunction {
    pub name: Name,
    pub params: Vec<HirParam>,
    pub ret_type: Option<HirTypeRef>,
    pub has_body: bool,
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub name: Name,
    pub ty: HirTypeRef,
}

#[derive(Debug, Clone)]
pub struct HirStruct {
    pub name: Name,
    pub fields: Vec<HirStructField>,
}

#[derive(Debug, Clone)]
pub struct HirStructField {
    pub name: Name,
    pub ty: HirTypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HirTypeRef {
    Named(Name),
    Ref(Box<HirTypeRef>),
    Unknown,
    Error,
}
