import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

const command = process.argv[2] ?? 'riddle-lsp';
const uri = 'file:///riddle-lsp-smoke.rid';
const stableUri = 'file:///riddle-lsp-stable.rid';
const fixUri = 'file:///riddle-lsp-fix.rid';
const completionUri = 'file:///riddle-lsp-completion.rid';
const server = spawn(command, [], { stdio: ['pipe', 'pipe', 'inherit'] });
let input = Buffer.alloc(0);
const messages = [];
const waiters = [];

function send(message) {
  const body = JSON.stringify(message);
  server.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

function dispatch(message) {
  const index = waiters.findIndex(({ predicate }) => predicate(message));
  if (index === -1) {
    messages.push(message);
    return;
  }
  const [{ resolve, timer }] = waiters.splice(index, 1);
  clearTimeout(timer);
  resolve(message);
}

function read(predicate, timeout = 15_000) {
  const index = messages.findIndex(predicate);
  if (index !== -1) {
    return Promise.resolve(messages.splice(index, 1)[0]);
  }
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('timed out waiting for LSP message')), timeout);
    waiters.push({ predicate, resolve, timer });
  });
}

server.stdout.on('data', (chunk) => {
  input = Buffer.concat([input, chunk]);
  while (true) {
    const headerEnd = input.indexOf('\r\n\r\n');
    if (headerEnd === -1) return;
    const header = input.subarray(0, headerEnd).toString('ascii');
    const length = Number(/^Content-Length:\s*(\d+)$/im.exec(header)?.[1]);
    assert(Number.isInteger(length), `invalid LSP header: ${header}`);
    const bodyStart = headerEnd + 4;
    if (input.length < bodyStart + length) return;
    const body = input.subarray(bodyStart, bodyStart + length).toString('utf8');
    input = input.subarray(bodyStart + length);
    dispatch(JSON.parse(body));
  }
});

