// english: the variables `use English` aliases into the caller.
//
// English's `import` picks `@EXPORT` at runtime (`*EXPORT =
// \@COMPLETE_EXPORT` or `\@MINIMAL_EXPORT`, depending on
// `-no_match_vars`), so the static export surface is empty and every
// `$ERRNO` / `$EVAL_ERROR` would read as undeclared. The plugin states
// the list as an Import of globs: an exported `*NAME` binds every
// sigil's variable of that name, exactly as Exporter does.
//
// An explicit list (`use English qw($ERRNO)`) is already its own native
// Import, so the plugin speaks only for the bare forms.

fn id() { "english" }

fn triggers() { [ #{ UsesModule: "English" } ] }

// English.pm's @MINIMAL_EXPORT, in its order.
fn minimal_export() {
    [
        "*ARG", "*LAST_PAREN_MATCH", "*INPUT_LINE_NUMBER", "*NR",
        "*INPUT_RECORD_SEPARATOR", "*RS", "*OUTPUT_AUTOFLUSH",
        "*OUTPUT_FIELD_SEPARATOR", "*OFS", "*OUTPUT_RECORD_SEPARATOR", "*ORS",
        "*LIST_SEPARATOR", "*SUBSCRIPT_SEPARATOR", "*SUBSEP",
        "*FORMAT_PAGE_NUMBER", "*FORMAT_LINES_PER_PAGE", "*FORMAT_LINES_LEFT",
        "*FORMAT_NAME", "*FORMAT_TOP_NAME", "*FORMAT_LINE_BREAK_CHARACTERS",
        "*FORMAT_FORMFEED", "*CHILD_ERROR", "*OS_ERROR", "*ERRNO",
        "*EXTENDED_OS_ERROR", "*EVAL_ERROR", "*PROCESS_ID", "*PID",
        "*REAL_USER_ID", "*UID", "*EFFECTIVE_USER_ID", "*EUID",
        "*REAL_GROUP_ID", "*GID", "*EFFECTIVE_GROUP_ID", "*EGID",
        "*PROGRAM_NAME", "*PERL_VERSION", "*OLD_PERL_VERSION", "*ACCUMULATOR",
        "*COMPILING", "*DEBUGGING", "*SYSTEM_FD_MAX", "*INPLACE_EDIT",
        "*PERLDB", "*BASETIME", "*WARNING", "*EXECUTABLE_NAME", "*OSNAME",
        "*LAST_REGEXP_CODE_RESULT", "*EXCEPTIONS_BEING_CAUGHT",
        "*LAST_SUBMATCH_RESULT", "@LAST_MATCH_START", "@LAST_MATCH_END",
    ]
}

// @MATCH_EXPORT: left out under `-no_match_vars`.
fn match_export() { ["*MATCH", "*PREMATCH", "*POSTMATCH"] }

fn on_use(ctx) {
    if ctx.module_name != "English" { return []; }
    let no_match_vars = false;
    for i in ctx.imports {
        if i == "-no_match_vars" {
            no_match_vars = true;
        } else {
            return [];
        }
    }
    let names = minimal_export();
    if !no_match_vars { names += match_export(); }
    [
        #{
            Import: #{
                module_name: "English",
                imported_symbols: names.map(|n| #{ local_name: n }),
                span: ctx.span,
            }
        }
    ]
}
