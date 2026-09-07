//! The php editor surface over stdio — the verbs no CLI mirror reaches:
//! signature help (the pack call-site path), the nested document outline,
//! and the diagnostics a didOpen publishes. A tiny real client: it answers
//! the server→client requests an editor answers so the server cannot wedge.
#![cfg(feature = "php")]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};

struct Client {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    next_id: i64,
    notes: Vec<serde_json::Value>,
}

impl Client {
    fn spawn(root: &std::path::Path) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .current_dir(root)
            .env("XDG_CACHE_HOME", root.join(".cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn perl-lsp");
        let reader = BufReader::new(child.stdout.take().unwrap());
        Client { child, reader, next_id: 1, notes: Vec::new() }
    }
    fn send(&mut self, v: serde_json::Value) {
        let body = v.to_string();
        let stdin = self.child.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        stdin.flush().unwrap();
    }
    fn read_message(&mut self) -> serde_json::Value {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0u8; len];
        self.reader.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read_message();
            if m.get("id") == Some(&serde_json::json!(id)) && m.get("method").is_none() {
                return m["result"].clone();
            }
            if m.get("id").is_some() && m.get("method").is_some() {
                // a server→client request: answer like an editor
                self.send(serde_json::json!({"jsonrpc": "2.0", "id": m["id"], "result": serde_json::Value::Null}));
                continue;
            }
            self.notes.push(m);
        }
    }
    fn notify(&mut self, method: &str, params: serde_json::Value) {
        self.send(serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

fn uri(p: &std::path::Path) -> String {
    format!("file://{}", p.display())
}

#[test]
fn php_signature_help_outline_and_diagnostics_over_stdio() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2stdio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Mailer.php", "<?php\nnamespace App;\n\nclass Mailer\n{\n    /**\n     * Send one message.\n     */\n    public function send(string $to, string $subject, string $body = ''): bool\n    {\n        return $to !== '' && $subject !== '';\n    }\n}\n");
    let service = "<?php\nnamespace App;\n\nclass Service\n{\n    public function __construct(private Mailer $mailer) {}\n\n    public function run(string $who): void\n    {\n        $this->mailer->send($who, 'x');\n        $this->missingMethod();\n    }\n}\n";
    w("src/Service.php", service);
    let mut c = Client::spawn(&dir);
    c.request("initialize", serde_json::json!({"processId": null, "rootUri": uri(&dir), "capabilities": {"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}}}}));
    c.notify("initialized", serde_json::json!({}));
    let svc = uri(&dir.join("src/Service.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": svc, "languageId": "php", "version": 1, "text": service}}));
    // readiness: the workspace index answers a cross-file definition
    let send_col = service.lines().nth(9).unwrap().find("send").unwrap();
    let mut ready = false;
    for _ in 0..120 {
        let r = c.request("textDocument/definition", serde_json::json!({"textDocument": {"uri": svc}, "position": {"line": 9, "character": send_col}}));
        if r.as_array().is_some_and(|a| !a.is_empty()) || r.is_object() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(ready, "definition never answered");
    // signature help inside the second argument of `send(`
    let arg2 = service.lines().nth(9).unwrap().find("'x'").unwrap() + 1;
    let sig = c.request("textDocument/signatureHelp", serde_json::json!({"textDocument": {"uri": svc}, "position": {"line": 9, "character": arg2}}));
    let label = sig["signatures"][0]["label"].as_str().unwrap_or("");
    assert!(label.starts_with("send(string $to, string $subject, string $body = '')"), "{sig}");
    assert_eq!(sig["activeParameter"], serde_json::json!(1), "{sig}");
    assert_eq!(sig["signatures"][0]["parameters"].as_array().map(|a| a.len()), Some(3), "{sig}");
    assert!(sig["signatures"][0]["documentation"].to_string().contains("Send one message"), "{sig}");
    // typeDefinition on the property token (`$this->mailer`) and on the
    // call (`->send()` returns bool: no class, empty) — the member ladder
    let mailer_col = service.lines().nth(9).unwrap().find("mailer").unwrap();
    let td = c.request("textDocument/typeDefinition", serde_json::json!({"textDocument": {"uri": svc}, "position": {"line": 9, "character": mailer_col}}));
    let td_uris: Vec<String> = td.as_array().map(|a| a.iter().filter_map(|l| l["uri"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    assert!(td_uris.iter().any(|u| u.ends_with("src/Mailer.php")), "typeDefinition on a property read: {td}");
    let td = c.request("textDocument/typeDefinition", serde_json::json!({"textDocument": {"uri": svc}, "position": {"line": 9, "character": send_col}}));
    assert!(td.as_array().map(|a| a.is_empty()).unwrap_or(true), "a bool-returning call has no class: {td}");
    // the outline nests members under the class
    let syms = c.request("textDocument/documentSymbol", serde_json::json!({"textDocument": {"uri": svc}}));
    let class = syms.as_array().unwrap().iter().find(|s| s["name"] == "Service").expect("class");
    let kids: Vec<&str> = class["children"].as_array().unwrap().iter().filter_map(|k| k["name"].as_str()).collect();
    assert!(kids.contains(&"__construct") && kids.contains(&"run"), "{kids:?}");
    // diagnostics: the didOpen publish carries the undefined method
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut found = false;
    while std::time::Instant::now() < deadline && !found {
        // drain by issuing a cheap request; notifications queue in `notes`
        c.request("textDocument/documentSymbol", serde_json::json!({"textDocument": {"uri": svc}}));
        found = c.notes.iter().any(|n| {
            n["method"] == "textDocument/publishDiagnostics"
                && n["params"]["diagnostics"].as_array().is_some_and(|d| d.iter().any(|x| x["message"].as_str().unwrap_or("").contains("missingMethod")))
        });
        if !found {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    assert!(found, "no unresolved-method diagnostic published: {:?}", c.notes.iter().filter(|n| n["method"] == "textDocument/publishDiagnostics").collect::<Vec<_>>());
    c.request("shutdown", serde_json::Value::Null);
    c.notify("exit", serde_json::Value::Null);
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The editor-shape verbs on a php document: folding follows the scopes
/// the skeleton minted, selectionRange walks the tree's ancestors, and a
/// method's outgoing calls are its callees — a property read is a value,
/// not a call.
#[test]
fn php_editor_axes_over_stdio() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2axes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    let src = "<?php\nnamespace App;\n\nclass Queue\n{\n    private array $items = [];\n    const LIMIT = 3;\n\n    public function push(string $x): void\n    {\n        if (\\count($this->items) < self::LIMIT) {\n            $this->items[] = $x;\n        }\n    }\n\n    public function fill(): void\n    {\n        $this->push('a');\n        $this->push('b');\n        $n = $this->items;\n    }\n\n    public function all()\n    {\n        return $this->items;\n    }\n}\n";
    w("src/Queue.php", src);
    let mut c = Client::spawn(&dir);
    c.request("initialize", serde_json::json!({"processId": null, "rootUri": uri(&dir), "capabilities": {}}));
    c.notify("initialized", serde_json::json!({}));
    let u = uri(&dir.join("src/Queue.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": u, "languageId": "php", "version": 1, "text": src}}));
    let col = |line: usize, needle: &str| src.lines().nth(line).unwrap().find(needle).unwrap();
    // readiness: goto-def on the `push` call answers the declaration
    let mut ready = false;
    for _ in 0..120 {
        let r = c.request("textDocument/definition", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": 17, "character": col(17, "push")}}));
        if r.as_array().is_some_and(|a| !a.is_empty()) || r.is_object() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(ready, "definition never answered");
    // folding: the class body, both method bodies and the `if` block
    let folds = c.request("textDocument/foldingRange", serde_json::json!({"textDocument": {"uri": u}}));
    let ranges: Vec<(u64, u64)> = folds.as_array().map(|a| a.iter().map(|f| (f["startLine"].as_u64().unwrap(), f["endLine"].as_u64().unwrap())).collect()).unwrap_or_default();
    assert!(ranges.contains(&(3, 26)) || ranges.contains(&(4, 26)), "class body folds: {ranges:?}");
    assert!(ranges.iter().any(|r| r.0 >= 8 && r.1 == 13), "push body folds: {ranges:?}");
    assert!(ranges.iter().any(|r| r.0 == 10 && r.1 == 12), "the if block folds: {ranges:?}");
    // selectionRange: the token's ancestors up to the file
    let sel = c.request("textDocument/selectionRange", serde_json::json!({"textDocument": {"uri": u}, "positions": [{"line": 17, "character": col(17, "push")}]}));
    let mut depth = 0;
    let mut node = sel.as_array().and_then(|a| a.first().cloned());
    while let Some(n) = node {
        depth += 1;
        node = n.get("parent").filter(|p| !p.is_null()).cloned();
    }
    assert!(depth >= 4, "selection range ancestors: {sel}");
    // call hierarchy from `fill`: outgoing is `push` (twice), never the
    // `$this->items` read; incoming on `push` is `fill`
    let items = c.request("textDocument/prepareCallHierarchy", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": 15, "character": col(15, "fill")}}));
    let item = items.as_array().and_then(|a| a.first().cloned()).expect("prepareCallHierarchy on fill");
    let out = c.request("callHierarchy/outgoingCalls", serde_json::json!({"item": item}));
    let to: Vec<String> = out.as_array().map(|a| a.iter().filter_map(|e| e["to"]["name"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    assert_eq!(to, vec!["push".to_string()], "outgoing calls of fill: {out}");
    assert_eq!(out[0]["fromRanges"].as_array().map(|a| a.len()), Some(2), "{out}");
    let items = c.request("textDocument/prepareCallHierarchy", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": 8, "character": col(8, "push")}}));
    let item = items.as_array().and_then(|a| a.first().cloned()).expect("prepareCallHierarchy on push");
    let inc = c.request("callHierarchy/incomingCalls", serde_json::json!({"item": item}));
    let from: Vec<String> = inc.as_array().map(|a| a.iter().filter_map(|e| e["from"]["name"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    assert_eq!(from, vec!["fill".to_string()], "incoming calls of push: {inc}");
    // parameter-name hints: `push('a')` / `push('b')` show `x:`; the
    // property read on the next line is not a call, and the untyped local
    // it fills gets the type lane's `: array`
    let hints = c.request("textDocument/inlayHint", serde_json::json!({"textDocument": {"uri": u}, "range": {"start": {"line": 15, "character": 0}, "end": {"line": 21, "character": 0}}}));
    let got: Vec<(u64, u64, String)> = hints.as_array().map(|a| a.iter().map(|h| (h["position"]["line"].as_u64().unwrap(), h["position"]["character"].as_u64().unwrap(), h["label"].as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
    assert_eq!(got, vec![(19, col(19, " = $this") as u64, ": array".to_string()), (17, col(17, "'a'") as u64, "x:".to_string()), (18, col(18, "'b'") as u64, "x:".to_string())], "inlay hints: {hints}");
    // missing return type: `all()` returns the array property in a file that
    // declares return types; the quick-fix writes `: array` after `all()`
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut mrt: Option<serde_json::Value> = None;
    while std::time::Instant::now() < deadline && mrt.is_none() {
        c.request("textDocument/documentSymbol", serde_json::json!({"textDocument": {"uri": u}}));
        mrt = c.notes.iter().rev().filter(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == u)
            .flat_map(|n| n["params"]["diagnostics"].as_array().cloned().unwrap_or_default())
            .find(|d| d["code"] == "missing-return-type");
        if mrt.is_none() { std::thread::sleep(std::time::Duration::from_millis(250)); }
    }
    let mrt = mrt.expect("missing-return-type hint on all()");
    // the declaration hover says what the bag infers for the untyped callable
    let hv = c.request("textDocument/hover", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": 22, "character": col(22, "all")}}));
    assert!(hv.to_string().contains("returns: array"), "hover on all(): {hv}");
    assert_eq!(mrt["range"]["start"]["line"], 22, "{mrt}");
    assert!(mrt["message"].as_str().unwrap().contains("`array`"), "{mrt}");
    let acts = c.request("textDocument/codeAction", serde_json::json!({"textDocument": {"uri": u}, "range": mrt["range"], "context": {"diagnostics": [mrt]}}));
    let act = acts.as_array().and_then(|a| a.iter().find(|x| x["title"].as_str().unwrap_or("").starts_with("Add return type"))).cloned().unwrap_or_else(|| panic!("return type action: {acts}"));
    let e = &act["edit"]["changes"][&u][0];
    assert_eq!(e["newText"], ": array", "{act}");
    assert_eq!((e["range"]["start"]["line"].as_u64(), e["range"]["start"]["character"].as_u64()), (Some(22), Some(col(22, "()") as u64 + 2)), "{act}");
    c.request("shutdown", serde_json::Value::Null);
    c.notify("exit", serde_json::Value::Null);
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Contracts: a class implementing an interface (or extending an abstract
/// class) that leaves a method undeclared is reported — a `__call` catch-all
/// does not excuse it — an abstract composer is not, and the quick-fix
/// declares the missing methods from the contract's own declarator.
#[test]
fn php_unimplemented_method_over_stdio() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Greeter.php", "<?php\nnamespace App;\n\ninterface Greeter\n{\n    public function hi(string $n): string;\n    public function bye(): void;\n}\n");
    w("src/Base.php", "<?php\nnamespace App;\n\nabstract class Base implements Greeter\n{\n    public function hi(string $n): string\n    {\n        return $n;\n    }\n\n    abstract protected function tag(): string;\n}\n");
    let en = "<?php\nnamespace App;\n\nclass En implements Greeter\n{\n    public function hi(string $n): string\n    {\n        return $n;\n    }\n}\n\nclass Sub extends Base\n{\n}\n\nclass Dyn implements Greeter\n{\n    public function __call($m, $a) {}\n}\n";
    w("src/En.php", en);
    let mut c = Client::spawn(&dir);
    c.request("initialize", serde_json::json!({"processId": null, "rootUri": uri(&dir), "capabilities": {}}));
    c.notify("initialized", serde_json::json!({}));
    let u = uri(&dir.join("src/En.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": u, "languageId": "php", "version": 1, "text": en}}));
    // the lane publishes once the pack index has settled: wait for it
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut diags: Vec<serde_json::Value> = Vec::new();
    while std::time::Instant::now() < deadline {
        c.request("textDocument/documentSymbol", serde_json::json!({"textDocument": {"uri": u}}));
        if let Some(n) = c.notes.iter().rev().find(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == u) {
            let ds: Vec<serde_json::Value> = n["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
            if ds.iter().any(|d| d["code"] == "unimplemented-method") {
                diags = ds;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let contract: Vec<&serde_json::Value> = diags.iter().filter(|d| d["code"] == "unimplemented-method").collect();
    let msgs: Vec<String> = contract.iter().map(|d| d["message"].as_str().unwrap_or("").to_string()).collect();
    assert_eq!(contract.len(), 3, "one diagnostic per concrete composer: {msgs:?}");
    let en_diag = contract.iter().find(|d| d["range"]["start"]["line"] == 3).expect("En is reported");
    assert!(en_diag["message"].as_str().unwrap().contains("`Greeter::bye()`") && !en_diag["message"].as_str().unwrap().contains("hi()"), "{msgs:?}");
    let sub_diag = contract.iter().find(|d| d["range"]["start"]["line"] == 11).expect("Sub is reported");
    let sub_msg = sub_diag["message"].as_str().unwrap();
    assert!(sub_msg.contains("`Greeter::bye()`") && sub_msg.contains("`Base::tag()`") && !sub_msg.contains("hi()"), "{sub_msg}");
    // `__call` catches calls at runtime; the contract is checked at declaration
    let dyn_diag = contract.iter().find(|d| d["range"]["start"]["line"] == 15).expect("Dyn is reported");
    assert!(dyn_diag["message"].as_str().unwrap().contains("`Greeter::hi()`"), "{msgs:?}");
    // the quick-fix on Sub: both stubs before the closing brace
    let acts = c.request("textDocument/codeAction", serde_json::json!({"textDocument": {"uri": u}, "range": sub_diag["range"], "context": {"diagnostics": [sub_diag]}}));
    let act = acts.as_array().and_then(|a| a.iter().find(|x| x["title"].as_str().unwrap_or("").starts_with("Implement"))).cloned().unwrap_or_else(|| panic!("implement action; diag data {} actions {acts}", sub_diag["data"]));
    assert_eq!(act["title"], "Implement 2 missing methods", "{act}");
    let edits = act["edit"]["changes"][&u].as_array().cloned().unwrap_or_default();
    assert_eq!(edits.len(), 1, "{act}");
    let new_text = edits[0]["newText"].as_str().unwrap();
    assert!(new_text.contains("public function bye(): void\n    {\n") && new_text.contains("public function tag(): string"), "{new_text}");
    assert_eq!(edits[0]["range"]["start"]["line"], 13, "{act}");
    c.request("shutdown", serde_json::Value::Null);
    c.notify("exit", serde_json::Value::Null);
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A class declared under another namespace completes on its leaf with the
/// `use` row as the edit that makes it spellable; a class the file already
/// imports or declares carries no edit.
#[test]
fn php_auto_import_completion_over_stdio() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2autoimport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/Util")).unwrap();
    std::fs::create_dir_all(dir.join("src/Web")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Util/Greeter.php", "<?php\nnamespace App\\Util;\n\nclass Greeter\n{\n    public function hi(string $n): string { return $n; }\n}\n");
    w("src/Util/Grid.php", "<?php\nnamespace App\\Util;\n\nclass Grid\n{\n}\n");
    let home = "<?php\nnamespace App\\Web;\n\nuse App\\Util\\Grid;\n\nclass Home\n{\n    public function run(): string\n    {\n        $g = new Gre\n        $x = new Grid();\n        return $g->hi(\"x\");\n    }\n}\n";
    w("src/Web/Home.php", home);
    let mut c = Client::spawn(&dir);
    c.request("initialize", serde_json::json!({"processId": null, "rootUri": uri(&dir), "capabilities": {}}));
    c.notify("initialized", serde_json::json!({}));
    let u = uri(&dir.join("src/Web/Home.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": u, "languageId": "php", "version": 1, "text": home}}));
    let col = |line: usize, needle: &str| home.lines().nth(line).unwrap().find(needle).unwrap();
    // readiness: the pack index answers a cross-file definition (`Grid` is imported)
    let mut ready = false;
    for _ in 0..120 {
        let r = c.request("textDocument/definition", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": 10, "character": col(10, "Grid")}}));
        if r.as_array().is_some_and(|a| !a.is_empty()) || r.is_object() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(ready, "definition never answered");
    let items = |c: &mut Client, line: usize, character: usize| -> Vec<serde_json::Value> {
        let r = c.request("textDocument/completion", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": line, "character": character}}));
        r.get("items").and_then(|i| i.as_array()).cloned().or_else(|| r.as_array().cloned()).unwrap_or_default()
    };
    let gre = items(&mut c, 9, col(9, "Gre") + 3);
    let g = gre.iter().find(|i| i["label"] == "Greeter").unwrap_or_else(|| panic!("Greeter offered: {gre:?}"));
    assert_eq!(g["detail"], "App\\Util\\Greeter", "{g}");
    let edits = g["additionalTextEdits"].as_array().cloned().unwrap_or_default();
    assert_eq!(edits.len(), 1, "{g}");
    assert_eq!(edits[0]["newText"], "use App\\Util\\Greeter;\n", "{g}");
    assert_eq!(edits[0]["range"]["start"]["line"], 4, "after the last use row: {g}");
    // the imported class completes without an edit
    let gri = items(&mut c, 10, col(10, "Gri") + 3);
    let gd = gri.iter().find(|i| i["label"] == "Grid").unwrap_or_else(|| panic!("Grid offered: {gri:?}"));
    assert!(gd["additionalTextEdits"].is_null() || gd["additionalTextEdits"].as_array().is_some_and(|a| a.is_empty()), "{gd}");
    c.request("shutdown", serde_json::Value::Null);
    c.notify("exit", serde_json::Value::Null);
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// `parent::__construct()` under a same-leaf alias (`use App\Base\Manager as
/// BaseManager; class Manager extends BaseManager`) is the PARENT's
/// constructor: no arity report for a call the parent accepts, one for a
/// call it does not.
#[test]
fn php_parent_call_under_same_leaf_alias_over_stdio() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2palias-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/Base")).unwrap();
    std::fs::create_dir_all(dir.join("src/App")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Base/Manager.php", "<?php\nnamespace App\\Base;\n\nclass Manager\n{\n    public function __construct()\n    {\n    }\n\n    public function boot(string $env): void\n    {\n    }\n}\n");
    let child = "<?php\nnamespace App\\App;\n\nuse App\\Base\\Manager as BaseManager;\n\nclass Manager extends BaseManager\n{\n    public function __construct(array $x)\n    {\n        parent::__construct();\n        parent::boot('dev', 'extra');\n    }\n}\n";
    w("src/App/Manager.php", child);
    let mut c = Client::spawn(&dir);
    c.request("initialize", serde_json::json!({"processId": null, "rootUri": uri(&dir), "capabilities": {}}));
    c.notify("initialized", serde_json::json!({}));
    let u = uri(&dir.join("src/App/Manager.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": u, "languageId": "php", "version": 1, "text": child}}));
    // the arity lane runs once the pack index settles: wait for the one true report
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut diags: Vec<serde_json::Value> = Vec::new();
    while std::time::Instant::now() < deadline {
        c.request("textDocument/documentSymbol", serde_json::json!({"textDocument": {"uri": u}}));
        if let Some(n) = c.notes.iter().rev().find(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == u) {
            let ds: Vec<serde_json::Value> = n["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
            if ds.iter().any(|d| d["code"] == "arity-mismatch") {
                diags = ds;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let arity: Vec<(u64, String)> = diags.iter().filter(|d| d["code"] == "arity-mismatch").map(|d| (d["range"]["start"]["line"].as_u64().unwrap(), d["message"].as_str().unwrap_or("").to_string())).collect();
    assert_eq!(arity.len(), 1, "only the call the parent rejects: {arity:?}");
    assert_eq!(arity[0].0, 10, "{arity:?}");
    assert!(arity[0].1.contains("Expected 1"), "the PARENT's boot(string $env): {arity:?}");
    c.request("shutdown", serde_json::Value::Null);
    c.notify("exit", serde_json::Value::Null);
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scoped access completes the class's members: `self::` and `Cfg::`
/// answer constants and methods — never the function's locals — and
/// `$this->` keeps answering the instance members.
#[test]
fn php_scoped_completion_over_stdio() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2scoped-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    let cfg = "<?php\nnamespace App;\n\nclass Cfg\n{\n    const LIMIT = 1;\n    public static int $count = 0;\n    public static function make(): Cfg { return new Cfg(); }\n    public function inst(): int\n    {\n        $local = 2;\n        $a = self::LIMIT;\n        $b = Cfg::make();\n        $c = $this->inst();\n        return $local + $a + $c;\n    }\n}\n";
    w("src/Cfg.php", cfg);
    let mut c = Client::spawn(&dir);
    c.request("initialize", serde_json::json!({"processId": null, "rootUri": uri(&dir), "capabilities": {}}));
    c.notify("initialized", serde_json::json!({}));
    let u = uri(&dir.join("src/Cfg.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": u, "languageId": "php", "version": 1, "text": cfg}}));
    let col = |line: usize, needle: &str| cfg.lines().nth(line).unwrap().find(needle).unwrap();
    let mut ready = false;
    for _ in 0..120 {
        let r = c.request("textDocument/definition", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": 12, "character": col(12, "make")}}));
        if r.as_array().is_some_and(|a| !a.is_empty()) || r.is_object() {
            ready = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(ready, "definition never answered");
    let labels = |c: &mut Client, line: usize, character: usize| -> Vec<String> {
        let r = c.request("textDocument/completion", serde_json::json!({"textDocument": {"uri": u}, "position": {"line": line, "character": character}}));
        let items = r.get("items").and_then(|i| i.as_array()).cloned().or_else(|| r.as_array().cloned()).unwrap_or_default();
        items.iter().filter_map(|i| i["label"].as_str().map(str::to_string)).collect()
    };
    // cursor right after `self::` (on the `L` of LIMIT)
    let s = labels(&mut c, 11, col(11, "LIMIT"));
    assert!(s.iter().any(|l| l == "LIMIT") && s.iter().any(|l| l == "make"), "self:: members: {s:?}");
    // a static property is spelled `self::$count`; it is not an instance member
    assert!(s.iter().any(|l| l == "$count") && !s.iter().any(|l| l == "count"), "self:: static property: {s:?}");
    assert!(!s.iter().any(|l| l.contains("local") || l == "inst"), "self:: offers constants and statics only: {s:?}");
    let k = labels(&mut c, 12, col(12, "make"));
    assert!(k.iter().any(|l| l == "make") && !k.iter().any(|l| l.contains("local") || l == "inst"), "Cfg:: members: {k:?}");
    assert!(k.iter().any(|l| l == "class") && s.iter().any(|l| l == "class"), "Cfg::class is the pack's class literal: {k:?}");
    let t = labels(&mut c, 13, col(13, "inst"));
    assert!(t.iter().any(|l| l == "inst") && !t.iter().any(|l| l.contains("local") || l == "LIMIT" || l == "class" || l == "count" || l == "$count"), "$this-> members: {t:?}");
    c.request("shutdown", serde_json::Value::Null);
    c.notify("exit", serde_json::Value::Null);
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The import quick-fix: an undefined type the workspace declares elsewhere
/// publishes (once the pack index has settled) with its candidates, and
/// `codeAction` offers `use App\Util\Helper;` after the last `use` row.
#[test]
fn php_import_class_code_action_over_stdio() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/Util")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Util/Helper.php", "<?php\nnamespace App\\Util;\n\nclass Helper\n{\n    public static function go(): int { return 1; }\n}\n");
    w("src/Mailer.php", "<?php\nnamespace App;\n\nclass Mailer\n{\n    public function send(): bool { return true; }\n}\n");
    let service = "<?php\nnamespace App;\n\nuse App\\Mailer;\n\nclass Service\n{\n    public function run(): int\n    {\n        return Helper::go();\n    }\n}\n";
    w("src/Service.php", service);
    let mut c = Client::spawn(&dir);
    c.request("initialize", serde_json::json!({"processId": null, "rootUri": uri(&dir), "capabilities": {}}));
    c.notify("initialized", serde_json::json!({}));
    let svc = uri(&dir.join("src/Service.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": svc, "languageId": "php", "version": 1, "text": service}}));
    // the undefined-type diagnostic publishes once the pack index settles
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut diag: Option<serde_json::Value> = None;
    while std::time::Instant::now() < deadline && diag.is_none() {
        c.request("textDocument/documentSymbol", serde_json::json!({"textDocument": {"uri": svc}}));
        diag = c.notes.iter().filter(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == svc)
            .flat_map(|n| n["params"]["diagnostics"].as_array().cloned().unwrap_or_default())
            .find(|d| d["code"] == "undefined-type");
        if diag.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    let diag = diag.expect("no undefined-type diagnostic published");
    assert!(diag["message"].as_str().unwrap().contains("App\\Helper"), "{diag}");
    let actions = c.request("textDocument/codeAction", serde_json::json!({"textDocument": {"uri": svc}, "range": diag["range"], "context": {"diagnostics": [diag]}}));
    let actions = actions.as_array().cloned().unwrap_or_default();
    let import = actions.iter().find(|a| a["title"] == "Add 'use App\\Util\\Helper;'").unwrap_or_else(|| panic!("{actions:?}"));
    let edit = &import["edit"]["changes"][&svc][0];
    assert_eq!(edit["newText"], "use App\\Util\\Helper;\n", "{edit}");
    // after the `use App\Mailer;` row (line 3): line 4
    assert_eq!(edit["range"]["start"]["line"], 4, "{edit}");
    assert_eq!(edit["range"]["start"]["character"], 0, "{edit}");
    // a namespace-less script with `declare(strict_types=1)`: the import goes
    // after the preamble, never before the declare
    let script = "<?php\ndeclare(strict_types=1);\n\nHelper::go();\n";
    w("bin/run.php", script);
    let run_uri = uri(&dir.join("bin/run.php"));
    c.notify("textDocument/didOpen", serde_json::json!({"textDocument": {"uri": run_uri, "languageId": "php", "version": 1, "text": script}}));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut sdiag: Option<serde_json::Value> = None;
    while std::time::Instant::now() < deadline && sdiag.is_none() {
        c.request("textDocument/documentSymbol", serde_json::json!({"textDocument": {"uri": run_uri}}));
        sdiag = c.notes.iter().filter(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == run_uri)
            .flat_map(|n| n["params"]["diagnostics"].as_array().cloned().unwrap_or_default())
            .find(|d| d["code"] == "undefined-type");
        if sdiag.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
    let sdiag = sdiag.expect("no undefined-type diagnostic on the script");
    let actions = c.request("textDocument/codeAction", serde_json::json!({"textDocument": {"uri": run_uri}, "range": sdiag["range"], "context": {"diagnostics": [sdiag]}}));
    let import = actions.as_array().and_then(|a| a.iter().find(|x| x["title"] == "Add 'use App\\Util\\Helper;'").cloned()).unwrap_or_else(|| panic!("{actions}"));
    let edit = &import["edit"]["changes"][&run_uri][0];
    assert_eq!(edit["range"]["start"]["line"], 2, "after the declare row: {edit}");
    assert_eq!(edit["newText"], "\nuse App\\Util\\Helper;\n", "{edit}");
    // the fixture's `use App\Mailer;` is never spelled: an unnecessary-tagged
    // hint with a row-deleting quick-fix
    let unused = c.notes.iter().filter(|n| n["method"] == "textDocument/publishDiagnostics" && n["params"]["uri"] == svc)
        .flat_map(|n| n["params"]["diagnostics"].as_array().cloned().unwrap_or_default())
        .find(|d| d["code"] == "unused-import")
        .expect("no unused-import diagnostic");
    assert_eq!(unused["tags"], serde_json::json!([1]), "{unused}");
    assert_eq!(unused["range"]["start"]["line"], 3, "{unused}");
    let actions = c.request("textDocument/codeAction", serde_json::json!({"textDocument": {"uri": svc}, "range": unused["range"], "context": {"diagnostics": [unused]}}));
    let remove = actions.as_array().and_then(|a| a.iter().find(|x| x["title"] == "Remove unused import").cloned()).unwrap_or_else(|| panic!("{actions}"));
    let edit = &remove["edit"]["changes"][&svc][0];
    assert_eq!(edit["newText"], "", "{edit}");
    assert_eq!((edit["range"]["start"]["line"].as_u64(), edit["range"]["end"]["line"].as_u64()), (Some(3), Some(4)), "{edit}");
    c.request("shutdown", serde_json::Value::Null);
    c.notify("exit", serde_json::Value::Null);
    let _ = c.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
