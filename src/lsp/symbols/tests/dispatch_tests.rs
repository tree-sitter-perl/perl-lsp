//! String-dispatch signature help and dispatch-arg completion (mojo-events path).

use super::*;

// ---- String-dispatch signature help (mojo-events plugin path) ----

/// `$self->emit('ready', CURSOR)` should surface the `->on('ready', sub
/// ($self, $msg) {})` handler's params as sig help. The dispatch string
/// is arg 0; handler params are offset by 1 so active_parameter lines
/// up with the user's cursor.
#[test]
fn sig_help_returns_handler_params_for_emit() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub register {
    my $self = shift;
    $self->on('ready', sub {
        my ($self_in, $msg, $when) = @_;
        warn $msg;
    });
}

sub fire {
    my $self = shift;
    $self->emit('ready', 'hi', )
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    // Cursor just after `'hi', ` on the emit line — active_param=2 means
    // we're in the 2nd handler slot (after event name + first handler arg).
    let pos = {
        let (line_idx, line) = src
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("->emit('ready'"))
            .unwrap();
        let col = line.find(", )").unwrap() + 2;
        Position {
            line: line_idx as u32,
            character: col as u32,
        }
    };

    let sig = signature_help(&analysis, &tree, src, pos, &idx)
        .expect("sig help should surface handler sig");
    assert_eq!(sig.signatures.len(), 1, "one registered handler");

    let s = &sig.signatures[0];
    // Label mirrors the actual call the user is writing — `emit('ready',
    // $msg, $when)` — not a fake method-call shape.
    assert!(
        s.label.starts_with("emit('ready'"),
        "label should show the call shape starting with emit('ready'): {}",
        s.label
    );
    // Documentation carries the class + line provenance.
    if let Some(Documentation::String(ref d)) = s.documentation {
        assert!(
            d.contains("My::Emitter"),
            "doc should name the owning class: {}",
            d
        );
    } else {
        panic!("expected Documentation::String, got {:?}", s.documentation);
    }
    // $self_in stripped as implicit → remaining params $msg, $when.
    let params = s.parameters.as_ref().expect("has params");
    assert_eq!(params.len(), 2, "drops implicit $self_in");
    assert!(matches!(&params[0].label, ParameterLabel::Simple(s) if s == "$msg"));
    assert!(matches!(&params[1].label, ParameterLabel::Simple(s) if s == "$when"));
}

/// Multiple `->on('ready', sub {...})` wire-ups stack — each becomes a
/// separate SignatureInformation entry, so users see every handler
/// shape they might be dispatching to.
#[test]
fn sig_help_stacks_multiple_handler_defs() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub new {
    my $self = bless {}, shift;
    $self->on('tick', sub {
        my ($self_in, $count) = @_;
    });
    $self->on('tick', sub {
        my ($self_in, $count, $unit) = @_;
    });
    $self;
}

sub go {
    my $self = shift;
    $self->emit('tick', )
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let pos = {
        let line_idx = src
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("->emit('tick'"))
            .map(|(i, _)| i)
            .unwrap();
        let line = src.lines().nth(line_idx).unwrap();
        let col = line.find(", )").unwrap() + 2;
        Position {
            line: line_idx as u32,
            character: col as u32,
        }
    };

    let sig = signature_help(&analysis, &tree, src, pos, &idx).expect("sig help should fire");
    assert_eq!(
        sig.signatures.len(),
        2,
        "stacked handlers: one signature per ->on call"
    );

    let labels: Vec<&str> = sig.signatures.iter().map(|s| s.label.as_str()).collect();
    assert!(
        labels.iter().all(|l| l.starts_with("emit('tick'")),
        "every signature uses emit('tick', ...) call shape: {:?}",
        labels
    );
}

/// Baseline before the user started typing: cursor in the empty
/// second-arg slot `$self->emit('connect', CURSOR );`. Sig help
/// should offer handler params from the moment the comma is typed.
#[test]
fn sig_help_fires_in_empty_second_slot() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    $self->on('connect', sub {
        my ($self_in, $sock, $remote_ip) = @_;
    });
}

