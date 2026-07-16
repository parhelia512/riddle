const vscode = require('vscode');
const { LanguageClient } = require('vscode-languageclient/node');

let client;

async function activate(context) {
  const config = vscode.workspace.getConfiguration('riddle');
  const command = config.get('server.path', 'riddle-lsp');
  const args = config.get('server.arguments', []);

  client = new LanguageClient(
    'riddle-lsp',
    'Riddle Language Server',
    { command, args },
    {
      documentSelector: [{ scheme: 'file', language: 'riddle' }],
      synchronize: {
        fileEvents: [
          vscode.workspace.createFileSystemWatcher('**/*.rid'),
          vscode.workspace.createFileSystemWatcher('**/Clue.toml'),
        ],
      },
    },
  );

  context.subscriptions.push(client);
  await client.start();
}

async function deactivate() {
  await client?.stop();
}

module.exports = { activate, deactivate };