try {
  send({
    jsonrpc: '2.0',
    id: 1,
    method: 'initialize',
    params: {
      processId: null,
      rootUri: null,
      capabilities: {
        textDocument: { completion: { completionItem: { labelDetailsSupport: true } } },
      },
    },
  });
  const initialized = await read((message) => message.id === 1);
  assert.equal(initialized.result.serverInfo.name, 'riddle-lsp');
  assert.equal(initialized.result.capabilities.positionEncoding, 'utf-16');
  assert.equal(initialized.result.capabilities.codeActionProvider, true);
  assert.deepEqual(initialized.result.capabilities.completionProvider.triggerCharacters, ['.', ':']);
  assert.equal(initialized.result.capabilities.inlayHintProvider, true);
  assert(initialized.result.capabilities.semanticTokensProvider);

  send({ jsonrpc: '2.0', method: 'initialized', params: {} });
  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri,
        languageId: 'riddle',
        version: 1,
        text: 'fun main() { missing; }',
      },
    },
  });
  const diagnostics = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === uri &&
      message.params.version === 1,
  );
  assert.equal(diagnostics.params.diagnostics.length, 1);
  const [unresolved] = diagnostics.params.diagnostics;
  assert.equal(unresolved.code, 'E0050');
  assert.equal(unresolved.source, 'riddle');
  assert.equal(unresolved.severity, 1);
  assert.equal(unresolved.message, 'unresolved name: `missing`');
  assert.deepEqual(unresolved.range, {
    start: { line: 0, character: 13 },
    end: { line: 0, character: 20 },
  });

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didChange',
    params: {
      textDocument: { uri, version: 2 },
      contentChanges: [{ text: 'fun main() {}' }],
    },
  });
  const fixed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === uri &&
      message.params.version === 2,
  );
  assert.deepEqual(fixed.params.diagnostics, []);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: stableUri,
        languageId: 'riddle',
        version: 1,
        text: 'fun stable() { stable_missing; }',
      },
    },
  });
  const stable = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === stableUri &&
      message.params.version === 1,
  );
  assert.equal(stable.params.diagnostics[0].code, 'E0050');

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: fixUri,
        languageId: 'riddle',
        version: 1,
        text: 'fun main() { let mut total = 0; let add = fun() { total += 1; }; add(); }',
      },
    },
  });
  const fixDiagnostics = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === fixUri &&
      message.params.version === 1,
  );
  const mutableClosure = fixDiagnostics.params.diagnostics.find(
    (diagnostic) => diagnostic.code === 'E0031',
  );
  assert(mutableClosure);
  assert.equal(mutableClosure.relatedInformation[0].message, 'mutable closure called here');

  send({
    jsonrpc: '2.0',
    id: 2,
    method: 'textDocument/codeAction',
    params: {
      textDocument: { uri: fixUri },
      range: mutableClosure.range,
      context: { diagnostics: [mutableClosure], only: ['quickfix'] },
    },
  });
  const codeActions = await read((message) => message.id === 2);
  assert.equal(codeActions.result.length, 1);
  assert.equal(codeActions.result[0].kind, 'quickfix');
  assert.equal(codeActions.result[0].isPreferred, true);
  assert.deepEqual(codeActions.result[0].edit.changes[fixUri][0], {
    range: { start: mutableClosure.range.start, end: mutableClosure.range.start },
    newText: 'mut ',
  });

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didChange',
    params: {
      textDocument: { uri: fixUri, version: 2 },
      contentChanges: [
        { text: 'struct Foo{}\n\nfun main(){\n    let a = Foo{};\n    let b = a;\n    let c = a;\n}' },
      ],
    },
  });
  await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === fixUri &&
      message.params.version === 2 &&
      message.params.diagnostics.some((diagnostic) => diagnostic.code === 'E0100'),
  );
  send({
    jsonrpc: '2.0',
    id: 3,
    method: 'textDocument/inlayHint',
    params: {
      textDocument: { uri: fixUri },
      range: { start: { line: 0, character: 0 }, end: { line: 6, character: 1 } },
    },
  });
  const inlayHints = await read((message) => message.id === 3);
  assert.equal(inlayHints.result.length, 2);
  assert.equal(inlayHints.result.filter((hint) => hint.label === ': Foo').length, 2);

  const lastBurstVersion = 14;
  for (let version = 3; version <= lastBurstVersion; version += 1) {
    send({
      jsonrpc: '2.0',
      method: 'textDocument/didChange',
      params: {
        textDocument: { uri, version },
        contentChanges: [{ text: `fun main() { missing_${version}; }` }],
      },
    });
  }
  const latest = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === uri &&
      message.params.version === lastBurstVersion,
  );
  assert.equal(latest.params.diagnostics[0].code, 'E0050');
  assert.equal(
    messages.some(
      (message) =>
        message.method === 'textDocument/publishDiagnostics' &&
        message.params.uri === uri &&
        message.params.version >= 3 &&
        message.params.version < lastBurstVersion,
    ),
    false,
    'stale diagnostics were published during a change burst',
  );
  assert.equal(
    messages.some(
      (message) =>
        message.method === 'textDocument/publishDiagnostics' &&
        message.params.uri === stableUri,
    ),
    false,
    'unchanged diagnostics were published again',
  );

  const completionText = 'fun main() { let c = String::new(); let d = c.i }';
  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: completionUri,
        languageId: 'riddle',
        version: 1,
        text: completionText,
      },
    },
  });
  await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === completionUri &&
      message.params.version === 1,
  );
  send({
    jsonrpc: '2.0',
    id: 4,
    method: 'textDocument/completion',
    params: {
      textDocument: { uri: completionUri },
      position: { line: 0, character: completionText.indexOf('c.i') + 3 },
    },
  });
  const completions = await read((message) => message.id === 4);
  assert(
    completions.result.some(
      (item) =>
        item.label === 'is_empty' &&
        item.labelDetails.detail === '(&self)' &&
        item.labelDetails.description === 'bool' &&
        item.insertText === 'is_empty' &&
        item.kind === 2,
    ),
  );

  send({
    jsonrpc: '2.0',
    id: 5,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri } },
  });
  const semanticTokens = await read((message) => message.id === 5);
  assert(semanticTokens.result.data.length > 0);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri } },
  });
  const closed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === uri &&
      message.params.version == null,
  );
  assert.deepEqual(closed.params.diagnostics, []);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri: stableUri } },
  });
  const stableClosed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === stableUri &&
      message.params.version == null,
  );
  assert.deepEqual(stableClosed.params.diagnostics, []);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri: fixUri } },
  });
  const fixClosed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === fixUri &&
      message.params.version == null,
  );
  assert.deepEqual(fixClosed.params.diagnostics, []);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri: completionUri } },
  });
  const completionClosed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === completionUri &&
      message.params.version == null,
  );
  assert.deepEqual(completionClosed.params.diagnostics, []);

  send({ jsonrpc: '2.0', id: 6, method: 'shutdown', params: null });
  await read((message) => message.id === 6);
  send({ jsonrpc: '2.0', method: 'exit' });
  console.log('riddle-lsp stdio handshake passed');
} finally {
  server.stdin.end();
  const exited = await Promise.race([
    new Promise((resolve) => server.once('exit', resolve)),
    new Promise((resolve) => setTimeout(resolve, 2_000)),
  ]);
  if (exited === undefined && server.exitCode === null) server.kill();
}
