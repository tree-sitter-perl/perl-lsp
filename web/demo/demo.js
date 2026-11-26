// The demo page: Monaco as the editor, perl-lsp (in workers) as the language
// server, and this file as the thin adapter between Monaco's provider
// interfaces and LSP requests. No analysis happens here.
import { runCommand, startServer } from './runtime/client.js';

const LANGUAGE = 'perl';
const ROOT = 'file:///work';
const status = document.getElementById('status');
const setStatus = (text, kind = '') => { status.textContent = text; status.className = kind; };

// A scene opens a file, puts the cursor on a token and runs one editor
// action (or asks the server and lists the answer), so each feature shows
// without the visitor knowing a keybinding. `on` is text in the file; `nth`
// picks a later occurrence; `offset` moves the cursor into the token.
const HOVER = 'editor.action.showHover';
const DEFINITION = 'editor.action.revealDefinition';
const SCENES = [
  ['Types, with no annotations', [
    { id: 'infer', title: 'A constructor call', note: '$cart is a Shop::Cart.', file: 'app.pl', on: '$cart = ', action: HOVER },
    { id: 'chain', title: 'A chain keeps its type', note: 'add() returns the cart, so apply() is still a Shop::Cart method.', file: 'app.pl', on: '->apply(', offset: 2, action: HOVER },
    { id: 'maybe', title: 'A value that may be undef', note: 'parse() can return nothing: $parsed is Maybe<Shop::Item>, and calling a method on it is flagged.', file: 'app.pl', on: '$parsed->name', action: HOVER },
    { id: 'narrow', title: 'A guard narrows the type', note: 'After `if defined`, the same variable is a Shop::Item.', file: 'app.pl', on: '$parsed->name', nth: 1, action: HOVER },
    { id: 'ref', title: 'ref() checks narrow too', note: 'Inside the ARRAY branch $input is an ArrayRef.', file: 'lib/Shop/Util.pm', on: '@$input', offset: 1, action: HOVER },
    { id: 'inlay', title: 'Inlay hints', note: 'Return types and variable types appear inline, greyed.', file: 'lib/Shop/Cart.pm', on: 'sub totals' },
  ]],
  ['Control flow', [
    { id: 'flow-return', title: 'An early return', note: 'After `return ... unless defined $item`, $item is a Shop::Item for the rest of the sub.', file: 'flow.pl', on: '$item->label', action: HOVER },
    { id: 'flow-noreturn', title: 'A call that never returns', note: 'fail() always dies, so the line after it only runs with an item.', file: 'flow.pl', on: '$item->name', action: HOVER },
    { id: 'flow-ordie', title: '// die', note: 'The undef case never gets past the assignment.', file: 'flow.pl', on: '$item = Shop::Item->parse($line) //', action: HOVER },
    { id: 'flow-loop', title: 'next in a loop', note: 'The rest of the loop body only sees real items.', file: 'flow.pl', on: '$cart->add( $item', offset: 12, action: HOVER },
    { id: 'flow-reassign', title: 'Assigning again', note: 'The guard was about the old value: the second parse may be undef again, and the call is flagged.', file: 'flow.pl', on: '$item->price', action: HOVER },
    { id: 'flow-redundant', title: 'A guard that cannot fail', note: 'new() always returns a cart, so `defined $cart` is flagged as redundant.', file: 'flow.pl', on: 'defined $cart', offset: 9, action: HOVER },
    { id: 'flow-closure', title: 'A closure runs later', note: '$item is assigned again after the closure is made, so inside it the guard is not taken as proof.', file: 'flow.pl', on: '{ $item->name }', offset: 3, action: HOVER },
    { id: 'flow-try', title: 'try / catch', note: 'Assigned in try, untouched in catch: the two ways out are joined.', file: 'flow.pl', on: 'return $item;', nth: 1, offset: 8, action: HOVER },
    { id: 'flow-deref', title: 'The wrong kind of reference', note: 'ref() proved an array; a hash deref on the next line would die at runtime.', file: 'flow.pl', on: '$input->{count}', offset: 9, action: HOVER },
  ]],
  ['Unions, shapes and named arguments', [
    { id: 'union', title: 'One of two classes', note: 'pick_discount() returns a Percent on one path and a Fixed on the other.', file: 'shapes.pl', on: '$discount = ', action: HOVER },
    { id: 'union-arms', title: 'A call on a union', note: 'describe() is answered for both classes: Percent overrides it, Fixed inherits it.', file: 'shapes.pl', on: '$discount->describe', offset: 12, action: HOVER },
    { id: 'union-def', title: 'Both definitions', note: 'Go to definition offers the two describe() subs the call can reach.', file: 'shapes.pl', on: '$discount->describe', offset: 12, action: DEFINITION },
    { id: 'union-warn', title: 'A method only one arm has', note: 'rate() exists on Percent but not on Fixed, and the warning says which.', file: 'shapes.pl', on: '$discount->rate', offset: 12, action: HOVER },
    { id: 'shape', title: 'A hash that grows', note: 'host and paths were added by later statements; all three keys complete.', file: 'shapes.pl', on: '$settings->{port}', offset: 12, action: 'editor.action.triggerSuggest' },
    { id: 'named-complete', title: 'Named argument completion', note: 'new() reads carrier, express and insured from %args; the two not yet passed are offered.', file: 'shapes.pl', on: "carrier => 'dhl', ", offset: 18, action: 'editor.action.triggerSuggest' },
    { id: 'named-def', title: 'A named argument to its reader', note: 'weight => jumps to $opt{weight} inside quote().', file: 'shapes.pl', on: 'weight => 2', action: DEFINITION },
    { id: 'named-rename', title: 'Rename a named argument', note: 'Renaming carrier here renames the key new() reads.', file: 'shapes.pl', on: "carrier => 'dhl'", action: 'editor.action.rename' },
    { id: 'tuple', title: 'A list return', note: 'quote() returns (price, route); each variable gets its own type.', file: 'shapes.pl', on: '$price,', action: HOVER },
    { id: 'tuple-sub', title: 'The list as a type', note: 'The sub itself returns a sequence of a number and a string.', file: 'shapes.pl', on: '->quote', offset: 3, action: HOVER },
  ]],
  ['Navigation', [
    { id: 'def', title: 'Go to definition, across files', note: 'From the call into lib/Shop/Util.pm.', file: 'app.pl', on: 'format_price(', action: DEFINITION },
    { id: 'role', title: 'A method from a role', note: 'describe() is not in Shop::Item; it comes from the role it consumes.', file: 'app.pl', on: '$book->describe', offset: 7, action: DEFINITION },
    { id: 'super', title: 'SUPER::', note: 'Resolves to the parent class.', file: 'lib/Shop/Discount/Percent.pm', on: 'SUPER::describe', offset: 7, action: DEFINITION },
    { id: 'ctor', title: 'A constructor argument', note: 'currency => jumps to where new() reads that key.', file: 'app.pl', on: "currency => 'EUR'", action: DEFINITION },
    { id: 'key', title: 'A hash key from a return value', note: '{net} jumps to the key in the hash split_tax() returns.', file: 'app.pl', on: '$parts->{net}', offset: 9, action: DEFINITION },
    { id: 'dynamic', title: 'A method name built at runtime', note: '"add_$kind" is folded: the call resolves to add_shipping.', file: 'app.pl', on: '$cart->$method', offset: 8, action: HOVER },
    { id: 'refs', title: 'Find references', note: 'label is required by the role, defined in the class, and called.', file: 'lib/Shop/Item.pm', on: 'sub label', offset: 4, action: 'editor.action.goToReferences' },
    { id: 'impl', title: 'Find implementations', note: 'Every subclass that overrides amount_off.', file: 'lib/Shop/Discount.pm', on: 'sub amount_off', offset: 4, action: 'editor.action.goToImplementation' },
    { id: 'outline', title: 'Outline', note: 'Packages, subs and Moo attributes of the file.', file: 'lib/Shop/Item.pm', on: 'has name', action: 'editor.action.quickOutline' },
  ]],
  ['Editing', [
    { id: 'complete', title: 'Method completion', note: "The cart's methods, with their return types.", file: 'app.pl', on: '$cart->totals', offset: 7, action: 'editor.action.triggerSuggest' },
    { id: 'keys', title: 'Hash key completion', note: 'The keys of the hash split_tax() returns.', file: 'app.pl', on: '$parts->{net}', offset: 9, action: 'editor.action.triggerSuggest' },
    { id: 'signature', title: 'Signature help', note: 'Parameter names, read from the sub body.', file: 'app.pl', on: 'format_price( $totals', offset: 13, action: 'editor.action.triggerParameterHints' },
    { id: 'rename', title: 'Rename across files', note: 'Renames add where it is defined and in both chained calls.', file: 'lib/Shop/Cart.pm', on: 'sub add ', offset: 4, action: 'editor.action.rename' },
    { id: 'docs', title: 'POD on hover', note: 'Documentation written next to the sub.', file: 'app.pl', on: '$cart->add(', offset: 7, action: HOVER },
  ]],
  ['Diagnostics', [
    { id: 'typo', title: 'A misspelled method', note: 'totls is not a Shop::Cart method.', file: 'app.pl', on: '# A typo', action: 'editor.action.marker.next' },
    { id: 'import', title: 'A missing import, with its fix', note: 'round_cents exists in Shop::Util; the quick fix adds it to the use line.', file: 'app.pl', on: 'round_cents(3', action: 'editor.action.quickFix' },
  ]],
  ['Frameworks, from their real source', [
    { id: 'mojo-helper', title: 'A Mojolicious helper', note: 'current_cart is declared in startup(); here it is a method, and it returns a Shop::Cart.', file: 'lib/Shop/Web/Controller/Cart.pm', on: '$self->current_cart', offset: 8, action: HOVER },
    { id: 'mojo-helper-def', title: 'Where the helper is declared', note: 'Jumps to the helper(...) call in the application class.', file: 'lib/Shop/Web/Controller/Cart.pm', on: '$self->current_cart', offset: 8, action: DEFINITION },
    { id: 'mojo-route', title: 'A route to its action', note: "'cart#show' jumps to show() in the Cart controller.", file: 'lib/Shop/Web.pm', on: "'cart#show'", offset: 7, action: DEFINITION },
    { id: 'mojo-docs', title: 'Framework documentation', note: "Mojolicious's own POD for get(), read from its source.", file: 'lib/Shop/Web.pm', on: '$r->get', offset: 5, action: HOVER },
    { id: 'mojo-source', title: 'Into the framework', note: 'render() opens Mojolicious::Controller, read-only.', file: 'lib/Shop/Web/Controller/Cart.pm', on: '$self->render( json', offset: 8, action: DEFINITION },
    { id: 'dbic-rs', title: 'A DBIx::Class resultset', note: "resultset('Product') is a ResultSet of Product rows.", file: 'catalog.pl', on: '$products = ', action: HOVER },
    { id: 'dbic-row', title: 'A row from it', note: 'next() gives a Product row, so display_name resolves.', file: 'catalog.pl', on: '$product = ', action: HOVER },
    { id: 'dbic-column', title: 'A column accessor', note: 'price is a column declared in add_columns.', file: 'catalog.pl', on: '$product->price', offset: 10, action: DEFINITION },
    { id: 'dbic-rel', title: 'A relationship', note: 'reviews comes from has_many.', file: 'catalog.pl', on: '$product->reviews', offset: 10, action: DEFINITION },
    { id: 'minion-task', title: 'A Minion job to its task', note: 'enqueue(send_receipt => ...) jumps to the add_task that registered it.', file: 'lib/Shop/Jobs.pm', on: 'enqueue( send_receipt', offset: 9, action: DEFINITION },
    { id: 'minion-args', title: 'Job arguments', note: "total => in the enqueued list is the %args key the task reads.", file: 'lib/Shop/Jobs.pm', on: 'total => 12', action: DEFINITION },
    { id: 'minion-options', title: 'enqueue options', note: 'The options hash completes with what Minion accepts.', file: 'lib/Shop/Jobs.pm', on: '{ priority => 5', offset: 2, action: 'editor.action.triggerSuggest' },
    { id: 'minion-docs', title: 'The job object', note: '$job in the task is a Minion::Job, documented from its source.', file: 'lib/Shop/Jobs.pm', on: '$job->finish', offset: 6, action: HOVER },
    { id: 'events', title: 'An event to its handlers', note: 'emit(shipped => ...) knows the handler registered with on().', file: 'lib/Shop/Notifier.pm', on: 'emit( shipped', offset: 7, action: HOVER },
    { id: 'tt-isa', title: 'Type::Tiny on an attribute', note: 'isa => Int types the accessor.', file: 'customers.pl', on: '$points', action: HOVER },
    { id: 'tt-instance', title: 'InstanceOf', note: "isa => InstanceOf['Shop::Cart'] makes cart() a Shop::Cart.", file: 'customers.pl', on: '$cart ', action: HOVER },
    { id: 'tt-narrow', title: 'A Type::Tiny check narrows', note: 'After `return ... unless is_Int($amount)`, $amount is a number.', file: 'lib/Shop/Customer.pm', on: '$self->points( $self->points + $amount', offset: 31, action: HOVER },
    { id: 'moo-handles', title: 'Moo delegation', note: 'handles => [qw(add totals)] makes totals() a method here, typed as the cart types it.', file: 'customers.pl', on: '$customer->totals', offset: 12, action: HOVER },
    { id: 'moo-ctor', title: 'Constructor keys from has', note: 'new() takes the attributes; the ones not yet passed are offered.', file: 'customers.pl', on: "name => 'Ada', ", offset: 15, action: 'editor.action.triggerSuggest' },
    { id: 'moo-clearer', title: 'A clearer', note: "forget_address exists because of clearer => 'forget_address'.", file: 'customers.pl', on: 'forget_address', offset: 2, action: DEFINITION },
    { id: 'moo-source', title: 'Into Moo', note: 'use Moo opens the installed module.', file: 'lib/Shop/Item.pm', on: 'use Moo', offset: 5, action: DEFINITION },
  ]],
  ['Whole program', [
    { id: 'heatmap', title: 'Heatmap and dead code', note: 'Runs `perl-lsp --heatmap` on this project, in the tab: who calls what, how much, and which subs nothing reaches.', report: ['--heatmap', '/work', '--html'] },
    { id: 'callers', title: 'Who calls this?', note: 'Incoming calls to subtotal.', file: 'lib/Shop/Cart.pm', on: 'sub subtotal', offset: 4, list: 'incomingCalls' },
    { id: 'callees', title: 'What does this call?', note: 'Outgoing calls from totals, across files.', file: 'lib/Shop/Cart.pm', on: 'sub totals', offset: 4, list: 'outgoingCalls' },
    { id: 'subtypes', title: 'Subclasses', note: 'Every class that inherits from Shop::Discount.', file: 'lib/Shop/Discount.pm', on: 'package Shop::Discount', offset: 8, list: 'subtypes' },
    { id: 'class', title: 'Perl 5.38 class syntax', note: 'cart => is the field of the same name, declared with :param.', file: 'app.pl', on: 'cart => $cart', action: DEFINITION },
  ]],
  ['Teach it your own DSL', [
    { id: 'plugin-def', title: 'A flag name is a link', note: "To perl 'new_checkout' is a string. The project's plugin says it names a flag, so it jumps to the declaration.", file: 'flags.pl', on: "'new_checkout'", offset: 2, action: DEFINITION },
    { id: 'plugin-refs', title: 'Every use of a flag', note: 'The declaration and each lookup, across files.', file: 'lib/Shop/Flags.pm', on: 'gift_wrap ', action: 'editor.action.goToReferences' },
    { id: 'plugin-rename', title: 'Rename a flag', note: 'Renames it where it is declared and where it is looked up.', file: 'flags.pl', on: "'gift_wrap'", offset: 2, action: 'editor.action.rename' },
    { id: 'plugin-source', title: 'The plugin itself', note: 'Under 90 lines: a tree-sitter query for the two call shapes and a function that says what a match means. Edit it and the server restarts with your version.', file: '.perl-lsp/flags.rhai', on: 'fn patterns', offset: 0 },
    { id: 'plugin-edit', title: 'Teach it another verb', note: "Adds \"disabled\" to the plugin's verb list for you. The server restarts in a few seconds.", file: '.perl-lsp/flags.rhai', on: '"feature_flag" "enabled"', offset: 0, replace: '"feature_flag" "enabled" "disabled"' },
    { id: 'plugin-after', title: '…and disabled() follows', note: "After the edit above, 'beta_search' jumps to its declaration too. Before it, nothing happens.", file: 'flags.pl', on: "'beta_search'", offset: 2, action: DEFINITION },
  ]],
];