sub fire {
    my $self = shift;
    $self->emit('connect', );
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit('connect'"))
        .unwrap();
    let col = line.find(", )").unwrap() + 2; // just after `, `
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let sig = signature_help(&analysis, &tree, src, pos, &idx)
        .expect("empty arg slot after comma should offer handler sig");
    let s = &sig.signatures[0];
    assert!(
        s.label.starts_with("emit('connect'"),
        "baseline: label identifies emit handler call: {}",
        s.label
    );
}

/// Flow gap fix: `my $dynamic = 'connect'; $self->emit($dynamic, ...)`
/// — hover already worked (DispatchCall.target_name is const-folded
/// by the plugin) but sig help used to miss because it parsed the
/// first arg from text ($dynamic → not a literal). Now sig help
/// routes through the DispatchCall ref too, so const folding
/// composes uniformly and this class of gap can't reopen.
#[test]
fn sig_help_follows_const_folding_like_hover_does() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    $self->on('connect', sub {
        my ($self_in, $sock, $remote_ip) = @_;
    });
}

sub fire {
    my $self = shift;
    my $dynamic = 'connect';
    $self->emit($dynamic, 'hi', );
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit($dynamic"))
        .unwrap();
    let col = line.find(", )").unwrap() + 2;
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let sig = signature_help(&analysis, &tree, src, pos, &idx)
        .expect("sig help must follow const folding like hover does");
    let s = &sig.signatures[0];
    assert!(
        s.label.starts_with("emit('connect'"),
        "const-folded: $dynamic → 'connect' → emit('connect', ...) label; got: {}",
        s.label
    );
    let params = s.parameters.as_ref().unwrap();
    assert_eq!(
        params.len(),
        2,
        "$sock, $remote_ip (implicit $self_in dropped)"
    );
}

/// Regression: cursor inside the SECOND literal-string arg of a
/// dispatch call — matches the user's screenshot where
/// `$self->emit('connect', 'soc' )` had the cursor mid-'soc'. The
/// string-dispatch sig should still fire (first arg is 'connect',
/// handler is registered, active_param is 1).
#[test]
fn sig_help_fires_from_inside_second_string_arg() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    $self->on('connect', sub {
        my ($self_in, $sock, $remote_ip) = @_;
    });
}

sub fire {
    my $self = shift;
    $self->emit('connect', 'soc' );
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit('connect', 'soc'"))
        .unwrap();
    // Cursor at column index pointing into the middle of `'soc'`.
    let col = line.find("'soc'").unwrap() + 2; // between 's' and 'o'
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let sig = signature_help(&analysis, &tree, src, pos, &idx)
        .expect("cursor in 2nd string arg should still surface handler sig");
    let s = &sig.signatures[0];
    assert!(
        s.label.starts_with("emit('connect'"),
        "label should still be the emit(handler) form: {}",
        s.label
    );
    let params = s.parameters.as_ref().unwrap();
    assert_eq!(params.len(), 2, "handler params ($sock, $remote_ip)");
}

/// Completion at the first arg of a dispatch call should list every
/// registered Handler on the receiver's class — top priority, quoted
/// insert, handler params in detail. Same abstraction as hover +
/// sig help, so new plugins don't have to wire this up separately.
#[test]
fn completion_offers_handler_names_at_dispatch_arg0() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    $self->on('connect', sub { my ($s, $sock, $ip) = @_; });
    $self->on('disconnect', sub { my ($s) = @_; });
}

sub fire {
    my $self = shift;
    $self->emit();
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    // Cursor inside the empty `()` of `$self->emit()`.
    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit()"))
        .unwrap();
    let col = line.find("emit(").unwrap() + "emit(".len();
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);

    // Every registered handler shows up as a top-priority suggestion.
    let connect = items
        .iter()
        .find(|i| i.label == "connect")
        .expect("connect handler should be offered at emit arg-0");
    let disconnect = items
        .iter()
        .find(|i| i.label == "disconnect")
        .expect("disconnect handler should be offered at emit arg-0");

    assert_eq!(
        connect.kind,
        Some(CompletionItemKind::EVENT),
        "handler completion kind is EVENT (matches outline)"
    );
    assert_eq!(
        connect.insert_text.as_deref(),
        Some("'connect'"),
        "insert should include quotes so the user doesn't type them"
    );
    assert!(
        connect
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("My::Emitter"),
        "detail should name the owning class: {:?}",
        connect.detail
    );
    assert!(
        connect.detail.as_deref().unwrap_or("").contains("$sock"),
        "detail should expose handler params: {:?}",
        connect.detail
    );

    // Sort text puts handlers ahead of other general completions.
    // Space prefix sorts lex-before any digit-prefixed sort_text,
    // guaranteeing handlers as a top block even when surrounding
    // items (local subs at PRIORITY_LOCAL=0) tie on numeric priority.
    assert!(
        connect
            .sort_text
            .as_deref()
            .unwrap_or("zzz")
            .starts_with(' '),
        "handler sort should lead with space to outrank digit-prefixed sort_text: {:?}",
        connect.sort_text
    );
    assert!(disconnect
        .sort_text
        .as_deref()
        .unwrap_or("zzz")
        .starts_with(' '));
}

