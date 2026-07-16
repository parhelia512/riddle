import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

const command = process.argv[2] ?? 'riddle-lsp';
const uri = 'file:///riddle-lsp-smoke.rid';
const stableUri = 'file:///riddle-lsp-stable.rid';
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
    params: { processId: null, rootUri: null, capabilities: {} },
  });
  const initialized = await read((message) => message.id === 1);
  assert.equal(initialized.result.serverInfo.name, 'riddle-lsp');
  assert.equal(initialized.result.capabilities.positionEncoding, 'utf-16');
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

  send({
    jsonrpc: '2.0',
    id: 2,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri } },
  });
  const semanticTokens = await read((message) => message.id === 2);
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

  send({ jsonrpc: '2.0', id: 3, method: 'shutdown', params: null });
  await read((message) => message.id === 3);
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
