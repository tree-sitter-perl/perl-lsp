// search-roots: the idioms that add a directory to @INC where the
// directory they add is statically known. Each entry names its anchor —
// what a relative path in it is relative to: the process's working
// directory (`working_dir`, the workspace root for a server) or the
// directory of the file that writes it (`file_dir`). A path written
// absolute is absolute whatever the anchor.
//
// - `use`: a module whose import list is roots (`use lib 'x'`).
// - `variable`: a package scalar holding a directory; a root written as
//   that scalar followed by a literal path (`"$FindBin::Bin/../lib"`,
//   `$FindBin::Bin . '/lib'`) is that path under the anchor.
// - `array`: an array that IS the search path; each literal a `verbs`
//   call adds to it (`unshift @INC, 'x'`) is a root.
//
// Anything computed or read from the environment names no root.

fn id() { "search-roots" }

fn triggers() { [] }

fn search_root_makers() {
    [
        #{ kind: "use", name: "lib", anchor: "working_dir" },
        #{ kind: "use", name: "lib::relative", anchor: "file_dir" },
        #{ kind: "variable", name: "FindBin::Bin", anchor: "file_dir" },
        #{ kind: "variable", name: "FindBin::RealBin", anchor: "file_dir" },
        #{ kind: "array", name: "INC", anchor: "working_dir", verbs: ["unshift", "push"] },
    ]
}
