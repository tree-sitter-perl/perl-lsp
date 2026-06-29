// cpp-attributes: C++ declarator-position attribute/export macros and what
// they SIGNAL.
//
// `class Q_CORE_EXPORT Foo {...}` parses as a corrupt function — the export
// macro sits where the parser expects the type name, so the class evaporates.
// Core's `cpp_reparse::strip_declarator_macros` blanks any such macro
// (length-preserving) so the class re-parses; that heuristic RECOVERS the
// class behind ANY unknown macro but discards what the macro meant. This
// manifest restores the meaning: the analyze path looks the blanked token up
// here and, when known, stamps the signal onto the recovered class symbol
// (`Symbol.attributes`) — surfaced in hover. Unknown macros still recover via
// the heuristic; they just carry no signal.
//
// The vocabulary is OPEN. This is a representative Qt starter set — visibility
// macros (`Q_*_EXPORT`) signal "exported", deprecation macros signal
// "deprecated". A house codebase ships its own macros via a `.perl-lsp/` plugin
// declaring the same `attribute_macros()` shape; core unions them all.

fn id() { "cpp-attributes" }

// No emit/query hooks — this plugin is purely a declarative manifest, read
// trigger-independently like overrides/dispatch_verbs. So no triggers fire.
fn triggers() { [] }

fn attribute_macros() {
    let exported = ["Q_CORE_EXPORT", "Q_GUI_EXPORT", "Q_WIDGETS_EXPORT",
                    "Q_NETWORK_EXPORT", "Q_QML_EXPORT", "Q_DECL_EXPORT",
                    "Q_DECL_IMPORT"];
    let deprecated = ["Q_DEPRECATED", "QT_DEPRECATED", "Q_DECL_DEPRECATED"];
    let out = [];
    for m in exported {
        out += #{ name: m, signal: "exported" };
    }
    for m in deprecated {
        out += #{ name: m, signal: "deprecated" };
    }
    out
}
