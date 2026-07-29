import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const command = process.argv[2] ?? 'riddle-lsp';
const uri = 'file:///riddle-lsp-smoke.rid';
const stableUri = 'file:///riddle-lsp-stable.rid';
const fixUri = 'file:///riddle-lsp-fix.rid';
const completionUri = 'file:///riddle-lsp-completion.rid';
const generalCompletionUri = 'file:///riddle-lsp-general-completion.rid';
const navigationUri = 'file:///riddle-lsp-navigation.rid';
const navigationText = [
  'trait Show { fun show(&self) -> i32; }',
  'struct Value {}',
  'impl Show for Value { fun show(&self) -> i32 { 1 } }',
  'fun main() { let value = Value {}; value.show(); }',
].join('\n');
const projectRoot = mkdtempSync(join(tmpdir(), 'riddle-lsp-smoke-'));
const projectMainText = 'mod util;\nfun main() { let callable = util::make; callable; }\n';
const projectUtilText = 'pub fun make() -> i32 { 1 }\n';
mkdirSync(join(projectRoot, 'src'), { recursive: true });
writeFileSync(
  join(projectRoot, 'Clue.toml'),
  '[package]\nname = "smoke"\n\n[dependencies]\n',
);
writeFileSync(join(projectRoot, 'src', 'main.rid'), projectMainText);
const projectUtilPath = join(projectRoot, 'src', 'util.rid');
writeFileSync(projectUtilPath, projectUtilText);
const projectMainUri = pathToFileURL(join(projectRoot, 'src', 'main.rid')).href;
const projectUtilUri = pathToFileURL(join(projectRoot, 'src', 'util.rid')).href;
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