const monaco = await new Promise(resolve => {
  window.require.config({ paths: { vs: 'vs' } });
  window.require(['vs/editor/editor.main'], () => resolve(window.monaco));
});

// ---- LSP <-> Monaco shapes -------------------------------------------------

const toPosition = p => ({ line: p.lineNumber - 1, character: p.column - 1 });
const toRange = r => r && new monaco.Range(r.start.line + 1, r.start.character + 1, r.end.line + 1, r.end.character + 1);
const toLocation = l => {
  const uri = monaco.Uri.parse(l.uri ?? l.targetUri);
  modelFor(uri);
  return { uri, range: toRange(l.range ?? l.targetSelectionRange) };
};
const toMarkdown = c => [c].flat().map(part => ({ value: typeof part === 'string' ? part : part.kind === 'plaintext' || !part.kind && !part.language ? part.value : part.language ? '```' + part.language + '\n' + part.value + '\n```' : part.value }));
const at = (model, position) => ({ textDocument: { uri: model.uri.toString() }, position: toPosition(position) });

// LSP numbers its completion kinds 1..25 in this order; Monaco names them.
const COMPLETION_KINDS = ['Text', 'Method', 'Function', 'Constructor', 'Field', 'Variable', 'Class', 'Interface', 'Module', 'Property', 'Unit', 'Value', 'Enum', 'Keyword', 'Snippet', 'Color', 'File', 'Reference', 'Folder', 'EnumMember', 'Constant', 'Struct', 'Event', 'Operator', 'TypeParameter'];
const toCompletionKind = n => monaco.languages.CompletionItemKind[COMPLETION_KINDS[n - 1]] ?? monaco.languages.CompletionItemKind.Text;
// An LSP hint is shown as Info: Monaco draws a Hint marker but never shows
// its message, and the server's unresolved-name diagnostics are hints.
const SEVERITIES = [monaco.MarkerSeverity.Error, monaco.MarkerSeverity.Warning, monaco.MarkerSeverity.Info, monaco.MarkerSeverity.Info];

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