/// Completion peels an `Optional<Foo>` receiver to offer `Foo`'s methods,
/// even though the same receiver does NOT dispatch (hover/goto correctly
/// refuse an unguarded optional). Completion is suggestive — the author
/// may not have written the `defined` guard yet.
#[test]
fn completion_peels_optional_receiver() {
    let src = r#"package Foo;
sub go { my ($self) = @_; }
sub spin { my ($self) = @_; }
package P;
sub maybe { return undef unless 1; return Foo->new; }
sub use_it {
    my $r = maybe();
    $r->
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.trim() == "$r->")
        .unwrap();
    let col = line.find("$r->").unwrap() + "$r->".len();
    let pos = Position { line: line_idx as u32, character: col as u32 };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    assert!(
        items.iter().any(|i| i.label == "go"),
        "Optional<Foo> receiver should offer Foo's methods (peeled): {:?}",
        items.iter().map(|i| &i.label).collect::<Vec<_>>(),
    );
    assert!(items.iter().any(|i| i.label == "spin"));
}

/// Bug B: dispatch-target items set their `insert_text` to
/// `'name'` (quoted) but left `filter_text` unset — some LSP
/// clients fall back to `insert_text` for client-side prefix
/// matching, so typing `c` after `(` fails to match `'connect'`
/// (prefix starts with `'`, not `c`). `filter_text` now pins
/// client-side matching to the bare label regardless of insert
/// shape; typing a character keeps the handler visible.
#[test]
fn completion_dispatch_filter_text_matches_bare_name() {
    let src = r#"
package My::Emitter;
use parent 'Mojo::EventEmitter';
sub wire { my $self = shift; $self->on('connect', sub {}); }
sub fire { my $self = shift; $self->emit(); }
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit()"))
        .unwrap();
    let col = line.find("emit(").unwrap() + "emit(".len();
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    let connect = items
        .iter()
        .find(|i| i.label == "connect")
        .expect("connect handler offered");

    // filter_text is the bare name — the client can prefix-match on
    // `c`/`co`/`con`/... even though insert_text is `'connect'`.
    assert_eq!(
        connect.filter_text.as_deref(),
        Some("connect"),
        "filter_text must be the bare label, not the quoted insert_text"
    );
    assert_eq!(
        connect.insert_text.as_deref(),
        Some("'connect'"),
        "insert_text still quotes for the bare-parens case"
    );
}

/// Bug: dispatch-target completion always wrapped the label in
/// quotes — so if the cursor was already inside `''`, accepting
/// `connect` inserted `''connect''`. Now detects the string
/// context via the tree and emits bare text.
#[test]
fn completion_dispatch_inside_quotes_does_not_double_quote() {
    let src = r#"
package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    $self->on('connect', sub { my ($s) = @_; });
}

sub fire {
    my $self = shift;
    $self->emit('');
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    // Cursor BETWEEN the two quotes in `->emit('')`.
    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit('')"))
        .unwrap();
    let col = line.find("('").unwrap() + 2;
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    let connect = items
        .iter()
        .find(|i| i.label == "connect")
        .expect("connect handler offered inside '|'");
    // Cursor inside a string arg → item ships a textEdit pinned
    // to the string-content span so the client's word-at-cursor
    // heuristic can't drop it over non-identifier chars. The
    // newText is the BARE handler name (no wrapping quotes) and
    // insert_text is cleared — textEdit takes precedence in the
    // LSP spec, and leaving insert_text alongside confuses some
    // clients. The original "don't double-quote" invariant now
    // reads off textEdit.newText instead of insert_text.
    assert_eq!(
        connect.insert_text, None,
        "cursor is inside quotes; insert_text is cleared in favor of textEdit"
    );
    use tower_lsp::lsp_types::CompletionTextEdit;
    let Some(CompletionTextEdit::Edit(ref te)) = connect.text_edit else {
        panic!(
            "expected a TextEdit for mid-string dispatch item; got {:?}",
            connect.text_edit
        );
    };
    assert_eq!(
        te.new_text, "connect",
        "textEdit.newText is the bare label — not `'connect'` (would double-quote inside '|')"
    );
}

