import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

const command = process.argv[2] ?? 'riddle-lsp';
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
  assert(initialized.result.capabilities.semanticTokensProvider);

  send({ jsonrpc: '2.0', method: 'initialized', params: {} });
  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: 'file:///riddle-lsp-smoke.rid',
        languageId: 'riddle',
        version: 1,
        text: 'fun main() { missing; }',
      },
    },
  });
  const diagnostics = await read(
    (message) => message.method === 'textDocument/publishDiagnostics',
  );
  assert(diagnostics.params.diagnostics.length > 0);

  send({
    jsonrpc: '2.0',
    id: 2,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri: 'file:///riddle-lsp-smoke.rid' } },
  });
  const semanticTokens = await read((message) => message.id === 2);
  assert(semanticTokens.result.data.length > 0);

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
