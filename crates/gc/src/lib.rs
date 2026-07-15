pub const RUNTIME_C: &str = include_str!("runtime.c");

#[cfg(test)]
#[path = "../../../tests/gc/runtime.rs"]
mod tests;