const [project, deps] = await Promise.all(['project.json', 'deps.json'].map(async f => (await fetch(f)).json()));
// Installed modules sit where the server looks for them without being told.
const DEPS = 'local/lib/perl5/';
function* flatten(tree, prefix = '') {
  for (const [name, value] of Object.entries(tree)) {
    if (typeof value === 'string') yield [prefix + name, value];
    else yield* flatten(value, prefix + name + '/');
  }
}
// The project's own plugins are files like any other, in the directory the
// server loads them from. Monaco has no Rhai grammar; Rust's is close.
const isPlugin = path => path.endsWith('.rhai');
const models = new Map();
for (const [path, text] of flatten(project)) {
  const language = isPlugin(path) ? 'rust' : /\.(pm|pl|t)$/.test(path) ? LANGUAGE : null;
  if (language) models.set(path, monaco.editor.createModel(text, language, monaco.Uri.parse(`${ROOT}/${path}`)));
}
// The project as it stands in the editor, with its dependencies: what a
// server or a command starts on.
function workspace() {
  const files = structuredClone(project);
  for (const [path, model] of models) {
    if (path.startsWith(DEPS)) continue;
    const parts = path.split('/'), name = parts.pop();
    parts.reduce((tree, part) => tree[part] ??= {}, files)[name] = model.getValue();
  }
  return { ...files, local: { lib: { perl5: deps } } };
}
// A dependency gets a model the first time an answer points into it: Monaco
// can only show or peek a location whose model exists.
function modelFor(uri) {
  const path = uri.toString().slice(ROOT.length + 1);
  if (models.has(path) || !path.startsWith(DEPS)) return models.get(path);
  const text = path.slice(DEPS.length).split('/').reduce((tree, part) => tree?.[part], deps);
  if (typeof text !== 'string') return undefined;
  const model = monaco.editor.createModel(text, LANGUAGE, uri);
  models.set(path, model);
  return model;
}

