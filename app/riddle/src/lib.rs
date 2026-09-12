//! Riddle tool driver: `riddle fmt`, `riddle run`, and `riddle repl`.
//!
//! The REPL evaluates a journal of statements by recompiling and
//! re-interpreting the whole session each time: definitions accumulate as
//! top-level items, `let` lines accumulate inside a generated `main`, and
//! each expression line is appended as a `println!("{:?}", expr)` tail.
//! Side effects therefore replay on every evaluation; `:reset` starts a
//! fresh session.

pub mod repl;
