use la_arena::{Arena, Idx};

use super::Name;

pub type FunctionId = Idx<Function>;
pub type StructId = Idx<Struct>;

#[derive(Debug)]
pub struct ItemTree {
    pub functions: Arena<Function>,
    pub structs: Arena<Struct>,
    pub top_level: Vec<TopLevelItem>,
}

#[derive(Debug)]
pub enum TopLevelItem {
    Function(FunctionId),
    Struct(StructId),
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: Name,
    pub params: Vec<Param>,
    pub ret_type: Option<TypeRef>,
    pub has_body: bool,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Name,
    pub ty: TypeRef,
}

#[derive(Debug, Clone)]
pub struct Struct {
    pub name: Name,
    pub fields: Vec<StructField>,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: Name,
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    Named(Name),
    Ref(Box<TypeRef>),
    Error,
}