// The server's semantic tokens, coloured: sigils by container kind, hash
// keys, framework keywords, and writes set apart from reads.
const SEMANTIC_COLOURS = {
  light: { 'variable.scalar': '1f5fbf', 'variable.array': '8a3ffc', 'variable.hash': '9a6700', parameter: 'b3541e', property: '0b7285', macro: 'a626a4', namespace: '2b6f3e', method: '6f42c1', function: '6f42c1', keyword: 'b3372f', enumMember: '9a6700' },
  dark: { 'variable.scalar': '6cb6ff', 'variable.array': 'c792ea', 'variable.hash': 'e5c07b', parameter: 'f29e74', property: '56c2d6', macro: 'd78fe6', namespace: '7fd49b', method: 'b6a0ff', function: 'b6a0ff', keyword: 'f08a80', enumMember: 'e5c07b' },
};
for (const [name, base] of [['light', 'vs'], ['dark', 'vs-dark']]) {
  monaco.editor.defineTheme('perl-lsp-' + name, {
    base,
    inherit: true,
    rules: [
      ...Object.entries(SEMANTIC_COLOURS[name]).map(([token, foreground]) => ({ token, foreground })),
      { token: 'variable.modification', fontStyle: 'underline' },
      { token: 'variable.declaration', fontStyle: 'bold' },
    ],
    colors: {},
  });
}
const darkMode = matchMedia('(prefers-color-scheme: dark)');
const editor = monaco.editor.create(document.getElementById('editor'), {
  model: null,
  automaticLayout: true,
  minimap: { enabled: false },
  fontSize: 14,
  'semanticHighlighting.enabled': true,
  theme: 'perl-lsp-' + (darkMode.matches ? 'dark' : 'light'),
});
darkMode.addEventListener('change', () => monaco.editor.setTheme('perl-lsp-' + (darkMode.matches ? 'dark' : 'light')));

