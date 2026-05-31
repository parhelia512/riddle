pub mod item_tree;
pub mod body;
pub mod lower_items;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Name(pub String);