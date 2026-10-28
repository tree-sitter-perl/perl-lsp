// log4perl: Log::Log4perl's `import` builds its exports at runtime from
// tags, so the static export surface is empty. The plugin states what each
// tag installs in the caller (Log/Log4perl.pm `import`, 1.57):
//
//   :levels     $OFF $FATAL ... $ALL, from %Log::Log4perl::Level::PRIORITY
//   get_logger  the function
//   :easy       both of the above, plus the stealth loggers DEBUG, LOGDIE, ...
//
// `use Log::Log4perl::Level` with no namespace argument installs the level
// scalars and their `*_INT` functions in the caller; with one it installs
// them elsewhere, so the plugin stays silent.

fn id() { "log4perl" }

fn triggers() { [ #{ UsesModule: "Log::Log4perl" }, #{ UsesModule: "Log::Log4perl::Level" } ] }

fn levels() { ["OFF", "FATAL", "ERROR", "WARN", "INFO", "DEBUG", "TRACE", "ALL"] }

fn easy_loggers() {
    [
        "TRACE", "DEBUG", "INFO", "WARN", "ERROR", "FATAL", "ALWAYS",
        "LOGCROAK", "LOGCLUCK", "LOGCARP", "LOGCONFESS",
        "LOGDIE", "LOGEXIT", "LOGWARN",
    ]
}

fn import_of(from, names, span) {
    if names.len() == 0 { return []; }
    [ #{ Import: #{ module_name: from, imported_symbols: names.map(|n| #{ local_name: n }), span: span } } ]
}

fn on_use(ctx) {
    if ctx.negated { return []; }
    if ctx.module_name == "Log::Log4perl::Level" {
        if ctx.imports.len() != 0 { return []; }
        let names = levels().map(|l| "$" + l);
        names += levels().map(|l| l + "_INT");
        return import_of("Log::Log4perl::Level", names, ctx.span);
    }
    if ctx.module_name != "Log::Log4perl" { return []; }
    let easy = ":easy" in ctx.imports;
    let names = [];
    if easy || ":levels" in ctx.imports { names += levels().map(|l| "$" + l); }
    if easy || "get_logger" in ctx.imports { names.push("get_logger"); }
    if easy { names += easy_loggers(); }
    import_of("Log::Log4perl", names, ctx.span)
}