const fileList = document.getElementById('files'), depList = document.getElementById('deps');
function listFile(list, path, label) {
  const li = document.createElement('li'), b = document.createElement('button');
  b.textContent = label;
  b.dataset.path = path;
  b.onclick = () => show(path);
  li.append(b);
  list.append(li);
}
function show(path, selection) {
  const model = modelFor(monaco.Uri.parse(`${ROOT}/${path}`));
  if (!model) return false;
  const dependency = path.startsWith(DEPS);
  if (dependency && !depList.querySelector(`[data-path="${CSS.escape(path)}"]`)) {
    depList.parentElement.hidden = false;
    listFile(depList, path, path.slice(DEPS.length));
  }
  editor.setModel(model);
  editor.updateOptions({ readOnly: dependency });
  for (const b of document.querySelectorAll('nav [data-path]')) b.setAttribute('aria-current', String(b.dataset.path === path));
  if (selection) {
    editor.setSelection(selection);
    editor.revealRangeInCenterIfOutsideViewport(selection);
  }
  editor.focus();
  return true;
}
for (const path of models.keys()) listFile(fileList, path, path);
// Monaco alone has no notion of opening another file; goto-def and the
// references peek ask this opener.
monaco.editor.registerEditorOpener({
  openCodeEditor(_source, resource, where) {
    const path = resource.toString().slice(ROOT.length + 1);
    return show(path, where && (where.startLineNumber ? where : new monaco.Range(where.lineNumber, where.column, where.lineNumber, where.column)));
  },
});

