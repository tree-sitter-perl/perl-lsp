// House plugin for FHEM (pinned in corpus/bootstrap.sh).
//
// fhem.pl declares the server's globals with `use vars` in package main,
// then `do`-loads every FHEM/*.pm into the same stash; the modules
// (`package main;` nearly all of them) use %defs, %attr, %modules, ...
// without declaring them, as Perl allows once fhem.pl has run. The plugin
// declares them wherever a main-package file turns strict on, the only
// place the lint can fire.
//
// The declarations sit on the file's own `use strict`, so goto-def lands
// there rather than in fhem.pl: cross-file package globals are a known
// engine gap, and this is the house's stand-in for them.

fn id() { "house-fhem-globals" }

fn triggers() { [ #{ UsesModule: "strict" } ] }

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

fn on_use(ctx) {
    if ctx.module_name != "strict" || ctx.negated { return []; }
    let pkg = ctx.current_package;
    if pkg != () && pkg != "main" { return []; }
    globals().map(|n| #{ PackageVariable: #{ name: n, span: ctx.span } })
}
