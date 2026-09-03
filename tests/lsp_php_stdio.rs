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
    assert!(!s.iter().any(|l| l.contains("local") || l == "inst"), "self:: offers constants and statics only: {s:?}");
    let k = labels(&mut c, 12, col(12, "make"));
    assert!(k.iter().any(|l| l == "make") && !k.iter().any(|l| l.contains("local") || l == "inst"), "Cfg:: members: {k:?}");
    assert!(k.iter().any(|l| l == "class") && s.iter().any(|l| l == "class"), "Cfg::class is the pack's class literal: {k:?}");
    let t = labels(&mut c, 13, col(13, "inst"));
    assert!(t.iter().any(|l| l == "inst") && !t.iter().any(|l| l.contains("local") || l == "LIMIT" || l == "class"), "$this-> members: {t:?}");
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
