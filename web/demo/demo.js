// The demo page: Monaco as the editor, perl-lsp (in workers) as the language
// server, and this file as the thin adapter between Monaco's provider
// interfaces and LSP requests. No analysis happens here.
import { startServer } from './runtime/client.js';

const LANGUAGE = 'perl';
const ROOT = 'file:///work';
const status = document.getElementById('status');
const setStatus = (text, kind = '') => { status.textContent = text; status.className = kind; };

// Each scene opens a file, puts the cursor on a token and runs one editor
// action, so the feature shows without the visitor knowing the keybinding.
const SCENES = [
  { title: 'Types across files', note: '$acct is an Account, inferred through make_account() in Bank.pm.', file: 'app.pl', on: '$acct', action: 'editor.action.showHover' },
  { title: 'Go to definition', note: 'Jumps from the call into lib/Bank.pm.', file: 'app.pl', on: 'make_account', action: 'editor.action.revealDefinition' },
  { title: 'Completion from the class', note: 'Moo accessors and methods of Account.', file: 'app.pl', on: 'deposit', action: 'editor.action.triggerSuggest' },
  { title: 'Find references', note: 'Every use of deposit, in both files.', file: 'app.pl', on: 'deposit', action: 'editor.action.goToReferences' },
  { title: 'Rename across files', note: 'Renames the method where it is declared and where it is called.', file: 'lib/Account.pm', on: 'deposit', action: 'editor.action.rename' },
];

const monaco = await new Promise(resolve => {
  window.require.config({ paths: { vs: 'vs' } });
  window.require(['vs/editor/editor.main'], () => resolve(window.monaco));
});

// ---- LSP <-> Monaco shapes -------------------------------------------------

const toPosition = p => ({ line: p.lineNumber - 1, character: p.column - 1 });
const toRange = r => r && new monaco.Range(r.start.line + 1, r.start.character + 1, r.end.line + 1, r.end.character + 1);
const toLocation = l => ({ uri: monaco.Uri.parse(l.uri ?? l.targetUri), range: toRange(l.range ?? l.targetSelectionRange) });
const toMarkdown = c => [c].flat().map(part => ({ value: typeof part === 'string' ? part : part.kind === 'plaintext' || !part.kind && !part.language ? part.value : part.language ? '```' + part.language + '\n' + part.value + '\n```' : part.value }));
const at = (model, position) => ({ textDocument: { uri: model.uri.toString() }, position: toPosition(position) });

// LSP numbers its completion kinds 1..25 in this order; Monaco names them.
const COMPLETION_KINDS = ['Text', 'Method', 'Function', 'Constructor', 'Field', 'Variable', 'Class', 'Interface', 'Module', 'Property', 'Unit', 'Value', 'Enum', 'Keyword', 'Snippet', 'Color', 'File', 'Reference', 'Folder', 'EnumMember', 'Constant', 'Struct', 'Event', 'Operator', 'TypeParameter'];
const toCompletionKind = n => monaco.languages.CompletionItemKind[COMPLETION_KINDS[n - 1]] ?? monaco.languages.CompletionItemKind.Text;
const SEVERITIES = [monaco.MarkerSeverity.Error, monaco.MarkerSeverity.Warning, monaco.MarkerSeverity.Info, monaco.MarkerSeverity.Hint];

function toWorkspaceEdit(edit) {
  const perFile = edit.documentChanges?.map(c => [c.textDocument.uri, c.edits]) ?? Object.entries(edit.changes ?? {});
  return {
    edits: perFile.flatMap(([uri, edits]) => edits.map(e => ({
      resource: monaco.Uri.parse(uri),
      textEdit: { range: toRange(e.range), text: e.newText },
      versionId: undefined,
    }))),
  };
}

// ---- project and server ----------------------------------------------------

const project = await (await fetch('project.json')).json();
function* flatten(tree, prefix = '') {
  for (const [name, value] of Object.entries(tree)) {
    if (typeof value === 'string') yield [prefix + name, value];
    else yield* flatten(value, prefix + name + '/');
  }
}
const models = new Map();
for (const [path, text] of flatten(project)) {
  models.set(path, monaco.editor.createModel(text, LANGUAGE, monaco.Uri.parse(`${ROOT}/${path}`)));
}

const editor = monaco.editor.create(document.getElementById('editor'), {
  model: null,
  automaticLayout: true,
  minimap: { enabled: false },
  fontSize: 14,
  'semanticHighlighting.enabled': true,
  theme: matchMedia('(prefers-color-scheme: dark)').matches ? 'vs-dark' : 'vs',
});