/// Red pin (user-reported): dispatch-target completions for labels
/// containing non-identifier chars (`/`, `#`) died client-side
/// because nvim's word-at-cursor heuristic uses `iskeyword`, which
/// excludes `/` and `#` by default. The server returned the item
/// with `filter_text = "/users/profile"` but the client extracted
/// `users` or `profile` (a word run starting/ending at the non-
/// keyword boundary) and dropped the item since neither is a
/// prefix of `/users/profile`. Same shape for `Users#list` — the
/// cursor parked past the `#` gave word `list`, which fails
/// `"Users#list".starts_with("list")`.
///
/// Fix: emit `textEdit` with `range = string_content_span_at(...)`
/// so the client filters by the whole in-range text against the
/// full label — regardless of keyword class. This pin locks that
/// textEdit emission for BOTH flavors; regressing either re-
/// surfaces the bug for any route with a URL path or `Ctrl#act`
/// handler name.
#[test]
fn completion_dispatch_textedit_handles_non_keyword_labels() {
    use crate::index::module_index::ModuleIndex;
    use tower_lsp::lsp_types::CompletionTextEdit;

    // Route declarations: one URL path (leading `/`), one
    // `Ctrl#act` (embedded `#`). Both must survive mid-string
    // completion inside `url_for('...')`.
    let app_src = r#"package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list');

get '/users/profile' => sub { my ($c) = @_; };
"#;
    let app_fa = std::sync::Arc::new(crate::build::builder::build(
        &{
            let mut p = tree_sitter::Parser::new();
            p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
            p.parse(app_src, None).unwrap()
        },
        app_src.as_bytes(),
    ));

    let idx = std::sync::Arc::new(ModuleIndex::new_for_test());
    idx.register_workspace_module(std::path::PathBuf::from("/tmp/app.pl"), app_fa);

    let ctrl_src = r#"package Users;
use parent 'Mojolicious::Controller';

sub list {
    my ($c) = @_;
    $c->url_for('/users/profile');
}
"#;
    let ctrl_fa = crate::build::builder::build(
        &{
            let mut p = tree_sitter::Parser::new();
            p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
            p.parse(ctrl_src, None).unwrap()
        },
        ctrl_src.as_bytes(),
    );

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ctrl_src, None).unwrap();

    // Cursor deep inside the path, past the first `/` —
    // `'/users/pr|ofile'`. Before the fix, nvim would extract
    // `users` or `profile` from the chars around the cursor;
    // neither is a prefix of the `/users/profile` label.
    let line_idx = 5u32; // `    $c->url_for('/users/profile');`
    let line = ctrl_src.lines().nth(line_idx as usize).unwrap();
    let quote_start = line.find("'/users/profile").unwrap();
    let pr_col = (quote_start + 1 + "/users/pr".len()) as u32;
    let pos = Position {
        line: line_idx,
        character: pr_col,
    };

    let items = completion_items_for_test(&ctrl_fa, &tree, ctrl_src, pos, &idx, None);

    let path_item = items
        .iter()
        .find(|i| i.label == "/users/profile")
        .expect("/users/profile must be offered (dispatch completion inside string)");

    // insert_text is cleared; textEdit carries the range spanning
    // the entire string content, so the client uses `/users/pr...`
    // as the filter input and matches against the full label.
    assert_eq!(
        path_item.insert_text, None,
        "insert_text cleared — textEdit takes precedence for non-keyword-char labels"
    );
    let Some(CompletionTextEdit::Edit(ref te)) = path_item.text_edit else {
        panic!(
            "expected textEdit for `/users/profile`; got {:?}",
            path_item.text_edit
        );
    };
    assert_eq!(
        te.new_text, "/users/profile",
        "textEdit.newText is the bare label, no surrounding quotes"
    );
    // Range must span the string CONTENT (between the quotes).
    // Start column = col of first `/` (just after opening quote),
    // end column = col of closing quote.
    assert_eq!(te.range.start.line, line_idx);
    assert_eq!(te.range.end.line, line_idx);
    assert_eq!(
        te.range.start.character,
        (quote_start + 1) as u32,
        "range start hugs the char just after the opening quote",
    );
    assert_eq!(
        te.range.end.character,
        (quote_start + 1 + "/users/profile".len()) as u32,
        "range end hugs the closing quote — replacement stays INSIDE the existing quotes",
    );

    // Same check for the `Ctrl#act` flavor — cursor past the `#`.
    let ctrl_src_hash = r#"package Users;
