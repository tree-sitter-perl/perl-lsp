//! The Perl builtin surface — the ONE table answering "is this name owned
//! by the Perl language, and as what".
//!
//! Every builtin-aware consumer projects membership from here: the
//! resolution BUILTIN tier sources completion names from `Function` rows,
//! diagnostics suppression asks `is_builtin` (any kind — a keyword or
//! filehandle surfacing as a call ref is still not user code), builtin
//! hover gates its perlfunc-doc lookup on the same membership
//! (`index/builtins_pod.rs` stays the doc-VALUE store, keyed by these
//! names), and the typed signature slots (`builtin_return_type` /
//! `builtin_first_arg_type`) are columns of the same rows — a typed name
//! that isn't a `Function` row cannot exist by construction.
//!
//! Perl-driver-scoped: consumers reach this table only on Perl-stamped
//! analyses (`FileAnalysis.language`) — pack languages never consult it.
//! Pure `&str` like `conventions.rs`; no tree-sitter.
//!
//! Anti-drift tripwire: `builtins_pod_tests.rs` asserts every perlfunc.pod
//! entry name is a row here (modulo its documented prose-noise set), and
//! every `Function` row has a perlfunc doc entry.

use super::file_analysis::InferredType;

/// What kind of name the Perl language owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinKind {
    /// A callable builtin (`print`, `exp`, `fc`) — the BUILTIN resolution
    /// tier's completion source.
    Function,
    /// Core bareword filehandle (`STDIN`, `DATA`) — suppresses
    /// `print DATA` / `STDOUT->autoflush` / `-t STDIN` style false
    /// unresolved hints.
    BarewordFilehandle,
    /// Declarator / flow-control / operator / quote-like keyword (`my`,
    /// `if`, `eq`, `tr`). Suppression-only: never offered as a callable.
    Keyword,
}

use BuiltinKind as K;

// Column shorthands for the table below.
const N: Option<InferredType> = Some(InferredType::Numeric);
const S: Option<InferredType> = Some(InferredType::String);
const B: Option<InferredType> = Some(InferredType::Bool);
const X: Option<InferredType> = None;

