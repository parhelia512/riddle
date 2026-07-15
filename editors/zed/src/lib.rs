use zed_extension_api::{self as zed, Result};

struct RiddleExtension;

impl zed::Extension for RiddleExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary = zed::settings::LspSettings::for_worktree("riddle-lsp", worktree)?.binary;
        let command = binary
            .as_ref()
            .and_then(|settings| settings.path.clone())
            .or_else(|| worktree.which("riddle-lsp"))
            .ok_or("riddle-lsp was not found on PATH")?;
        Ok(zed::Command {
            command,
            args: binary
                .and_then(|settings| settings.arguments)
                .unwrap_or_default(),
            env: Default::default(),
        })
    }
}

zed::register_extension!(RiddleExtension);