// The server's own diagnostics are kept beside the markers: a code action
// request must send them back as the server wrote them.
const diagnostics = new Map();
function onNotification(method, params) {
  if (method !== 'textDocument/publishDiagnostics') return;
  const model = monaco.editor.getModel(monaco.Uri.parse(params.uri));
  if (!model) return;
  diagnostics.set(params.uri, params.diagnostics);
  monaco.editor.setModelMarkers(model, 'perl-lsp', params.diagnostics.map(d => ({
    ...toRange(d.range),
    message: d.message,
    severity: SEVERITIES[(d.severity ?? 1) - 1],
    code: d.code === undefined ? undefined : String(d.code),
    source: d.source,
  })));
}

// Starts a server on the project as it stands and opens every project file
// in it: a rename can edit any of them, and the server must then be told,
// or its copy of that file goes stale.
let lsp;
async function connect() {
  // Warnings are on because that is where the server says a plugin did not
  // compile.
  const server = startServer({ wasm: 'perl-lsp.wasm', shim: 'shim/index.js', files: workspace(), env: ['RUST_LOG=warn'] });
  lsp = server;
  server.onLog(text => {
    const complaint = /(?:WARN|ERROR)[^\n]*?\] (plugin [^\n]*)/.exec(text);
    if (complaint && lsp === server) setStatus(complaint[1].replace('/work/', ''), 'bad');
  });
  // The server's stderr, for whoever is debugging the page (and the page
  // test, which prints it when a scene fails).
  window.serverLog = server.logs;
  server.exited.then(why => { if (lsp === server) setStatus(why, 'bad'); });
  server.onNotification(onNotification);
  const { capabilities } = await server.request('initialize', {
    processId: null,
    rootUri: ROOT,
    workspaceFolders: [{ uri: ROOT, name: 'work' }],
    // The opt-in lints are on so the page can show them.
    initializationOptions: {
      diagnostics: { optionalDeref: true, redundantGuard: true, derefShape: true, unresolvedMethodCrossFile: true },
    },
    capabilities: {
      textDocument: {
        hover: { contentFormat: ['markdown', 'plaintext'] },
        completion: { completionItem: { documentationFormat: ['markdown', 'plaintext'] } },
        signatureHelp: { signatureInformation: { documentationFormat: ['markdown', 'plaintext'] } },
        rename: { prepareSupport: true },
        codeAction: { codeActionLiteralSupport: { codeActionKind: { valueSet: ['quickfix'] } } },
      },
    },
  });
  server.notify('initialized', {});
  for (const [path, model] of models) {
    if (model.getLanguageId() !== LANGUAGE || path.startsWith(DEPS)) continue;
    server.notify('textDocument/didOpen', { textDocument: { uri: model.uri.toString(), languageId: LANGUAGE, version: model.getVersionId(), text: model.getValue() } });
  }
  return capabilities;
}