const fileList = document.getElementById('files');
function show(path, selection) {
  const model = models.get(path);
  editor.setModel(model);
  for (const b of fileList.querySelectorAll('button')) b.setAttribute('aria-current', String(b.dataset.path === path));
  if (selection) {
    editor.setSelection(selection);
    editor.revealRangeInCenterIfOutsideViewport(selection);
  }
  editor.focus();
}
for (const path of models.keys()) {
  const li = document.createElement('li'), b = document.createElement('button');
  b.textContent = path;
  b.dataset.path = path;
  b.onclick = () => show(path);
  li.append(b);
  fileList.append(li);
}
// Monaco alone has no notion of opening another file; goto-def and the
// references peek ask this opener.
monaco.editor.registerEditorOpener({
  openCodeEditor(_source, resource, where) {
    const path = resource.toString().slice(ROOT.length + 1);
    if (!models.has(path)) return false;
    show(path, where && (where.startLineNumber ? where : new monaco.Range(where.lineNumber, where.column, where.lineNumber, where.column)));
    return true;
  },
});

let lsp;
try {
  lsp = startServer({ wasm: 'perl-lsp.wasm', shim: 'shim/index.js', files: project });
} catch (e) {
  setStatus(e.message + '. If this page just loaded, it reloads itself once to turn isolation on.', 'bad');
  throw e;
}
lsp.exited.then(why => setStatus(why, 'bad'));

setStatus('Downloading and compiling the server…');
const started = performance.now();
const { capabilities } = await lsp.request('initialize', {
  processId: null,
  rootUri: ROOT,
  workspaceFolders: [{ uri: ROOT, name: 'work' }],
  capabilities: {
    textDocument: {
      hover: { contentFormat: ['markdown', 'plaintext'] },
      completion: { completionItem: { documentationFormat: ['markdown', 'plaintext'] } },
      signatureHelp: { signatureInformation: { documentationFormat: ['markdown', 'plaintext'] } },
      rename: { prepareSupport: true },
    },
  },
});
lsp.notify('initialized', {});
setStatus(`Server ready in ${((performance.now() - started) / 1000).toFixed(1)} s.`, 'ok');

// Every model is an open document: a rename can edit any of them, and the
// server must then be told, or its copy of that file goes stale.
for (const model of models.values()) {
  lsp.notify('textDocument/didOpen', { textDocument: { uri: model.uri.toString(), languageId: LANGUAGE, version: model.getVersionId(), text: model.getValue() } });
  model.onDidChangeContent(() => lsp.notify('textDocument/didChange', {
    textDocument: { uri: model.uri.toString(), version: model.getVersionId() },
    contentChanges: [{ text: model.getValue() }],
  }));
}

lsp.onNotification((method, params) => {
  if (method !== 'textDocument/publishDiagnostics') return;
  const model = monaco.editor.getModel(monaco.Uri.parse(params.uri));
  if (!model) return;
  monaco.editor.setModelMarkers(model, 'perl-lsp', params.diagnostics.map(d => ({
    ...toRange(d.range),
    message: d.message,
    severity: SEVERITIES[(d.severity ?? 1) - 1],
    code: d.code === undefined ? undefined : String(d.code),
    source: d.source,
  })));
});

// ---- providers: one per server capability ----------------------------------

const L = monaco.languages;
const ask = (method, params) => lsp.request(method, params).catch(() => null);

if (capabilities.hoverProvider) L.registerHoverProvider(LANGUAGE, {
  async provideHover(model, position) {
    const r = await ask('textDocument/hover', at(model, position));
    return r && { contents: toMarkdown(r.contents), range: toRange(r.range) };
  },
});

if (capabilities.completionProvider) L.registerCompletionItemProvider(LANGUAGE, {
  triggerCharacters: capabilities.completionProvider.triggerCharacters,
  async provideCompletionItems(model, position) {
    const r = await ask('textDocument/completion', at(model, position));
    const word = model.getWordUntilPosition(position);
    const fallback = new monaco.Range(position.lineNumber, word.startColumn, position.lineNumber, word.endColumn);
    return {
      incomplete: Boolean(r?.isIncomplete),
      suggestions: (r?.items ?? r ?? []).map(item => ({
        label: item.label,
        kind: toCompletionKind(item.kind),
        detail: item.detail,
        documentation: item.documentation && toMarkdown(item.documentation)[0],
        sortText: item.sortText,
        filterText: item.filterText,
        insertText: item.textEdit?.newText ?? item.insertText ?? item.label,
        insertTextRules: item.insertTextFormat === 2 ? L.CompletionItemInsertTextRule.InsertAsSnippet : undefined,
        range: toRange(item.textEdit?.range ?? item.textEdit?.replace) ?? fallback,
        additionalTextEdits: item.additionalTextEdits?.map(e => ({ range: toRange(e.range), text: e.newText })),
      })),
    };
  },
});

if (capabilities.signatureHelpProvider) L.registerSignatureHelpProvider(LANGUAGE, {
  signatureHelpTriggerCharacters: capabilities.signatureHelpProvider.triggerCharacters,
  signatureHelpRetriggerCharacters: capabilities.signatureHelpProvider.retriggerCharacters,
  async provideSignatureHelp(model, position) {
    const r = await ask('textDocument/signatureHelp', at(model, position));
    return r && {
      value: {
        signatures: r.signatures.map(s => ({
          label: s.label,
          documentation: s.documentation && toMarkdown(s.documentation)[0],
          parameters: (s.parameters ?? []).map(p => ({ label: p.label, documentation: p.documentation && toMarkdown(p.documentation)[0] })),
          activeParameter: s.activeParameter,
        })),
        activeSignature: r.activeSignature ?? 0,
        activeParameter: r.activeParameter ?? 0,
      },
      dispose() {},
    };
  },
});