/// `(name, kind, return_type, first_arg_type)` — sorted by name (ASCII:
/// uppercase filehandles first), so membership is a binary search. Names
/// come from perlfunc.pod (see the module doc's tripwire). Type columns
/// carry only stable scalar contracts.
#[rustfmt::skip]
static BUILTINS: &[(&str, BuiltinKind, Option<InferredType>, Option<InferredType>)] = &[
    ("ARGV", K::BarewordFilehandle, X, X),
    ("ARGVOUT", K::BarewordFilehandle, X, X),
    ("DATA", K::BarewordFilehandle, X, X),
    ("STDERR", K::BarewordFilehandle, X, X),
    ("STDIN", K::BarewordFilehandle, X, X),
    ("STDOUT", K::BarewordFilehandle, X, X),
    ("abs", K::Function, N, N),
    ("accept", K::Function, X, X),
    ("alarm", K::Function, X, X),
    ("and", K::Keyword, X, X),
    ("atan2", K::Function, X, N),
    ("bind", K::Function, X, X),
    ("binmode", K::Function, X, X),
    ("bless", K::Function, X, X),
    ("break", K::Keyword, X, X),
    ("caller", K::Function, X, X),
    ("catch", K::Keyword, X, X),
    ("chdir", K::Function, X, X),
    ("chmod", K::Function, X, X),
    ("chomp", K::Function, X, S),
    ("chop", K::Function, X, S),
    ("chown", K::Function, X, X),
    ("chr", K::Function, S, N),
    ("chroot", K::Function, X, X),
    ("class", K::Keyword, X, X),
    ("close", K::Function, X, X),
    ("closedir", K::Function, X, X),
    ("cmp", K::Keyword, X, X),
    ("connect", K::Function, X, X),
    ("continue", K::Keyword, X, X),
    ("cos", K::Function, X, N),
    ("crypt", K::Function, S, X),
    ("dbmclose", K::Function, X, X),
    ("dbmopen", K::Function, X, X),
    ("default", K::Keyword, X, X),
    ("defer", K::Keyword, X, X),
    ("defined", K::Function, B, X),
    ("delete", K::Function, X, X),
    ("die", K::Function, X, X),
    ("do", K::Keyword, X, X),
    ("dump", K::Function, X, X),
    ("each", K::Function, X, X),
    ("else", K::Keyword, X, X),
    ("elsif", K::Keyword, X, X),
    ("endgrent", K::Function, X, X),
    ("endhostent", K::Function, X, X),
    ("endnetent", K::Function, X, X),
    ("endprotoent", K::Function, X, X),
    ("endpwent", K::Function, X, X),
    ("endservent", K::Function, X, X),
    ("eof", K::Function, X, X),
    ("eq", K::Keyword, X, X),
    ("eval", K::Function, X, X),
    ("evalbytes", K::Function, X, X),
    ("exec", K::Function, X, X),
    ("exists", K::Function, B, X),
    ("exit", K::Function, X, X),
    ("exp", K::Function, X, N),
    ("fc", K::Function, X, X),
    ("fcntl", K::Function, X, X),
    ("field", K::Keyword, X, X),
    ("fileno", K::Function, N, X),
    ("finally", K::Keyword, X, X),
    ("flock", K::Function, X, X),
    ("for", K::Keyword, X, X),
    ("foreach", K::Keyword, X, X),
    ("fork", K::Function, X, X),
    ("format", K::Keyword, X, X),
    ("formline", K::Function, X, X),
    ("ge", K::Keyword, X, X),
    ("getc", K::Function, X, X),
    ("getgrent", K::Function, X, X),
    ("getgrgid", K::Function, X, X),
    ("getgrnam", K::Function, X, X),
    ("gethostbyaddr", K::Function, X, X),
    ("gethostbyname", K::Function, X, X),
    ("gethostent", K::Function, X, X),
    ("getlogin", K::Function, X, X),
    ("getnetbyaddr", K::Function, X, X),
    ("getnetbyname", K::Function, X, X),
    ("getnetent", K::Function, X, X),
    ("getpeername", K::Function, X, X),
    ("getpgrp", K::Function, X, X),
    ("getppid", K::Function, X, X),
    ("getpriority", K::Function, X, X),
    ("getprotobyname", K::Function, X, X),
    ("getprotobynumber", K::Function, X, X),
    ("getprotoent", K::Function, X, X),
    ("getpwent", K::Function, X, X),
    ("getpwnam", K::Function, X, X),
    ("getpwuid", K::Function, X, X),
    ("getservbyname", K::Function, X, X),
    ("getservbyport", K::Function, X, X),
    ("getservent", K::Function, X, X),
    ("getsockname", K::Function, X, X),
    ("getsockopt", K::Function, X, X),
    ("given", K::Keyword, X, X),
    ("glob", K::Function, X, X),
    ("gmtime", K::Function, X, X),
    ("goto", K::Keyword, X, X),
    ("grep", K::Function, X, X),
    ("gt", K::Keyword, X, X),
    ("hex", K::Function, N, S),
    ("if", K::Keyword, X, X),
    ("import", K::Function, X, X),
    ("index", K::Function, N, S),
    ("int", K::Function, N, N),
    ("ioctl", K::Function, X, X),
    ("isa", K::Keyword, X, X),
    ("join", K::Function, S, X),
    ("keys", K::Function, X, X),
    ("kill", K::Function, X, X),
    ("last", K::Keyword, X, X),
    ("lc", K::Function, S, S),
    ("lcfirst", K::Function, S, S),
    ("le", K::Keyword, X, X),
    ("length", K::Function, N, S),
    ("link", K::Function, X, X),
    ("listen", K::Function, X, X),
    ("local", K::Keyword, X, X),
    ("localtime", K::Function, X, X),
    ("lock", K::Function, X, X),
    ("log", K::Function, X, N),
    ("lstat", K::Function, X, X),
    ("lt", K::Keyword, X, X),
    ("m", K::Keyword, X, X),
    ("map", K::Function, X, X),
    ("method", K::Keyword, X, X),
    ("mkdir", K::Function, X, X),
    ("msgctl", K::Function, X, X),
    ("msgget", K::Function, X, X),
    ("msgrcv", K::Function, X, X),
    ("msgsnd", K::Function, X, X),
    ("my", K::Keyword, X, X),
    ("ne", K::Keyword, X, X),
    ("next", K::Keyword, X, X),
    ("no", K::Keyword, X, X),
    ("not", K::Keyword, X, X),
    ("oct", K::Function, N, S),
    ("open", K::Function, X, X),
    ("opendir", K::Function, X, X),
    ("or", K::Keyword, X, X),
    ("ord", K::Function, N, S),
    ("our", K::Keyword, X, X),
    ("pack", K::Function, S, X),
    ("package", K::Keyword, X, X),
    ("pipe", K::Function, X, X),
    ("pop", K::Function, X, X),
    ("pos", K::Function, N, X),
    ("print", K::Function, X, X),
    ("printf", K::Function, X, X),
    ("prototype", K::Function, X, X),
    ("push", K::Function, X, X),
    ("quotemeta", K::Function, S, S),
    ("rand", K::Function, N, X),
    ("read", K::Function, X, X),
    ("readdir", K::Function, X, X),
    ("readline", K::Function, S, X),
    ("readlink", K::Function, S, X),
    ("readpipe", K::Function, X, X),
    ("recv", K::Function, X, X),
    ("redo", K::Keyword, X, X),
    ("ref", K::Function, S, X),
    ("rename", K::Function, X, X),
    ("require", K::Keyword, X, X),
    ("reset", K::Function, X, X),
    ("return", K::Keyword, X, X),
    ("reverse", K::Function, X, X),
    ("rewinddir", K::Function, X, X),
    ("rindex", K::Function, N, S),
    ("rmdir", K::Function, X, X),
    ("s", K::Keyword, X, X),
    ("say", K::Function, X, X),
    ("scalar", K::Function, X, X),
    ("seek", K::Function, X, X),
    ("seekdir", K::Function, X, X),
    ("select", K::Function, X, X),
    ("semctl", K::Function, X, X),
    ("semget", K::Function, X, X),
    ("semop", K::Function, X, X),
    ("send", K::Function, X, X),
    ("setgrent", K::Function, X, X),
    ("sethostent", K::Function, X, X),
    ("setnetent", K::Function, X, X),
    ("setpgrp", K::Function, X, X),
    ("setpriority", K::Function, X, X),
    ("setprotoent", K::Function, X, X),
    ("setpwent", K::Function, X, X),
    ("setservent", K::Function, X, X),
    ("setsockopt", K::Function, X, X),
    ("shift", K::Function, X, X),
    ("shmctl", K::Function, X, X),
    ("shmget", K::Function, X, X),
    ("shmread", K::Function, X, X),
    ("shmwrite", K::Function, X, X),
    ("shutdown", K::Function, X, X),
    ("sin", K::Function, X, N),
    ("sleep", K::Function, X, X),
    ("socket", K::Function, X, X),
    ("socketpair", K::Function, X, X),
    ("sort", K::Function, X, X),
    ("splice", K::Function, X, X),
    ("split", K::Function, X, X),
    ("sprintf", K::Function, S, X),
    ("sqrt", K::Function, N, N),
    ("srand", K::Function, X, X),
    ("stat", K::Function, X, X),
    ("state", K::Keyword, X, X),
    ("study", K::Function, X, X),
    ("sub", K::Keyword, X, X),
    ("substr", K::Function, S, S),
    ("symlink", K::Function, X, X),
    ("syscall", K::Function, X, X),
    ("sysopen", K::Function, X, X),
    ("sysread", K::Function, X, X),
    ("sysseek", K::Function, X, X),
    ("system", K::Function, X, X),
    ("syswrite", K::Function, X, X),
    ("tell", K::Function, N, X),
    ("telldir", K::Function, X, X),
    ("tie", K::Function, X, X),
    ("tied", K::Function, X, X),
    ("time", K::Function, N, X),
    ("times", K::Function, X, X),
    ("tr", K::Keyword, X, X),
    ("truncate", K::Function, X, X),
    ("try", K::Keyword, X, X),
    ("uc", K::Function, S, S),
    ("ucfirst", K::Function, S, S),
    ("umask", K::Function, X, X),
    ("undef", K::Function, X, X),
    ("unless", K::Keyword, X, X),
    ("unlink", K::Function, X, X),
    ("unpack", K::Function, X, X),
    ("unshift", K::Function, X, X),
    ("untie", K::Function, X, X),
    ("until", K::Keyword, X, X),
    ("use", K::Keyword, X, X),
    ("utime", K::Function, X, X),
    ("values", K::Function, X, X),
    ("vec", K::Function, X, X),
    ("wait", K::Function, X, X),
    ("waitpid", K::Function, X, X),
    ("wantarray", K::Function, X, X),
    ("warn", K::Function, X, X),
    ("when", K::Keyword, X, X),
    ("while", K::Keyword, X, X),
    ("write", K::Function, X, X),
    ("x", K::Keyword, X, X),
    ("xor", K::Keyword, X, X),
    ("y", K::Keyword, X, X),
];

