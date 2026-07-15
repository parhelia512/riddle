# Riddle for VS Code

This extension registers `.rid` files and starts `riddle-lsp` from `PATH`.

```bash
npm install
npx @vscode/vsce package
code --install-extension riddle-0.1.0.vsix
```

Set `riddle.server.path` when `riddle-lsp` is not available on `PATH`.