const locations = method => async (model, position, context) => {
  const r = await ask(method, { ...at(model, position), ...(context ? { context } : {}) });
  return [r ?? []].flat().map(toLocation);
};
if (capabilities.definitionProvider) L.registerDefinitionProvider(LANGUAGE, { provideDefinition: locations('textDocument/definition') });
if (capabilities.typeDefinitionProvider) L.registerTypeDefinitionProvider(LANGUAGE, { provideTypeDefinition: locations('textDocument/typeDefinition') });
if (capabilities.referencesProvider) L.registerReferenceProvider(LANGUAGE, {
  provideReferences: (model, position, context) => locations('textDocument/references')(model, position, context),
});

if (capabilities.documentHighlightProvider) L.registerDocumentHighlightProvider(LANGUAGE, {
  async provideDocumentHighlights(model, position) {
    const r = await ask('textDocument/documentHighlight', at(model, position));
    // LSP kinds are 1..3 (text, read, write); Monaco's are 0..2.
    return (r ?? []).map(h => ({ range: toRange(h.range), kind: (h.kind ?? 1) - 1 }));
  },
});

if (capabilities.renameProvider) L.registerRenameProvider(LANGUAGE, {
  async provideRenameEdits(model, position, newName) {
    try {
      const r = await lsp.request('textDocument/rename', { ...at(model, position), newName });
      return r ? toWorkspaceEdit(r) : { edits: [], rejectReason: 'Nothing to rename here.' };
    } catch (e) {
      return { edits: [], rejectReason: e.message };
    }
  },
  async resolveRenameLocation(model, position) {
    const r = await ask('textDocument/prepareRename', at(model, position));
    if (!r) return { rejectReason: 'This cannot be renamed.' };
    const range = toRange(r.range ?? r);
    return { range, text: r.placeholder ?? model.getValueInRange(range) };
  },
});

if (capabilities.inlayHintProvider) L.registerInlayHintsProvider(LANGUAGE, {
  async provideInlayHints(model, range) {
    const r = await ask('textDocument/inlayHint', {
      textDocument: { uri: model.uri.toString() },
      range: { start: toPosition(range.getStartPosition()), end: toPosition(range.getEndPosition()) },
    });
    return {
      hints: (r ?? []).map(h => ({
        position: { lineNumber: h.position.line + 1, column: h.position.character + 1 },
        label: typeof h.label === 'string' ? h.label : h.label.map(part => part.value).join(''),
        kind: h.kind,
        paddingLeft: h.paddingLeft,
        paddingRight: h.paddingRight,
      })),
      dispose() {},
    };
  },
});

if (capabilities.documentSymbolProvider) L.registerDocumentSymbolProvider(LANGUAGE, {
  async provideDocumentSymbols(model) {
    const r = await ask('textDocument/documentSymbol', { textDocument: { uri: model.uri.toString() } });
    // LSP symbol kinds are 1-based, Monaco's the same list 0-based.
    const convert = s => ({
      name: s.name,
      detail: s.detail ?? '',
      kind: s.kind - 1,
      tags: [],
      range: toRange(s.range ?? s.location.range),
      selectionRange: toRange(s.selectionRange ?? s.location.range),
      children: (s.children ?? []).map(convert),
    });
    return (r ?? []).map(convert);
  },
});

const semantic = capabilities.semanticTokensProvider;
if (semantic?.full) L.registerDocumentSemanticTokensProvider(LANGUAGE, {
  getLegend: () => semantic.legend,
  async provideDocumentSemanticTokens(model) {
    const r = await ask('textDocument/semanticTokens/full', { textDocument: { uri: model.uri.toString() } });
    return r && { data: new Uint32Array(r.data), resultId: r.resultId };
  },
  releaseDocumentSemanticTokens() {},
});

// ---- scenes ----------------------------------------------------------------

const sceneList = document.getElementById('scenes');
for (const scene of SCENES) {
  const li = document.createElement('li'), b = document.createElement('button');
  const title = document.createElement('strong'), note = document.createElement('span');
  title.textContent = scene.title;
  note.textContent = scene.note;
  b.append(title, note);
  b.onclick = () => {
    const match = models.get(scene.file).findMatches(scene.on, false, false, true, null, false)[0];
    if (!match) return;
    const start = match.range.getStartPosition();
    // One column in, so the cursor is inside the token whatever its sigil.
    const inside = new monaco.Range(start.lineNumber, start.column + 1, start.lineNumber, start.column + 1);
    show(scene.file, inside);
    editor.trigger('demo', scene.action, null);
  };
  li.append(b);
  sceneList.append(li);
}

show('app.pl');