function semanticTokenTypeAt(data, targetLine, targetCharacter) {
  let line = 0;
  let character = 0;
  for (let index = 0; index < data.length; index += 5) {
    const deltaLine = data[index];
    line += deltaLine;
    character = deltaLine === 0 ? character + data[index + 1] : data[index + 1];
    if (line === targetLine && character === targetCharacter) return data[index + 3];
  }
  return undefined;
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
        workspace: { didChangeWatchedFiles: { dynamicRegistration: true } },
      },
    },
  });
  const initialized = await read((message) => message.id === 1);
  assert.equal(initialized.result.serverInfo.name, 'riddle-lsp');
  assert.equal(initialized.result.capabilities.positionEncoding, 'utf-16');
  assert.equal(initialized.result.capabilities.textDocumentSync, 2);
  assert.equal(initialized.result.capabilities.codeActionProvider, true);
  assert.equal(initialized.result.capabilities.hoverProvider, true);
  assert.equal(initialized.result.capabilities.definitionProvider, true);
  assert.equal(initialized.result.capabilities.implementationProvider, true);
  const triggerCharacters = initialized.result.capabilities.completionProvider.triggerCharacters;
  assert.deepEqual(triggerCharacters, ['.', ':']);
  assert.equal(initialized.result.capabilities.inlayHintProvider, true);
  assert.equal(initialized.result.capabilities.semanticTokensProvider.full.delta, true);

  send({ jsonrpc: '2.0', method: 'initialized', params: {} });
  const watcherRegistration = await read(
    (message) => message.method === 'client/registerCapability',
    3_000,
  );
  const watchedFiles = watcherRegistration.params.registrations.find(
    (registration) => registration.method === 'workspace/didChangeWatchedFiles',
  );
  assert(watchedFiles);
  assert.deepEqual(
    new Set(watchedFiles.registerOptions.watchers.map((watcher) => watcher.globPattern)),
    new Set(['**/*.rid', '**/Clue.toml']),
  );
  send({ jsonrpc: '2.0', id: watcherRegistration.id, result: null });
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
    id: 21,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri: stableUri } },
  });
  const stableTokens = await read((message) => message.id === 21);

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
    id: 22,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri: stableUri } },
  });
  const stableTokensAfterUnrelatedOpen = await read((message) => message.id === 22);
  assert.equal(stableTokensAfterUnrelatedOpen.result.resultId, stableTokens.result.resultId);

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
  assert.deepEqual(codeActions.result[0].edit.documentChanges[0], {
    textDocument: { uri: fixUri, version: 1 },
    edits: [
      {
        range: { start: mutableClosure.range.start, end: mutableClosure.range.start },
        newText: 'mut ',
      },
    ],
  });

  send({
    jsonrpc: '2.0',
    id: 20,
    method: 'textDocument/codeAction',
    params: {
      textDocument: { uri: fixUri },
      range: mutableClosure.range,
      context: { diagnostics: [mutableClosure], only: ['source.organizeImports'] },
    },
  });
  const filteredCodeActions = await read((message) => message.id === 20);
  assert.deepEqual(filteredCodeActions.result, []);

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
    id: 23,
    method: 'textDocument/codeAction',
    params: {
      textDocument: { uri: fixUri },
      range: mutableClosure.range,
      context: { diagnostics: [mutableClosure], only: ['quickfix'] },
    },
  });
  const staleCodeActions = await read((message) => message.id === 23);
  assert.deepEqual(staleCodeActions.result, []);
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
  const memberCompletionStarted = performance.now();
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
  const memberCompletionMs = performance.now() - memberCompletionStarted;
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

  const generalCompletionText = 'fun Foo() {} fun main() { f }';
  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: generalCompletionUri,
        languageId: 'riddle',
        version: 1,
        text: generalCompletionText,
      },
    },
  });
  await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === generalCompletionUri &&
      message.params.version === 1,
  );
  const generalCompletionStarted = performance.now();
  send({
    jsonrpc: '2.0',
    id: 5,
    method: 'textDocument/completion',
    params: {
      textDocument: { uri: generalCompletionUri },
      position: { line: 0, character: generalCompletionText.lastIndexOf('f') + 1 },
    },
  });
  const generalCompletions = await read((message) => message.id === 5);
  const generalCompletionMs = performance.now() - generalCompletionStarted;
  assert(
    generalCompletions.result.some(
      (item) => item.label === 'Foo' && item.insertText === 'Foo' && item.kind === 3,
    ),
  );

  send({
    jsonrpc: '2.0',
    id: 6,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri } },
  });
  const semanticTokens = await read((message) => message.id === 6);
  assert(semanticTokens.result.data.length > 0);
  assert(semanticTokens.result.resultId);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didChange',
    params: {
      textDocument: { uri, version: 15 },
      contentChanges: [
        {
          range: {
            start: { line: 0, character: 13 },
            end: { line: 0, character: 23 },
          },
          rangeLength: 10,
          text: 'true',
        },
      ],
    },
  });
  const incrementallyFixed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === uri &&
      message.params.version === 15,
  );
  assert.deepEqual(incrementallyFixed.params.diagnostics, []);
  send({
    jsonrpc: '2.0',
    id: 7,
    method: 'textDocument/semanticTokens/full/delta',
    params: {
      textDocument: { uri },
      previousResultId: semanticTokens.result.resultId,
    },
  });
  const semanticDelta = await read((message) => message.id === 7);
  assert(semanticDelta.result.resultId);
  assert(Array.isArray(semanticDelta.result.edits));

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: projectMainUri,
        languageId: 'riddle',
        version: 1,
        text: projectMainText,
      },
    },
  });
  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: projectUtilUri,
        languageId: 'riddle',
        version: 1,
        text: projectUtilText,
      },
    },
  });
  await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === projectMainUri &&
      message.params.version === 1,
  );
  const tokenTypes = initialized.result.capabilities.semanticTokensProvider.legend.tokenTypes;
  const functionTokenType = tokenTypes.indexOf('function');
  const variableTokenType = tokenTypes.indexOf('variable');
  const makeCharacter = projectMainText.split('\n')[1].indexOf('make');
  send({
    jsonrpc: '2.0',
    id: 8,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri: projectMainUri } },
  });
  const projectTokensBefore = await read((message) => message.id === 8);
  assert.equal(
    semanticTokenTypeAt(projectTokensBefore.result.data, 1, makeCharacter),
    functionTokenType,
  );

  send({
    jsonrpc: '2.0',
    id: 24,
    method: 'textDocument/hover',
    params: {
      textDocument: { uri: projectMainUri },
      position: { line: 1, character: makeCharacter + 1 },
    },
  });
  const projectHover = await read((message) => message.id === 24);
  assert.match(projectHover.result.contents.value, /pub fun make\(\) -> i32/);

  send({
    jsonrpc: '2.0',
    id: 25,
    method: 'textDocument/definition',
    params: {
      textDocument: { uri: projectMainUri },
      position: { line: 1, character: makeCharacter + 1 },
    },
  });
  const projectDefinition = await read((message) => message.id === 25);
  assert.equal(projectDefinition.result.uri, projectUtilUri);
  assert.deepEqual(projectDefinition.result.range, {
    start: { line: 0, character: 8 },
    end: { line: 0, character: 12 },
  });

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: {
      textDocument: {
        uri: navigationUri,
        languageId: 'riddle',
        version: 1,
        text: navigationText,
      },
    },
  });
  await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === navigationUri &&
      message.params.version === 1,
  );
  const traitCallCharacter = navigationText.split('\n')[3].indexOf('show');
  send({
    jsonrpc: '2.0',
    id: 26,
    method: 'textDocument/definition',
    params: {
      textDocument: { uri: navigationUri },
      position: { line: 3, character: traitCallCharacter + 1 },
    },
  });
  const traitDefinition = await read((message) => message.id === 26);
  assert.equal(traitDefinition.result.range.start.line, 0);
  assert.equal(traitDefinition.result.range.start.character, navigationText.split('\n')[0].indexOf('show'));

  send({
    jsonrpc: '2.0',
    id: 27,
    method: 'textDocument/implementation',
    params: {
      textDocument: { uri: navigationUri },
      position: { line: 3, character: traitCallCharacter + 1 },
    },
  });
  const traitImplementation = await read((message) => message.id === 27);
  assert.equal(traitImplementation.result.length, 1);
  assert.equal(traitImplementation.result[0].range.start.line, 2);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didChange',
    params: {
      textDocument: { uri: projectUtilUri, version: 2 },
      contentChanges: [{ text: 'pub const make: i32 = 1;\n' }],
    },
  });
  send({
    jsonrpc: '2.0',
    id: 9,
    method: 'textDocument/semanticTokens/full',
    params: { textDocument: { uri: projectMainUri } },
  });
  const projectTokensAfter = await read((message) => message.id === 9);
  assert.equal(
    semanticTokenTypeAt(projectTokensAfter.result.data, 1, makeCharacter),
    variableTokenType,
  );
  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri: projectUtilUri } },
  });

  const fixedTime = new Date('2020-01-01T00:00:00.000Z');
  writeFileSync(projectUtilPath, 'pub fun make() -> i32 { missing_a }\n');
  utimesSync(projectUtilPath, fixedTime, fixedTime);
  send({
    jsonrpc: '2.0',
    method: 'workspace/didChangeWatchedFiles',
    params: { changes: [{ uri: projectUtilUri, type: 2 }] },
  });
  const firstDiskDiagnostics = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === projectUtilUri &&
      message.params.diagnostics.some((diagnostic) => diagnostic.message.includes('missing_a')),
  );
  assert.equal(firstDiskDiagnostics.params.diagnostics[0].code, 'E0050');
  const firstDiskStat = statSync(projectUtilPath);

  writeFileSync(projectUtilPath, 'pub fun make() -> i32 { missing_b }\n');
  utimesSync(projectUtilPath, fixedTime, fixedTime);
  const secondDiskStat = statSync(projectUtilPath);
  assert.equal(secondDiskStat.size, firstDiskStat.size);
  assert.equal(secondDiskStat.mtimeMs, firstDiskStat.mtimeMs);
  send({
    jsonrpc: '2.0',
    method: 'workspace/didChangeWatchedFiles',
    params: { changes: [{ uri: projectUtilUri, type: 2 }] },
  });
  const secondDiskDiagnostics = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === projectUtilUri &&
      message.params.diagnostics.some((diagnostic) => diagnostic.message.includes('missing_b')),
  );
  assert.equal(secondDiskDiagnostics.params.diagnostics[0].code, 'E0050');

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri: projectMainUri } },
  });

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

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri: generalCompletionUri } },
  });
  const generalCompletionClosed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === generalCompletionUri &&
      message.params.version == null,
  );
  assert.deepEqual(generalCompletionClosed.params.diagnostics, []);

  send({
    jsonrpc: '2.0',
    method: 'textDocument/didClose',
    params: { textDocument: { uri: navigationUri } },
  });
  const navigationClosed = await read(
    (message) =>
      message.method === 'textDocument/publishDiagnostics' &&
      message.params.uri === navigationUri &&
      message.params.version == null,
  );
  assert.deepEqual(navigationClosed.params.diagnostics, []);

  send({ jsonrpc: '2.0', id: 10, method: 'shutdown' });
  const shutdown = await read((message) => message.id === 10);
  assert.equal(shutdown.error, undefined);
  assert.equal(shutdown.result, null);
  send({ jsonrpc: '2.0', method: 'exit' });
  console.log(
    `riddle-lsp stdio handshake passed (member ${memberCompletionMs.toFixed(1)} ms, general ${generalCompletionMs.toFixed(1)} ms)`,
  );
} finally {
  server.stdin.end();
  const exited = await Promise.race([
    new Promise((resolve) => server.once('exit', resolve)),
    new Promise((resolve) => setTimeout(resolve, 2_000)),
  ]);
  if (exited === undefined && server.exitCode === null) server.kill();
  rmSync(projectRoot, { recursive: true, force: true });
}