use parent 'Mojolicious::Controller';

sub list {
    my ($c) = @_;
    $c->url_for('Users#list');
}
"#;
    let ctrl_fa_hash = crate::build::builder::build(
        &parser.parse(ctrl_src_hash, None).unwrap(),
        ctrl_src_hash.as_bytes(),
    );
    let tree_hash = parser.parse(ctrl_src_hash, None).unwrap();
    let line = ctrl_src_hash.lines().nth(5).unwrap();
    let quote_start = line.find("'Users#list").unwrap();
    let past_hash_col = (quote_start + 1 + "Users#li".len()) as u32;
    let pos = Position {
        line: 5,
        character: past_hash_col,
    };
    let items = completion_items_for_test(&ctrl_fa_hash, &tree_hash, ctrl_src_hash, pos, &idx, None);
    let hash_item = items
        .iter()
        .find(|i| i.label == "Users#list")
        .expect("Users#list must be offered when cursor is past the #");
    assert_eq!(hash_item.insert_text, None);
    let Some(CompletionTextEdit::Edit(ref te)) = hash_item.text_edit else {
        panic!(
            "expected textEdit for `Users#list`; got {:?}",
            hash_item.text_edit
        );
    };
    assert_eq!(te.new_text, "Users#list");
}

/// Red pin (user QA): accepting a dispatch completion APPENDED
/// the label instead of replacing the typed text — `url_for('/fall|')`
/// accepting `/fallback` yielded `url_for('/fall/fallback')`.
/// Root cause: `descendant_for_point_range` returns the enclosing
/// `string_literal` (not `string_content`) when the cursor sits
/// at the content's end boundary, because content ranges are
/// half-open. `string_content_span_at` then fell into the
/// zero-width "empty literal" branch and returned `(cursor,
/// cursor)` — textEdit replacing nothing = append. Fix descends
/// into the literal to find a `string_content` child before
/// giving up.
///
/// This pin covers three cursor positions, each of which previously
/// hit the wrapper-instead-of-content path:
///   1. INSIDE the content (baseline — already worked).
///   2. AT the content's end boundary (just before closing quote).
///   3. ON the closing quote itself.
/// All three must return a textEdit range covering the full
/// `string_content` span, so accepting the completion replaces
/// the typed prefix with the label cleanly.
#[test]
fn completion_dispatch_textedit_range_at_content_boundary() {
    use crate::index::module_index::ModuleIndex;
    use tower_lsp::lsp_types::CompletionTextEdit;

    let app_src = r#"package MyApp;
use Mojolicious::Lite;

any '/fallback' => sub { my ($c) = @_; };
"#;
    let app_fa = std::sync::Arc::new(crate::build::builder::build(
        &{
            let mut p = tree_sitter::Parser::new();
            p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
            p.parse(app_src, None).unwrap()
        },
        app_src.as_bytes(),
    ));

    let idx = std::sync::Arc::new(ModuleIndex::new_for_test());
    idx.register_workspace_module(std::path::PathBuf::from("/tmp/app.pl"), app_fa);

    let ctrl_src = r#"package Users;
use parent 'Mojolicious::Controller';

sub list {
    my ($c) = @_;
    $c->url_for('/fall');
}
"#;
    let ctrl_fa = crate::build::builder::build(
        &{
            let mut p = tree_sitter::Parser::new();
            p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
            p.parse(ctrl_src, None).unwrap()
        },
        ctrl_src.as_bytes(),
    );
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ctrl_src, None).unwrap();

    // `    $c->url_for('/fall');`
    let line_idx = 5u32;
    let line = ctrl_src.lines().nth(line_idx as usize).unwrap();
    let quote_start = line.find("'/fall'").unwrap();
    let content_start = (quote_start + 1) as u32; // `/`
    let content_end = content_start + "/fall".len() as u32; // just after `l`
    let closing_quote_col = content_end; // the `'`

    // Three cursor positions to exercise: inside the content,
    // at the content's end boundary, and on the closing quote.
    let cursor_variants = [
        ("inside content", content_start + 3), // between `a` and `l`
        ("end of content", content_end),       // just after `l`, before `'`
        ("on closing quote", closing_quote_col),
    ];

    for (label, col) in cursor_variants {
        let items = completion_items_for_test(
            &ctrl_fa,
            &tree,
            ctrl_src,
            Position {
                line: line_idx,
                character: col,
            },
            &idx,
            None,
        );
        let item = items
            .iter()
            .find(|i| i.label == "/fallback")
            .unwrap_or_else(|| {
                panic!(
                    "{}: /fallback must be offered at col {}; \
                                           got labels: {:?}",
                    label,
                    col,
                    items.iter().map(|i| &i.label).collect::<Vec<_>>()
                )
            });

        let Some(CompletionTextEdit::Edit(ref te)) = item.text_edit else {
            panic!("{}: expected textEdit; got {:?}", label, item.text_edit);
        };
        // Range must cover the FULL typed content, not zero-width.
        // That way accepting `/fallback` REPLACES `/fall`, not
        // appends to it — the pre-fix failure mode that produced
        // `'/fall/fallback'`.
        assert_eq!(
            te.range.start.character, content_start,
            "{}: range start must hug the first content char; got range {:?}",
            label, te.range,
        );
        assert_eq!(
            te.range.end.character, content_end,
            "{}: range end must hug the closing quote (exclusive of it); got range {:?}",
            label, te.range,
        );
        assert_eq!(
            te.new_text, "/fallback",
            "{}: newText is the bare label — no seasonal redundancy",
            label,
        );
    }
}