setStatus('Downloading and compiling the server…');
const started = performance.now();
let capabilities;
try {
  capabilities = await connect();
} catch (e) {
  setStatus(e.message + '. If this page just loaded, it reloads itself once to turn isolation on.', 'bad');
  throw e;
}
setStatus(`Server ready in ${((performance.now() - started) / 1000).toFixed(1)} s.`, 'ok');

// The server reads its plugins once, as it starts, so an edited plugin
// takes a new server. Only the newest edit's server is kept.
let reload = 0, reloadTimer;
async function reloadPlugins() {
  const mine = ++reload;
  setStatus('Restarting the server with the edited plugin…');
  const began = performance.now();
  lsp.terminate();
  for (const model of models.values()) monaco.editor.setModelMarkers(model, 'perl-lsp', []);
  diagnostics.clear();
  try {
    await connect();
  } catch (e) {
    if (mine === reload) setStatus('The server did not restart: ' + e.message, 'bad');
    return;
  }
  // A complaint about the plugin may already be showing; it stays.
  if (mine === reload && status.className !== 'bad') setStatus(`Plugin reloaded in ${((performance.now() - began) / 1000).toFixed(1)} s.`, 'ok');
}

for (const [path, model] of models) {
  if (isPlugin(path)) {
    model.onDidChangeContent(() => {
      clearTimeout(reloadTimer);
      reloadTimer = setTimeout(reloadPlugins, 700);
    });
  } else {
    model.onDidChangeContent(() => lsp.notify('textDocument/didChange', {
      textDocument: { uri: model.uri.toString(), version: model.getVersionId() },
      contentChanges: [{ text: model.getValue() }],
    }));
  }
}

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
if (capabilities.implementationProvider) L.registerImplementationProvider(LANGUAGE, { provideImplementation: locations('textDocument/implementation') });
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

if (capabilities.codeActionProvider) L.registerCodeActionProvider(LANGUAGE, {
  async provideCodeActions(model, range) {
    const uri = model.uri.toString();
    const here = (diagnostics.get(uri) ?? []).filter(d => monaco.Range.areIntersectingOrTouching(toRange(d.range), range));
    const r = await ask('textDocument/codeAction', {
      textDocument: { uri },
      range: { start: toPosition(range.getStartPosition()), end: toPosition(range.getEndPosition()) },
      context: { diagnostics: here },
    });
    return {
      actions: (r ?? []).filter(a => a.edit).map(a => ({
        title: a.title,
        kind: a.kind,
        isPreferred: a.isPreferred,
        edit: toWorkspaceEdit(a.edit),
      })),
      dispose() {},
    };
  },
});

if (capabilities.foldingRangeProvider) L.registerFoldingRangeProvider(LANGUAGE, {
  async provideFoldingRanges(model) {
    const r = await ask('textDocument/foldingRange', { textDocument: { uri: model.uri.toString() } });
    return (r ?? []).map(f => ({ start: f.startLine + 1, end: f.endLine + 1 }));
  },
});

