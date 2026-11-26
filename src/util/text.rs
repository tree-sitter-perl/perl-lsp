//! Source text in and out of `String`: reading a file perl may hold in any
//! byte encoding, and byte-bounded trimming that respects UTF-8.

/// A source file's text. Valid UTF-8 is taken as is; anything else is read
/// as Latin-1, one char per byte — what perl does with a file that has no
/// `use utf8`, and what an editor falling back to latin1 shows, so the
/// columns line up. `read_to_string` refuses such a file outright, which
/// left it unindexed and killed the CLI. Every fallback is counted
/// (`source.latin1`).
pub fn read_source(path: impl AsRef<std::path::Path>) -> std::io::Result<String> {
    if let Some(text) = held_buffer(path.as_ref()) {
        return Ok(text);
    }
    let bytes = std::fs::read(path)?;
    Ok(decode_source(bytes))
}

thread_local! {
    static HELD: std::cell::RefCell<Option<(std::path::PathBuf, String)>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with `path` reading as `text` on this thread, the way an editor
/// holds a buffer the disk has not seen: `--batch` asks about a half-typed
/// buffer through it. Only `read_source` on the calling thread sees it.
pub fn with_held_buffer<T>(path: &std::path::Path, text: String, f: impl FnOnce() -> T) -> T {
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            HELD.with(|h| *h.borrow_mut() = None);
        }
    }
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    HELD.with(|h| *h.borrow_mut() = Some((key, text)));
    let _release = Release;
    f()
}

fn held_buffer(path: &std::path::Path) -> Option<String> {
    HELD.with(|h| {
        let held = h.borrow();
        let (key, text) = held.as_ref()?;
        let canon = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        (canon == *key).then(|| text.clone())
    })
}

/// `read_source`'s decoding, for bytes already in hand (stdin).
pub fn decode_source(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            super::ghost_stats::count("source.latin1");
            e.into_bytes().into_iter().map(char::from).collect()
        }
    }
}

/// The longest prefix of `s` that fits in `max_bytes` without splitting a
/// character.
///
/// Every caller that caps a string for display wants this, not `&s[..max]` —
/// the naive slice panics whenever the cap lands inside a multibyte char, and
/// a caller that catches the panic per-file silently loses that file's whole
/// analysis. Russian POD in the CPAN corpus is what found it.
pub fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_source_reads_as_is_and_latin1_byte_per_char() {
        assert_eq!(decode_source("caf\u{e9}".as_bytes().to_vec()), "caf\u{e9}");
        // `café` in Latin-1: one byte for the é, which is not UTF-8.
        let latin1 = decode_source(vec![b'c', b'a', b'f', 0xE9, b'\n', 0xFF]);
        assert_eq!(latin1, "caf\u{e9}\n\u{ff}");
        assert_eq!(latin1.chars().count(), 6, "one char per byte");
    }

    #[test]
    fn a_held_buffer_reads_in_place_of_the_disk_on_its_thread_only() {
        let dir = std::env::temp_dir().join(format!("held-buffer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.py");
        std::fs::write(&file, "on disk\n").unwrap();
        let seen = with_held_buffer(&file, "f(a, ".to_string(), || {
            let other = std::thread::spawn({
                let file = file.clone();
                move || read_source(&file).unwrap()
            })
            .join()
            .unwrap();
            // The same file through another spelling of its path.
            let spelled = read_source(dir.join(".").join("a.py")).unwrap();
            (read_source(&file).unwrap(), spelled, other)
        });
        assert_eq!(seen, ("f(a, ".to_string(), "f(a, ".to_string(), "on disk\n".to_string()));
        assert_eq!(read_source(&file).unwrap(), "on disk\n", "released when the call returns");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shorter_than_cap_is_untouched() {
        assert_eq!(truncate_on_char_boundary("hello", 2000), "hello");
        assert_eq!(truncate_on_char_boundary("", 10), "");
    }

    #[test]
    fn exact_fit_is_untouched() {
        assert_eq!(truncate_on_char_boundary("abcd", 4), "abcd");
    }

    #[test]
    fn ascii_cuts_exactly_at_the_cap() {
        assert_eq!(truncate_on_char_boundary("abcdef", 4), "abcd");
    }

    /// The panic this function exists to prevent: the cap lands on byte 1 of a
    /// 2-byte char, so the prefix has to give that whole char back.
    #[test]
    fn multibyte_cuts_back_to_the_boundary() {
        let s = "аб"; // Cyrillic, 2 bytes each
        assert_eq!(s.len(), 4);
        assert_eq!(truncate_on_char_boundary(s, 3), "а");
        assert_eq!(truncate_on_char_boundary(s, 2), "а");
        assert_eq!(truncate_on_char_boundary(s, 1), "");
    }

    #[test]
    fn wide_chars_cut_back_across_several_bytes() {
        let s = "日本語"; // 3 bytes each
        assert_eq!(truncate_on_char_boundary(s, 5), "日");
        assert_eq!(truncate_on_char_boundary(s, 8), "日本");
        assert_eq!(truncate_on_char_boundary(s, 0), "");
    }

    /// A cap inside a combining sequence still yields valid UTF-8; we bound
    /// bytes, not graphemes.
    #[test]
    fn emoji_never_yields_invalid_utf8() {
        let s = "👍👍";
        for cap in 0..=s.len() {
            let got = truncate_on_char_boundary(s, cap);
            assert!(s.starts_with(got));
            assert!(got.len() <= cap);
        }
    }
}