fn row(name: &str) -> Option<&'static (&'static str, BuiltinKind, Option<InferredType>, Option<InferredType>)> {
    BUILTINS
        .binary_search_by_key(&name, |(n, ..)| n)
        .ok()
        .map(|i| &BUILTINS[i])
}

/// The kind the Perl language assigns `name`, or `None` for user code.
pub fn builtin_kind(name: &str) -> Option<BuiltinKind> {
    row(name).map(|r| r.1)
}

/// Any-kind membership: "the Perl language owns this name". The
/// diagnostics-suppression surface.
pub fn is_builtin(name: &str) -> bool {
    builtin_kind(name).is_some()
}

/// The builtins that modify the array they are handed first (`push @a, …`),
/// so `push @{$h->{k}}, …` creates the key it dereferences.
pub fn modifies_its_array_argument(name: &str) -> bool {
    matches!(name, "pop" | "push" | "shift" | "splice" | "unshift")
}

/// The BUILTIN resolution tier's completion source: callable builtins only
/// (keywords would be noise in an identifier slot; filehandles are
/// suppression-side membership, not completion candidates).
pub fn builtin_functions() -> impl Iterator<Item = &'static str> {
    BUILTINS
        .iter()
        .filter(|(_, k, ..)| matches!(k, K::Function))
        .map(|(n, ..)| *n)
}