/// Bug: typing `,` inside a known dispatch call (`->emit('x', |)`)
/// triggered completion which ran the global sub/module firehose —
/// useless here. Now suppresses imported/unimported function
/// completions when we're inside a known dispatcher call; sig
/// help remains the right affordance for guiding arg shape.
#[test]
fn completion_after_comma_in_dispatch_call_suppresses_firehose() {
    let src = r#"
package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire_one {}
sub wire_two {}
sub completely_unrelated {}

sub wire {
    my $self = shift;
    $self->on('connect', sub { my ($s, $sock) = @_; });
}

sub fire {
    my $self = shift;
    $self->emit('connect', );
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit('connect',"))
        .unwrap();
    let col = line.find(", )").unwrap() + 2;
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    assert!(
        !labels.contains(&"completely_unrelated"),
        "unrelated sub must not appear in dispatch arg completion: {:?}",
        labels
    );
    assert!(
        !labels.contains(&"wire_one"),
        "wire_one leak — dispatch arg completion should stay quiet: {:?}",
        labels
    );
}

/// Mid-string completion for route targets. Cursor inside
/// `->to('Users#lis|')` offers methods on Users, prefix-filtered
/// by `lis`. Generic for ANY plugin that emits MethodCallRef at
/// a string span (routes today, Catalyst forwards, etc.).
#[test]
fn completion_mid_string_route_target_scoped_to_invocant() {
    // Same-file Users package so the test is self-contained. Real
    // use would have Users in a separate file via workspace index;
    // the lookup path is the same (complete_methods_for_class
    // walks inheritance + module index).
    let src = r#"
package Users;
sub list {}
sub login {}
sub logout {}
sub delete_user {}

package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#lis');
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    // Cursor just after 'lis' in 'Users#lis' — active editing state.
    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("Users#lis"))
        .unwrap();
    let col = line.find("Users#lis").unwrap() + "Users#lis".len();
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    // Prefix-filtered: only `list` starts with `lis`; `login`,
    // `logout`, `delete_user` don't.
    assert!(
        labels.contains(&"list"),
        "list must be offered for prefix `lis`: {:?}",
        labels
    );
    assert!(
        !labels.contains(&"login"),
        "login does NOT start with `lis` — must be filtered out: {:?}",
        labels
    );
    assert!(
        !labels.contains(&"logout"),
        "logout does NOT start with `lis` — must be filtered out: {:?}",
        labels
    );
    assert!(
        !labels.contains(&"delete_user"),
        "delete_user is unrelated — must not appear: {:?}",
        labels
    );

    // Top-priority sort — the mid-string completion path is the
    // only sensible one at this cursor position.
    let list = items.iter().find(|i| i.label == "list").unwrap();
    assert!(
        list.sort_text
            .as_deref()
            .unwrap_or("zzz")
            .starts_with("000"),
        "mid-string method completion should be top-priority: {:?}",
        list.sort_text
    );
}

/// Mid-string completion for routes before `#` is typed — cursor at
/// `->to('Us|')`. The invocant portion isn't complete yet, so the
/// plugin won't have emitted a MethodCallRef. Graceful fallthrough
/// to general completion (or nothing) is the expected behavior.
#[test]
fn completion_mid_string_before_hash_falls_through() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Us');
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("'Us'"))
        .unwrap();
    let col = line.find("'Us'").unwrap() + "'Us".len();
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    // Doesn't assert what IS offered — just that it doesn't panic
    // and doesn't return complete nonsense. This is the honest
    // edge-case: without a `#` yet, no plugin-emitted ref exists.
    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    let _ = items;
}

