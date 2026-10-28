// House plugin for FHEM (pinned in corpus/bootstrap.sh).
//
// fhem.pl declares the server's globals with `use vars` in package main,
// then `do`-loads every FHEM/*.pm into the same stash; the modules
// (`package main;` nearly all of them) use %defs, %attr, %modules, ...
// without declaring them, as Perl allows once fhem.pl has run. The plugin
// declares them wherever a main-package file turns strict on, the only
// place the lint can fire.
//
// Modules with a package of their own reach the same globals through
// GPUtils' `GP_Import(qw(defs attr ...))`, which aliases each `*main::NAME`
// glob into the caller: an Import of those globs.
//
// The declarations sit on the file's own `use strict` (or GP_Import call), so goto-def lands
// there rather than in fhem.pl: cross-file package globals are a known
// engine gap, and this is the house's stand-in for them.

fn id() { "house-fhem-globals" }

fn triggers() { [ #{ UsesModule: "strict" }, #{ UsesModule: "GPUtils" } ] }

// fhem.pl's `use vars` names, in its order.
fn globals() {
    [
        "$addTimerStacktrace", "$auth_refresh", "$cmdFromAnalyze",
        "$devcount", "$devcountPrioSave", "$devcountTemp", "$featurelevel",
        "$fhemForked", "$fhemTestFile", "$fhem_started", "$haveInet6",
        "$init_done", "$internal_data", "$jsonSkipNameConversion",
        "$lastDefChange", "$lastWarningMsg", "$nextat", "$numCPUs",
        "$reread_active", "$selectTimestamp", "$tmpdevcount",
        "$unicodeEncoding", "$winService", "%attr", "%cmds", "%data",
        "%defaultattr", "%defs", "%inform", "%intAt", "%logInform",
        "%modules", "%ntfyHash", "%prioQueues", "%readyfnlist",
        "%selectlist", "%value", "@intAtA", "@structChangeHist",
        "$readingFnAttributes",
    ]
}

fn patterns() {
    [
        #{
            name: "gp_import",
            query: `
([
  (function_call_expression function: (_) @verb arguments: (_) @names)
  (ambiguous_function_call_expression function: (_) @verb arguments: (_) @names)
]
(#eq? @verb "GP_Import"))
`,
            projections: #{ names: ["list"] },
            expect: [
                #{ src: "BEGIN { GP_Import(qw(defs modules)); }", matches: 1 },
                #{ src: "GP_Import('defs', 'attr');", matches: 1 },
                #{ src: "GP_Export(qw(Define));", matches: 0 },
            ],
        }
    ]
}

fn on_match(pattern, m) {
    if pattern != "gp_import" { return []; }
    let names = m.captures.names.list;
    if names == () || names.len() == 0 { return []; }
    [
        #{
            Import: #{
                module_name: "main",
                // Each `list` entry is [word, span].
                imported_symbols: names.map(|n| #{ local_name: "*" + n[0] }),
                span: m.span,
            }
        }
    ]
}

fn on_use(ctx) {
    if ctx.module_name != "strict" || ctx.negated { return []; }
    let pkg = ctx.current_package;
    if pkg != () && pkg != "main" { return []; }
    globals().map(|n| #{ PackageVariable: #{ name: n, span: ctx.span } })
}