/// Return type of a builtin, for the ones with a stable scalar contract.
pub fn builtin_return_type(name: &str) -> Option<InferredType> {
    row(name).and_then(|r| r.2.clone())
}

/// Type constraint to push on the first argument of a Perl builtin.
pub fn builtin_first_arg_type(name: &str) -> Option<InferredType> {
    row(name).and_then(|r| r.3.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        for w in BUILTINS.windows(2) {
            assert!(
                w[0].0 < w[1].0,
                "BUILTINS not strictly sorted: '{}' >= '{}'",
                w[0].0,
                w[1].0,
            );
        }
    }

    #[test]
    fn membership_projections() {
        assert_eq!(builtin_kind("print"), Some(BuiltinKind::Function));
        assert_eq!(builtin_kind("STDIN"), Some(BuiltinKind::BarewordFilehandle));
        assert_eq!(builtin_kind("my"), Some(BuiltinKind::Keyword));
        assert_eq!(builtin_kind("frobnicate"), None);
        assert!(is_builtin("die") && is_builtin("chomp"));
        assert!(!is_builtin("my_custom_sub"));
        // `new` is a constructor CONVENTION (`conventions::is_constructor_name`),
        // not a builtin — keeping it out of this table is deliberate.
        assert!(!is_builtin("new"));
    }

    /// The drift the parallel encodings realized: names the typed tables /
    /// perlfunc knew that the old adapter allowlist lacked, producing false
    /// "unresolved function" hints. One surface now — pinned callable.
    #[test]
    fn drift_names_are_builtin_functions() {
        for name in ["exp", "fc", "evalbytes", "lock"] {
            assert_eq!(
                builtin_kind(name),
                Some(BuiltinKind::Function),
                "'{name}' must be a callable builtin",
            );
        }
        // exp is the typed-slot instance of the drift: its first-arg
        // constraint existed while the old allowlist missed the name.
        assert_eq!(builtin_first_arg_type("exp"), Some(InferredType::Numeric));
    }

    /// A typed column on a non-callable row is a table bug: the type slots
    /// describe call contracts.
    #[test]
    fn typed_columns_only_on_function_rows() {
        for (name, kind, ret, arg) in BUILTINS.iter() {
            if ret.is_some() || arg.is_some() {
                assert_eq!(
                    *kind,
                    BuiltinKind::Function,
                    "typed column on non-Function row '{name}'",
                );
            }
        }
    }
}
