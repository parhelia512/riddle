use crate::module::Module;

pub mod c;

/// Trait implemented by every compilation backend.
///
/// Each backend takes an MIR `Module` and produces target-specific output.
pub trait Backend {
    type Error: std::fmt::Debug;

    /// Compile an MIR module into target code (returned as a string).
    ///
    /// # Errors
    ///
    /// Returns a backend-specific error when code generation fails.
    fn compile(&mut self, module: &Module) -> Result<String, Self::Error>;

    /// Human-readable name of the backend (for diagnostics / logging).
    fn name(&self) -> &'static str;
}
