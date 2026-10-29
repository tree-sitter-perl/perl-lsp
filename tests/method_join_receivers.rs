//! A method's parameter joins over its callers. A caller whose receiver
//! names no class (a computed class-name string here, Foswiki's
//! `$handler->INCLUDE(..)`) may be calling another family's method of the
//! same name, so it stays out of the join; a typed caller still counts.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_perl-lsp");

const FOSWIKI: &str = "package Foswiki;
sub new { bless {}, shift }
sub INCLUDE {
    my ( $this, $params ) = @_;
    return $params;
}
sub _includeProtocol {
    my ( $this, $handler, $control, $params ) = @_;
    $handler = 'Foswiki::IncludeHandlers::' . $handler;
    return $handler->INCLUDE( $this, $control, $params );
}
1;
";

fn hover_params(tag: &str, extra: &[(&str, &str)]) -> String {
    let dir = std::env::temp_dir().join(format!("perl-lsp-join-recv-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Foswiki.pm"), FOSWIKI).unwrap();
    for (name, src) in extra {
        std::fs::write(dir.join(name), src).unwrap();
    }
    let req = "{\"id\":\"p\",\"q\":\"hover\",\"file\":\"Foswiki.pm\",\"line\":3,\"col\":17}\n";
    let mut child = Command::new(BIN)
        .args(["--batch", "."])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", dir.join(".test-cache"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(req.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_computed_receiver_is_not_a_caller() {
    let out = hover_params("computed", &[]);
    assert!(out.contains("\"ok\":true"), "{out}");
    assert!(!out.contains("*type:"), "{out}");
}

#[test]
fn a_typed_receiver_is_a_caller() {
    let app = "package App;\nsub run { Foswiki->new->INCLUDE(bless({}, 'Params')) }\n1;\n";
    let out = hover_params("typed", &[("App.pm", app)]);
    assert!(out.contains("*type: Params*"), "{out}");
}