/// Completion skips when the method isn't a declared dispatcher, even
/// if handlers exist on the class. (Empty dispatchers == "any" by
/// convention, but mojo-events declares ["emit"] specifically.)
#[test]
fn completion_skips_non_dispatcher_method() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    $self->on('connect', sub { my ($s) = @_; });
}

sub other {
    my $self = shift;
    $self->unrelated_method();
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->unrelated_method()"))
        .unwrap();
    let col = line.find("method(").unwrap() + "method(".len();
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    // `connect` may still appear as the socket BUILTIN (the identifier
    // universe's BUILTIN tier) — what must NOT appear is the registered
    // handler item for it.
    assert!(
        !items
            .iter()
            .any(|i| i.label == "connect" && i.kind == Some(CompletionItemKind::EVENT)),
        "non-dispatcher method must not surface handler completions"
    );
}

/// No handler params means no specialized sig help — fall through to
/// the regular method-signature path (or return None if ->emit isn't
/// locally defined, as in this test).
#[test]
fn sig_help_returns_none_when_no_handler_registered() {
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';

sub fire {
    my $self = shift;
    $self->emit('never_registered', )
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let pos = {
        let line_idx = src
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("never_registered"))
            .map(|(i, _)| i)
            .unwrap();
        let line = src.lines().nth(line_idx).unwrap();
        let col = line.find(", )").unwrap() + 2;
        Position {
            line: line_idx as u32,
            character: col as u32,
        }
    };

    let sig = signature_help(&analysis, &tree, src, pos, &idx);
    assert!(
        sig.is_none(),
        "no handler_params → no string-dispatch sig; also no local ->emit def"
    );
}

/// The past-arg-0 twin of the suppression test above: the firehose goes
/// quiet AND what remains is the variables in scope — the candidates
/// you'd pass as the next argument — with insert text that carries the
/// sigil. The suppression half held from day one and was pinned; the
/// insert half was not: variable candidates mint `insert_text` for the
/// sigil-typed lane (bare name — the client's buffer already holds the
/// `$` and word-replaces after it), and the bare-cursor lane consumed
/// them verbatim, so accepting `$self` here inserted sigil-less `self`.
/// Invisible in the menu (label and filter_text carry the sigil), wrong
/// only at the moment of insertion.
#[test]
fn completion_after_comma_in_dispatch_call_offers_scope_variables() {
    let src = r#"
package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    $self->on('connect', sub { my ($s, $sock) = @_; });
}

sub fire {
    my $self = shift;
    my $payload = { id => 1 };
    $self->emit('connect', );
}
"#;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();

    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("->emit('connect',"))
        .unwrap();
    let col = line.find(", )").unwrap() + 2;
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = completion_items_for_test(&analysis, &tree, src, pos, &idx, None);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    assert!(
        labels.contains(&"$self") && labels.contains(&"$payload"),
        "scope variables should be offered past arg-0 in a dispatch call: {:?}",
        labels
    );
    assert!(
        !labels.contains(&"wire"),
        "firehose stays suppressed while variables are offered: {:?}",
        labels
    );
    // The insertion half — what the buffer receives at a BARE cursor
    // must carry the sigil (no typed sigil exists for the client to keep).
    for want in ["$self", "$payload"] {
        let it = items.iter().find(|i| i.label == want).unwrap();
        assert_eq!(
            it.insert_text.as_deref().unwrap_or(&it.label),
            want,
            "bare-cursor insert for {want} must include the sigil"
        );
    }
}