if (capabilities.selectionRangeProvider) L.registerSelectionRangeProvider(LANGUAGE, {
  async provideSelectionRanges(model, positions) {
    const r = await ask('textDocument/selectionRange', { textDocument: { uri: model.uri.toString() }, positions: positions.map(toPosition) });
    return (r ?? []).map(innermost => {
      const chain = [];
      for (let s = innermost; s; s = s.parent) chain.push({ range: toRange(s.range) });
      return chain;
    });
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

// Monaco has no call- or type-hierarchy view, so those answers are listed in
// the side panel, each entry a link to its location.
const HIERARCHY = {
  incomingCalls: ['textDocument/prepareCallHierarchy', 'callHierarchy/incomingCalls', r => r.from],
  outgoingCalls: ['textDocument/prepareCallHierarchy', 'callHierarchy/outgoingCalls', r => r.to],
  subtypes: ['textDocument/prepareTypeHierarchy', 'typeHierarchy/subtypes', r => r],
  supertypes: ['textDocument/prepareTypeHierarchy', 'typeHierarchy/supertypes', r => r],
};
const results = document.getElementById('results');
function listItems(heading, items) {
  results.textContent = '';
  const h = document.createElement('h2');
  h.textContent = heading;
  results.append(h);
  const ul = document.createElement('ul');
  for (const item of items) {
    const li = document.createElement('li'), b = document.createElement('button');
    const path = item.uri.slice(ROOT.length + 1);
    b.textContent = `${item.name}  ${path}:${item.selectionRange.start.line + 1}`;
    b.onclick = () => show(path, toRange(item.selectionRange));
    li.append(b);
    ul.append(li);
  }
  if (!items.length) ul.append(Object.assign(document.createElement('li'), { textContent: 'Nothing found.' }));
  results.append(ul);
}
async function listHierarchy(kind, model, position, heading) {
  const [prepare, method, pick] = HIERARCHY[kind];
  const roots = await ask(prepare, at(model, position));
  const answer = roots?.[0] ? await ask(method, { item: roots[0] }) : [];
  listItems(heading, (answer ?? []).map(pick));
}

const search = document.getElementById('symbol-search');
search.addEventListener('input', async () => {
  const query = search.value.trim();
  if (!query) return void (results.textContent = '');
  const found = await ask('workspace/symbol', { query });
  if (search.value.trim() !== query) return;
  listItems(`Symbols matching "${query}"`, (found ?? []).map(s => ({ name: s.name, uri: s.location.uri, selectionRange: s.location.range })));
});

// A report is a CLI mode of the same binary, run to completion in a process
// of its own on the project as it stands in the editor. Its output is a
// self-contained page, shown sandboxed.
const report = document.getElementById('report');
report.querySelector('button').onclick = () => report.close();
let reporting = false;
async function runReport(scene) {
  if (reporting) return;
  reporting = true;
  results.textContent = `Running perl-lsp ${scene.report.join(' ')} …`;
  try {
    const started = performance.now();
    const run = await runCommand({ wasm: 'perl-lsp.wasm', shim: 'shim/index.js', files: workspace(), args: scene.report });
    if (run.code !== 0) throw new Error(run.stderr.trim().split('\n').pop() || `exit code ${run.code}`);
    results.textContent = `perl-lsp ${scene.report.join(' ')} finished in ${((performance.now() - started) / 1000).toFixed(1)} s.`;
    report.querySelector('h2').textContent = scene.title;
    report.querySelector('iframe').srcdoc = run.stdout;
    report.showModal();
  } catch (e) {
    results.textContent = 'The report failed: ' + e.message;
  } finally {
    reporting = false;
  }
}

function runScene(scene) {
  if (scene.report) return void runReport(scene);
  const model = models.get(scene.file);
  const match = model.findMatches(scene.on, false, false, true, null, false)[scene.nth ?? 0];
  if (!match) return;
  const start = match.range.getStartPosition();
  // One column in by default, so the cursor is inside the token whatever
  // its sigil.
  const position = { lineNumber: start.lineNumber, column: start.column + (scene.offset ?? 1) };
  show(scene.file, monaco.Range.fromPositions(position));
  editor.revealLineInCenter(position.lineNumber);
  results.textContent = '';
  if (scene.replace) {
    if (!model.getValue().includes(scene.replace)) editor.executeEdits('demo', [{ range: match.range, text: scene.replace }]);
  } else if (scene.list) listHierarchy(scene.list, model, position, scene.title);
  else if (scene.action) editor.trigger('demo', scene.action, null);
}

const sceneList = document.getElementById('scenes');
for (const [group, scenes] of SCENES) {
  const heading = document.createElement('h2');
  heading.textContent = group;
  const ol = document.createElement('ol');
  for (const scene of scenes) {
    const li = document.createElement('li'), b = document.createElement('button');
    const title = document.createElement('strong'), note = document.createElement('span');
    title.textContent = scene.title;
    note.textContent = scene.note;
    b.dataset.scene = scene.id;
    b.append(title, note);
    b.onclick = () => runScene(scene);
    li.append(b);
    ol.append(li);
  }
  sceneList.append(heading, ol);
}

show('app.pl');
