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
use super::witnesses::{Ends, TypeObservation};

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
// How a call to it ends, where the language fixes it; `None` is no
// evidence, the top. `exec` replaces the process and `dump` aborts it, so
// neither comes back to anything a handler could catch.
const UW: Option<Ends> = Some(Ends::UNWINDS);
const EX: Option<Ends> = Some(Ends::EXITS);
const XE: Option<Ends> = None;

/// `(name, kind, return_type, first_arg_use, ends)` — sorted by name (ASCII:
/// uppercase filehandles first), so membership is a binary search. Names
/// come from perlfunc.pod (see the module doc's tripwire). Type columns
/// carry only stable scalar contracts.
#[rustfmt::skip]
static BUILTINS: &[(&str, BuiltinKind, Option<InferredType>, Option<TypeObservation>, Option<Ends>)] = &[
    ("ARGV", K::BarewordFilehandle, X, XU, XE),
    ("ARGVOUT", K::BarewordFilehandle, X, XU, XE),
    ("DATA", K::BarewordFilehandle, X, XU, XE),
    ("STDERR", K::BarewordFilehandle, X, XU, XE),
    ("STDIN", K::BarewordFilehandle, X, XU, XE),
    ("STDOUT", K::BarewordFilehandle, X, XU, XE),
    ("abs", K::Function, N, NU, XE),
    ("accept", K::Function, X, XU, XE),
    ("alarm", K::Function, X, XU, XE),
    ("and", K::Keyword, X, XU, XE),
    ("atan2", K::Function, X, NU, XE),
    ("bind", K::Function, X, XU, XE),
    ("binmode", K::Function, X, XU, XE),
    ("bless", K::Function, X, XU, XE),
    ("break", K::Keyword, X, XU, XE),
    ("caller", K::Function, X, XU, XE),
    ("catch", K::Keyword, X, XU, XE),
    ("chdir", K::Function, X, XU, XE),
    ("chmod", K::Function, X, XU, XE),
    ("chomp", K::Function, X, SU, XE),
    ("chop", K::Function, X, SU, XE),
    ("chown", K::Function, X, XU, XE),
    ("chr", K::Function, S, NU, XE),
    ("chroot", K::Function, X, XU, XE),
    ("class", K::Keyword, X, XU, XE),
    ("close", K::Function, X, XU, XE),
    ("closedir", K::Function, X, XU, XE),
    ("cmp", K::Keyword, X, XU, XE),
    ("connect", K::Function, X, XU, XE),
    ("continue", K::Keyword, X, XU, XE),
    ("cos", K::Function, X, NU, XE),
    ("crypt", K::Function, S, XU, XE),
    ("dbmclose", K::Function, X, XU, XE),
    ("dbmopen", K::Function, X, XU, XE),
    ("default", K::Keyword, X, XU, XE),
    ("defer", K::Keyword, X, XU, XE),
    ("defined", K::Function, B, XU, XE),
    ("delete", K::Function, X, XU, XE),
    ("die", K::Function, X, XU, UW),
    ("do", K::Keyword, X, XU, XE),
    ("dump", K::Function, X, XU, EX),
    ("each", K::Function, X, XU, XE),
    ("else", K::Keyword, X, XU, XE),
    ("elsif", K::Keyword, X, XU, XE),
    ("endgrent", K::Function, X, XU, XE),
    ("endhostent", K::Function, X, XU, XE),
    ("endnetent", K::Function, X, XU, XE),
    ("endprotoent", K::Function, X, XU, XE),
    ("endpwent", K::Function, X, XU, XE),
    ("endservent", K::Function, X, XU, XE),
    ("eof", K::Function, X, XU, XE),
    ("eq", K::Keyword, X, XU, XE),
    ("eval", K::Function, X, XU, XE),
    ("evalbytes", K::Function, X, XU, XE),
    ("exec", K::Function, X, XU, EX),
    ("exists", K::Function, B, XU, XE),
    ("exit", K::Function, X, XU, EX),
    ("exp", K::Function, X, NU, XE),
    ("fc", K::Function, X, XU, XE),
    ("fcntl", K::Function, X, XU, XE),
    ("field", K::Keyword, X, XU, XE),
    ("fileno", K::Function, N, XU, XE),
    ("finally", K::Keyword, X, XU, XE),
    ("flock", K::Function, X, XU, XE),
    ("for", K::Keyword, X, XU, XE),
    ("foreach", K::Keyword, X, XU, XE),
    ("fork", K::Function, X, XU, XE),
    ("format", K::Keyword, X, XU, XE),
    ("formline", K::Function, X, XU, XE),
    ("ge", K::Keyword, X, XU, XE),
    ("getc", K::Function, X, XU, XE),
    ("getgrent", K::Function, X, XU, XE),
    ("getgrgid", K::Function, X, XU, XE),
    ("getgrnam", K::Function, X, XU, XE),
    ("gethostbyaddr", K::Function, X, XU, XE),
    ("gethostbyname", K::Function, X, XU, XE),
    ("gethostent", K::Function, X, XU, XE),
    ("getlogin", K::Function, X, XU, XE),
    ("getnetbyaddr", K::Function, X, XU, XE),
    ("getnetbyname", K::Function, X, XU, XE),
    ("getnetent", K::Function, X, XU, XE),
    ("getpeername", K::Function, X, XU, XE),
    ("getpgrp", K::Function, X, XU, XE),
    ("getppid", K::Function, X, XU, XE),
    ("getpriority", K::Function, X, XU, XE),
    ("getprotobyname", K::Function, X, XU, XE),
    ("getprotobynumber", K::Function, X, XU, XE),
    ("getprotoent", K::Function, X, XU, XE),
    ("getpwent", K::Function, X, XU, XE),
    ("getpwnam", K::Function, X, XU, XE),
    ("getpwuid", K::Function, X, XU, XE),
    ("getservbyname", K::Function, X, XU, XE),
    ("getservbyport", K::Function, X, XU, XE),
    ("getservent", K::Function, X, XU, XE),
    ("getsockname", K::Function, X, XU, XE),
    ("getsockopt", K::Function, X, XU, XE),
    ("given", K::Keyword, X, XU, XE),
    ("glob", K::Function, X, XU, XE),
    ("gmtime", K::Function, X, XU, XE),
    ("goto", K::Keyword, X, XU, XE),
    ("grep", K::Function, X, XU, XE),
    ("gt", K::Keyword, X, XU, XE),
    ("hex", K::Function, N, SU, XE),
    ("if", K::Keyword, X, XU, XE),
    ("import", K::Function, X, XU, XE),
    ("index", K::Function, N, SU, XE),
    ("int", K::Function, N, NU, XE),
    ("ioctl", K::Function, X, XU, XE),
    ("isa", K::Keyword, X, XU, XE),
    ("join", K::Function, S, XU, XE),
    ("keys", K::Function, X, XU, XE),
    ("kill", K::Function, X, XU, XE),
    ("last", K::Keyword, X, XU, XE),
    ("lc", K::Function, S, SU, XE),
    ("lcfirst", K::Function, S, SU, XE),
    ("le", K::Keyword, X, XU, XE),
    ("length", K::Function, N, SU, XE),
    ("link", K::Function, X, XU, XE),
    ("listen", K::Function, X, XU, XE),
    ("local", K::Keyword, X, XU, XE),
    ("localtime", K::Function, X, XU, XE),
    ("lock", K::Function, X, XU, XE),
    ("log", K::Function, X, NU, XE),
    ("lstat", K::Function, X, XU, XE),
    ("lt", K::Keyword, X, XU, XE),
    ("m", K::Keyword, X, XU, XE),
    ("map", K::Function, X, XU, XE),
    ("method", K::Keyword, X, XU, XE),
    ("mkdir", K::Function, X, XU, XE),
    ("msgctl", K::Function, X, XU, XE),
    ("msgget", K::Function, X, XU, XE),
    ("msgrcv", K::Function, X, XU, XE),
    ("msgsnd", K::Function, X, XU, XE),
    ("my", K::Keyword, X, XU, XE),
    ("ne", K::Keyword, X, XU, XE),
    ("next", K::Keyword, X, XU, XE),
    ("no", K::Keyword, X, XU, XE),
    ("not", K::Keyword, X, XU, XE),
    ("oct", K::Function, N, SU, XE),
    ("open", K::Function, X, XU, XE),
    ("opendir", K::Function, X, XU, XE),
    ("or", K::Keyword, X, XU, XE),
    ("ord", K::Function, N, SU, XE),
    ("our", K::Keyword, X, XU, XE),
    ("pack", K::Function, S, XU, XE),
    ("package", K::Keyword, X, XU, XE),
    ("pipe", K::Function, X, XU, XE),
    ("pop", K::Function, X, XU, XE),
    ("pos", K::Function, N, XU, XE),
    ("print", K::Function, X, XU, XE),
    ("printf", K::Function, X, XU, XE),
    ("prototype", K::Function, X, XU, XE),
    ("push", K::Function, X, XU, XE),
    ("quotemeta", K::Function, S, SU, XE),
    ("rand", K::Function, N, XU, XE),
    ("read", K::Function, X, XU, XE),
    ("readdir", K::Function, X, XU, XE),
    ("readline", K::Function, S, XU, XE),
    ("readlink", K::Function, S, XU, XE),
    ("readpipe", K::Function, X, XU, XE),
    ("recv", K::Function, X, XU, XE),
    ("redo", K::Keyword, X, XU, XE),
    ("ref", K::Function, S, XU, XE),
    ("rename", K::Function, X, XU, XE),
    ("require", K::Keyword, X, XU, XE),
    ("reset", K::Function, X, XU, XE),
    ("return", K::Keyword, X, XU, XE),
    ("reverse", K::Function, X, XU, XE),
    ("rewinddir", K::Function, X, XU, XE),
    ("rindex", K::Function, N, SU, XE),
    ("rmdir", K::Function, X, XU, XE),
    ("s", K::Keyword, X, XU, XE),
    ("say", K::Function, X, XU, XE),
    ("scalar", K::Function, X, XU, XE),
    ("seek", K::Function, X, XU, XE),
    ("seekdir", K::Function, X, XU, XE),
    ("select", K::Function, X, XU, XE),
    ("semctl", K::Function, X, XU, XE),
    ("semget", K::Function, X, XU, XE),
    ("semop", K::Function, X, XU, XE),
    ("send", K::Function, X, XU, XE),
    ("setgrent", K::Function, X, XU, XE),
    ("sethostent", K::Function, X, XU, XE),
    ("setnetent", K::Function, X, XU, XE),
    ("setpgrp", K::Function, X, XU, XE),
    ("setpriority", K::Function, X, XU, XE),
    ("setprotoent", K::Function, X, XU, XE),
    ("setpwent", K::Function, X, XU, XE),
    ("setservent", K::Function, X, XU, XE),
    ("setsockopt", K::Function, X, XU, XE),
    ("shift", K::Function, X, XU, XE),
    ("shmctl", K::Function, X, XU, XE),
    ("shmget", K::Function, X, XU, XE),
    ("shmread", K::Function, X, XU, XE),
    ("shmwrite", K::Function, X, XU, XE),
    ("shutdown", K::Function, X, XU, XE),
    ("sin", K::Function, X, NU, XE),
    ("sleep", K::Function, X, XU, XE),
    ("socket", K::Function, X, XU, XE),
    ("socketpair", K::Function, X, XU, XE),
    ("sort", K::Function, X, XU, XE),
    ("splice", K::Function, X, XU, XE),
    ("split", K::Function, X, XU, XE),
    ("sprintf", K::Function, S, XU, XE),
    ("sqrt", K::Function, N, NU, XE),
    ("srand", K::Function, X, XU, XE),
    ("stat", K::Function, X, XU, XE),
    ("state", K::Keyword, X, XU, XE),
    ("study", K::Function, X, XU, XE),
    ("sub", K::Keyword, X, XU, XE),
    ("substr", K::Function, S, SU, XE),
    ("symlink", K::Function, X, XU, XE),
    ("syscall", K::Function, X, XU, XE),
    ("sysopen", K::Function, X, XU, XE),
    ("sysread", K::Function, X, XU, XE),
    ("sysseek", K::Function, X, XU, XE),
    ("system", K::Function, X, XU, XE),
    ("syswrite", K::Function, X, XU, XE),
    ("tell", K::Function, N, XU, XE),
    ("telldir", K::Function, X, XU, XE),
    ("tie", K::Function, X, XU, XE),
    ("tied", K::Function, X, XU, XE),
    ("time", K::Function, N, XU, XE),
    ("times", K::Function, X, XU, XE),
    ("tr", K::Keyword, X, XU, XE),
    ("truncate", K::Function, X, XU, XE),
    ("try", K::Keyword, X, XU, XE),
    ("uc", K::Function, S, SU, XE),
    ("ucfirst", K::Function, S, SU, XE),
    ("umask", K::Function, X, XU, XE),
    ("undef", K::Function, X, XU, XE),
    ("unless", K::Keyword, X, XU, XE),
    ("unlink", K::Function, X, XU, XE),
    ("unpack", K::Function, X, XU, XE),
    ("unshift", K::Function, X, XU, XE),
    ("untie", K::Function, X, XU, XE),
    ("until", K::Keyword, X, XU, XE),
    ("use", K::Keyword, X, XU, XE),
    ("utime", K::Function, X, XU, XE),
    ("values", K::Function, X, XU, XE),
    ("vec", K::Function, X, XU, XE),
    ("wait", K::Function, X, XU, XE),
    ("waitpid", K::Function, X, XU, XE),
    ("wantarray", K::Function, X, XU, XE),
    ("warn", K::Function, X, XU, XE),
    ("when", K::Keyword, X, XU, XE),
    ("while", K::Keyword, X, XU, XE),
    ("write", K::Function, X, XU, XE),
    ("x", K::Keyword, X, XU, XE),
    ("xor", K::Keyword, X, XU, XE),
    ("y", K::Keyword, X, XU, XE),
];

fn row(name: &str) -> Option<&'static (&'static str, BuiltinKind, Option<InferredType>, Option<TypeObservation>, Option<Ends>)> {
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
/// How a call to builtin `name` ends, where the language fixes it (`die`
/// unwinds; `exit`, `exec` and `dump` leave the process). `None` for
/// every other name: no evidence.
pub fn builtin_ends(name: &str) -> Option<Ends> {
    row(name).and_then(|r| r.4)
}

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
        for (name, kind, ret, arg, _) in BUILTINS.iter() {
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