/// The keys a call may pass come from the callable the call runs, through
/// the one call → callee binding: a class's `has` attributes for a
/// constructor nothing in the file declares, a `:$name` signature's
/// named parameters for a sub.
#[test]
fn a_call_key_completes_from_its_callee() {
    let src = r#"package Point;
use Moo;
has x => (is => 'ro');
has y => (is => 'ro');
sub move ($self, :$dx, :$dy = 0) { }
package main;
my $p = Point->new(x => 1, );
$p->move();
"#;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();
    let labels = |needle: &str| -> Vec<String> {
        let (row, line) = src.lines().enumerate().find(|(_, l)| l.contains(needle)).unwrap();
        let pos = Position { line: row as u32, character: (line.find(needle).unwrap() + needle.len()) as u32 };
        let mut out: Vec<String> = completion_items_for_test(&analysis, &tree, src, pos, &idx, None)
            .into_iter()
            .map(|i| i.label)
            .filter(|l| l.ends_with(" =>"))
            .collect();
        out.sort();
        out
    };
    assert_eq!(labels("x => 1, "), ["y =>"], "the attrs not yet passed");
    assert_eq!(labels("move("), ["dx =>", "dy =>"]);
}

/// The labels offered where `marker` stands in `src` (the marker removed).
fn labels_where(src_with_marker: &str, marker: &str) -> Vec<String> {
    let at = src_with_marker.find(marker).expect("marker");
    let src = src_with_marker.replacen(marker, "", 1);
    let before = &src[..at];
    let line = before.matches('\n').count() as u32;
    let character = (at - before.rfind('\n').map_or(0, |i| i + 1)) as u32;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(&src, None).unwrap();
    let analysis = crate::build::builder::build(&tree, src.as_bytes());
    let idx = ModuleIndex::new_for_test();
    completion_items_for_test(&analysis, &tree, &src, Position { line, character }, &idx, None)
        .into_iter()
        // A label is written with its separator (`retries =>`).
        .map(|i| i.label.split_whitespace().next().unwrap_or_default().to_string())
        .collect()
}

/// The arguments a dispatch hands its handler complete the handler's keys:
/// the names inside `enqueue`'s array, the keys of a hash at a place.
#[test]
fn a_task_s_keys_complete_inside_the_enqueue_array() {
    let named = labels_where(
        "use v5.36; use Minion;\nmy $minion = Minion->new;\n\
$minion->add_task(mail => sub ($job, %o) { $o{timeout} + $o{retries} });\n\
$minion->enqueue(mail => [timeout => 1, ^]);\n",
        "^",
    );
    assert!(named.iter().any(|l| l == "retries"), "the task's names, got {named:?}");
    assert!(!named.iter().any(|l| l == "timeout"), "a written name is not offered again, got {named:?}");

    let hashed = labels_where(
        "use v5.36; use Minion;\nmy $minion = Minion->new;\n\
$minion->add_task(mail => sub ($job, $o) { $o->{timeout} });\n\
$minion->enqueue(mail => [{^}]);\n",
        "^",
    );
    assert!(hashed.iter().any(|l| l == "timeout"), "the hash parameter's keys, got {hashed:?}");
}

/// A call's keys after the places its callee takes first complete the
/// names the callee collects, and a hash written at a place completes
/// that parameter's keys.
#[test]
fn keys_complete_by_place() {
    let after = labels_where(
        "use strict;\nsub f { my ($x, %a) = @_; $a{timeout} + $a{retries} }\nf(1, timeout => 1, ^);\n",
        "^",
    );
    assert!(after.iter().any(|l| l == "retries"), "names after the place, got {after:?}");

    let at = labels_where(
        "use strict;\nsub f { my ($x, $o) = @_; $o->{timeout} }\nf(1, {^});\n",
        "^",
    );
    assert!(at.iter().any(|l| l == "timeout"), "the second argument's keys, got {at:?}");
}
