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
//! `builtin_first_arg_use`) are columns of the same rows — a typed name
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
use super::witnesses::TypeObservation;

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
// What passing a value as the first argument reveals about it.
const NU: Option<TypeObservation> = Some(TypeObservation::NumericUse);
const SU: Option<TypeObservation> = Some(TypeObservation::StringUse);
const XU: Option<TypeObservation> = None;

/// `(name, kind, return_type, first_arg_use)` — sorted by name (ASCII:
/// uppercase filehandles first), so membership is a binary search. Names
/// come from perlfunc.pod (see the module doc's tripwire). Type columns
/// carry only stable scalar contracts.
#[rustfmt::skip]
static BUILTINS: &[(&str, BuiltinKind, Option<InferredType>, Option<TypeObservation>)] = &[
    ("ARGV", K::BarewordFilehandle, X, XU),
    ("ARGVOUT", K::BarewordFilehandle, X, XU),
    ("DATA", K::BarewordFilehandle, X, XU),
    ("STDERR", K::BarewordFilehandle, X, XU),
    ("STDIN", K::BarewordFilehandle, X, XU),
    ("STDOUT", K::BarewordFilehandle, X, XU),
    ("abs", K::Function, N, NU),
    ("accept", K::Function, X, XU),
    ("alarm", K::Function, X, XU),
    ("and", K::Keyword, X, XU),
    ("atan2", K::Function, X, NU),
    ("bind", K::Function, X, XU),
    ("binmode", K::Function, X, XU),
    ("bless", K::Function, X, XU),
    ("break", K::Keyword, X, XU),
    ("caller", K::Function, X, XU),
    ("catch", K::Keyword, X, XU),
    ("chdir", K::Function, X, XU),
    ("chmod", K::Function, X, XU),
    ("chomp", K::Function, X, SU),
    ("chop", K::Function, X, SU),
    ("chown", K::Function, X, XU),
    ("chr", K::Function, S, NU),
    ("chroot", K::Function, X, XU),
    ("class", K::Keyword, X, XU),
    ("close", K::Function, X, XU),
    ("closedir", K::Function, X, XU),
    ("cmp", K::Keyword, X, XU),
    ("connect", K::Function, X, XU),
    ("continue", K::Keyword, X, XU),
    ("cos", K::Function, X, NU),
    ("crypt", K::Function, S, XU),
    ("dbmclose", K::Function, X, XU),
    ("dbmopen", K::Function, X, XU),
    ("default", K::Keyword, X, XU),
    ("defer", K::Keyword, X, XU),
    ("defined", K::Function, B, XU),
    ("delete", K::Function, X, XU),
    ("die", K::Function, X, XU),
    ("do", K::Keyword, X, XU),
    ("dump", K::Function, X, XU),
    ("each", K::Function, X, XU),
    ("else", K::Keyword, X, XU),
    ("elsif", K::Keyword, X, XU),
    ("endgrent", K::Function, X, XU),
    ("endhostent", K::Function, X, XU),
    ("endnetent", K::Function, X, XU),
    ("endprotoent", K::Function, X, XU),
    ("endpwent", K::Function, X, XU),
    ("endservent", K::Function, X, XU),
    ("eof", K::Function, X, XU),
    ("eq", K::Keyword, X, XU),
    ("eval", K::Function, X, XU),
    ("evalbytes", K::Function, X, XU),
    ("exec", K::Function, X, XU),
    ("exists", K::Function, B, XU),
    ("exit", K::Function, X, XU),
    ("exp", K::Function, X, NU),
    ("fc", K::Function, X, XU),
    ("fcntl", K::Function, X, XU),
    ("field", K::Keyword, X, XU),
    ("fileno", K::Function, N, XU),
    ("finally", K::Keyword, X, XU),
    ("flock", K::Function, X, XU),
    ("for", K::Keyword, X, XU),
    ("foreach", K::Keyword, X, XU),
    ("fork", K::Function, X, XU),
    ("format", K::Keyword, X, XU),
    ("formline", K::Function, X, XU),
    ("ge", K::Keyword, X, XU),
    ("getc", K::Function, X, XU),
    ("getgrent", K::Function, X, XU),
    ("getgrgid", K::Function, X, XU),
    ("getgrnam", K::Function, X, XU),
    ("gethostbyaddr", K::Function, X, XU),
    ("gethostbyname", K::Function, X, XU),
    ("gethostent", K::Function, X, XU),
    ("getlogin", K::Function, X, XU),
    ("getnetbyaddr", K::Function, X, XU),
    ("getnetbyname", K::Function, X, XU),
    ("getnetent", K::Function, X, XU),
    ("getpeername", K::Function, X, XU),
    ("getpgrp", K::Function, X, XU),
    ("getppid", K::Function, X, XU),
    ("getpriority", K::Function, X, XU),
    ("getprotobyname", K::Function, X, XU),
    ("getprotobynumber", K::Function, X, XU),
    ("getprotoent", K::Function, X, XU),
    ("getpwent", K::Function, X, XU),
    ("getpwnam", K::Function, X, XU),
    ("getpwuid", K::Function, X, XU),
    ("getservbyname", K::Function, X, XU),
    ("getservbyport", K::Function, X, XU),
    ("getservent", K::Function, X, XU),
    ("getsockname", K::Function, X, XU),
    ("getsockopt", K::Function, X, XU),
    ("given", K::Keyword, X, XU),
    ("glob", K::Function, X, XU),
    ("gmtime", K::Function, X, XU),
    ("goto", K::Keyword, X, XU),
    ("grep", K::Function, X, XU),
    ("gt", K::Keyword, X, XU),
    ("hex", K::Function, N, SU),
    ("if", K::Keyword, X, XU),
    ("import", K::Function, X, XU),
    ("index", K::Function, N, SU),
    ("int", K::Function, N, NU),
    ("ioctl", K::Function, X, XU),
    ("isa", K::Keyword, X, XU),
    ("join", K::Function, S, XU),
    ("keys", K::Function, X, XU),
    ("kill", K::Function, X, XU),
    ("last", K::Keyword, X, XU),
    ("lc", K::Function, S, SU),
    ("lcfirst", K::Function, S, SU),
    ("le", K::Keyword, X, XU),
    ("length", K::Function, N, SU),
    ("link", K::Function, X, XU),
    ("listen", K::Function, X, XU),
    ("local", K::Keyword, X, XU),
    ("localtime", K::Function, X, XU),
    ("lock", K::Function, X, XU),
    ("log", K::Function, X, NU),
    ("lstat", K::Function, X, XU),
    ("lt", K::Keyword, X, XU),
    ("m", K::Keyword, X, XU),
    ("map", K::Function, X, XU),
    ("method", K::Keyword, X, XU),
    ("mkdir", K::Function, X, XU),
    ("msgctl", K::Function, X, XU),
    ("msgget", K::Function, X, XU),
    ("msgrcv", K::Function, X, XU),
    ("msgsnd", K::Function, X, XU),
    ("my", K::Keyword, X, XU),
    ("ne", K::Keyword, X, XU),
    ("next", K::Keyword, X, XU),
    ("no", K::Keyword, X, XU),
    ("not", K::Keyword, X, XU),
    ("oct", K::Function, N, SU),
    ("open", K::Function, X, XU),
    ("opendir", K::Function, X, XU),
    ("or", K::Keyword, X, XU),
    ("ord", K::Function, N, SU),
    ("our", K::Keyword, X, XU),
    ("pack", K::Function, S, XU),
    ("package", K::Keyword, X, XU),
    ("pipe", K::Function, X, XU),
    ("pop", K::Function, X, XU),
    ("pos", K::Function, N, XU),
    ("print", K::Function, X, XU),
    ("printf", K::Function, X, XU),
    ("prototype", K::Function, X, XU),
    ("push", K::Function, X, XU),
    ("quotemeta", K::Function, S, SU),
    ("rand", K::Function, N, XU),
    ("read", K::Function, X, XU),
    ("readdir", K::Function, X, XU),
    ("readline", K::Function, S, XU),
    ("readlink", K::Function, S, XU),
    ("readpipe", K::Function, X, XU),
    ("recv", K::Function, X, XU),
    ("redo", K::Keyword, X, XU),
    ("ref", K::Function, S, XU),
    ("rename", K::Function, X, XU),
    ("require", K::Keyword, X, XU),
    ("reset", K::Function, X, XU),
    ("return", K::Keyword, X, XU),
    ("reverse", K::Function, X, XU),
    ("rewinddir", K::Function, X, XU),
    ("rindex", K::Function, N, SU),
    ("rmdir", K::Function, X, XU),
    ("s", K::Keyword, X, XU),
    ("say", K::Function, X, XU),
    ("scalar", K::Function, X, XU),
    ("seek", K::Function, X, XU),
    ("seekdir", K::Function, X, XU),
    ("select", K::Function, X, XU),
    ("semctl", K::Function, X, XU),
    ("semget", K::Function, X, XU),
    ("semop", K::Function, X, XU),
    ("send", K::Function, X, XU),
    ("setgrent", K::Function, X, XU),
    ("sethostent", K::Function, X, XU),
    ("setnetent", K::Function, X, XU),
    ("setpgrp", K::Function, X, XU),
    ("setpriority", K::Function, X, XU),
    ("setprotoent", K::Function, X, XU),
    ("setpwent", K::Function, X, XU),
    ("setservent", K::Function, X, XU),
    ("setsockopt", K::Function, X, XU),
    ("shift", K::Function, X, XU),
    ("shmctl", K::Function, X, XU),
    ("shmget", K::Function, X, XU),
    ("shmread", K::Function, X, XU),
    ("shmwrite", K::Function, X, XU),
    ("shutdown", K::Function, X, XU),
    ("sin", K::Function, X, NU),
    ("sleep", K::Function, X, XU),
    ("socket", K::Function, X, XU),
    ("socketpair", K::Function, X, XU),
    ("sort", K::Function, X, XU),
    ("splice", K::Function, X, XU),
    ("split", K::Function, X, XU),
    ("sprintf", K::Function, S, XU),
    ("sqrt", K::Function, N, NU),
    ("srand", K::Function, X, XU),
    ("stat", K::Function, X, XU),
    ("state", K::Keyword, X, XU),
    ("study", K::Function, X, XU),
    ("sub", K::Keyword, X, XU),
    ("substr", K::Function, S, SU),
    ("symlink", K::Function, X, XU),
    ("syscall", K::Function, X, XU),
    ("sysopen", K::Function, X, XU),
    ("sysread", K::Function, X, XU),
    ("sysseek", K::Function, X, XU),
    ("system", K::Function, X, XU),
    ("syswrite", K::Function, X, XU),
    ("tell", K::Function, N, XU),
    ("telldir", K::Function, X, XU),
    ("tie", K::Function, X, XU),
    ("tied", K::Function, X, XU),
    ("time", K::Function, N, XU),
    ("times", K::Function, X, XU),
    ("tr", K::Keyword, X, XU),
    ("truncate", K::Function, X, XU),
    ("try", K::Keyword, X, XU),
    ("uc", K::Function, S, SU),
    ("ucfirst", K::Function, S, SU),
    ("umask", K::Function, X, XU),
    ("undef", K::Function, X, XU),
    ("unless", K::Keyword, X, XU),
    ("unlink", K::Function, X, XU),
    ("unpack", K::Function, X, XU),
    ("unshift", K::Function, X, XU),
    ("untie", K::Function, X, XU),
    ("until", K::Keyword, X, XU),
    ("use", K::Keyword, X, XU),
    ("utime", K::Function, X, XU),
    ("values", K::Function, X, XU),
    ("vec", K::Function, X, XU),
    ("wait", K::Function, X, XU),
    ("waitpid", K::Function, X, XU),
    ("wantarray", K::Function, X, XU),
    ("warn", K::Function, X, XU),
    ("when", K::Keyword, X, XU),
    ("while", K::Keyword, X, XU),
    ("write", K::Function, X, XU),
    ("x", K::Keyword, X, XU),
    ("xor", K::Keyword, X, XU),
    ("y", K::Keyword, X, XU),
];

fn row(name: &str) -> Option<&'static (&'static str, BuiltinKind, Option<InferredType>, Option<TypeObservation>)> {
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

/// The builtins that take the element they are handed out of its container
/// (`delete $h{k}`), leaving the slot without a value.
pub fn removes_its_element_argument(name: &str) -> bool {
    name == "delete"
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

/// What a Perl builtin's use of its first argument reveals about it.
pub fn builtin_first_arg_use(name: &str) -> Option<TypeObservation> {
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
        assert_eq!(builtin_first_arg_use("exp"), Some(TypeObservation::NumericUse));
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
