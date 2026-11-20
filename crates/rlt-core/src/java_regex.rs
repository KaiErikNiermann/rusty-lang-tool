//! Translate LanguageTool's Java regex syntax into what the `regex` crate reads the same way.
//!
//! LT writes its rules for `java.util.regex`. Most of that syntax means the same in the `regex`
//! crate, but escapes differ:
//!
//! - Java lets `\` escape any non-alphanumeric character as a literal (`\–`, `\ä`, `\=`); `regex`
//!   accepts only escapes of its own meta characters and rejects the rest, so the rule is dropped.
//! - `\<` and `\>` are literals in Java but word-boundary assertions in `regex`, which changes the
//!   meaning without an error.
//! - `\uXXXX` is a UTF-16 code unit in Java, so supplementary characters are written as surrogate
//!   pairs (`\ud83c\udc00`); `regex` takes code points and rejects surrogates.
//!
//! Look-around has no equivalent here and is left as it is (the regex then fails to compile).

use std::borrow::Cow;
use std::fmt::Write as _;

/// `src` with the Java-only escapes above rewritten; unchanged when it has no backslash.
#[must_use]
pub fn translate(src: &str) -> Cow<'_, str> {
    if !src.contains('\\') {
        return Cow::Borrowed(src);
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let Some(&next) = chars.get(i + 1).filter(|_| c == '\\') else {
            out.push(c);
            i += 1;
            continue;
        };
        if next == 'u' {
            if let Some((cp, used)) = utf16_escape(&chars[i..]) {
                let _ = write!(out, "\\x{{{cp:X}}}");
                i += used;
                continue;
            }
        }
        if next.is_ascii_alphanumeric() || next == '\\' {
            out.push(c);
            out.push(next); // a class, an assertion, or another escape `regex` reads like Java
        } else if is_meta(next) && next != '<' && next != '>' {
            out.push(c);
            out.push(next);
        } else {
            out.push(next); // an escaped literal
        }
        i += 2;
    }
    Cow::Owned(out)
}

/// Whether `regex` treats `c` as a meta character (so `\c` is a valid escape there).
fn is_meta(c: char) -> bool {
    let mut buf = [0; 4];
    regex::escape(c.encode_utf8(&mut buf)).len() > c.len_utf8()
}

/// A `\uXXXX` escape at the start of `s` (with its low surrogate when it is a high one): the code
/// point and how many characters it spans.
fn utf16_escape(s: &[char]) -> Option<(u32, usize)> {
    let unit = |at: usize| -> Option<u32> {
        if s.get(at) != Some(&'\\') || s.get(at + 1) != Some(&'u') {
            return None;
        }
        let hex: String = s.get(at + 2..at + 6)?.iter().collect();
        u32::from_str_radix(&hex, 16).ok()
    };
    let high = unit(0)?;
    if (0xD800..0xDC00).contains(&high) {
        let low = unit(6).filter(|l| (0xDC00..0xE000).contains(l))?;
        return Some((0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00), 12));
    }
    Some((high, 6))
}

#[cfg(test)]
mod tests {
    use super::translate;

    #[test]
    fn escaped_literals_lose_their_backslash() {
        assert_eq!(translate(r"(,|\-|\–|\()"), r"(,|\-|–|\()");
        assert_eq!(translate(r"[a-z0-9\äöüß]+e"), "[a-z0-9äöüß]+e");
        assert_eq!(translate(r"[:\{\=\<\>\[]"), r"[:\{=<>\[]");
    }

    #[test]
    fn classes_and_assertions_are_kept() {
        assert_eq!(translate(r"\p{Lu}\d+\b\.\\"), r"\p{Lu}\d+\b\.\\");
        assert_eq!(translate("plain"), "plain");
    }

    #[test]
    fn surrogate_pairs_become_code_points() {
        assert_eq!(
            translate(r"[\ud83c\udc00-\ud83c\udfff]+|[\u2600-\u27ff]+"),
            r"[\x{1F000}-\x{1F3FF}]+|[\x{2600}-\x{27FF}]+"
        );
    }

    #[test]
    fn translated_lt_regexes_compile() {
        for src in [
            r"(,|;|und|oder|—|\-|\–|\()",
            r"[\.\)\]\…\?\!\:;]",
            r"[:\{\=\<\>\[]",
            r"[\ud83c\udc00-\ud83c\udfff]+|[\ud83d\udc00-\ud83d\udfff]+|[\u2600-\u27ff]+",
        ] {
            assert!(regex::Regex::new(&translate(src)).is_ok(), "{src}");
        }
    }
}
