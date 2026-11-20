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

/// `rep`, a Java `Matcher.replaceAll` replacement for a pattern with `groups` capture groups, in the
/// `regex` crate's replacement syntax.
///
/// Java reads `$1s` as group 1 followed by `s`; `regex` reads it as a group named `1s` and inserts
/// nothing. Java's `\x` is a literal `x`; a literal `$` is `$$` in `regex`. Further digits after
/// `$` extend the group number only while it stays a group of the pattern (`Matcher.appendReplacement`,
/// JDK 21): `$12` is group 12 for a pattern with twelve groups, else group 1 followed by `2`.
#[must_use]
pub fn translate_replacement(rep: &str, groups: usize) -> String {
    let mut out = String::with_capacity(rep.len() + 4);
    let mut chars = rep.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('$') => out.push_str("$$"),
                Some(lit) => out.push(lit),
                None => out.push('\\'),
            },
            '$' if chars.peek().is_some_and(char::is_ascii_digit) => {
                let mut group = chars.next().and_then(|d| d.to_digit(10)).unwrap_or(0) as usize;
                while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                    let longer = group * 10 + d as usize;
                    if longer > groups {
                        break;
                    }
                    group = longer;
                    chars.next();
                }
                let _ = write!(out, "${{{group}}}");
            }
            '$' if chars.peek() == Some(&'{') => out.push('$'), // `${name}` reads the same
            '$' => out.push_str("$$"),
            _ => out.push(c),
        }
    }
    out
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
    fn replacements_keep_java_group_boundaries() {
        use super::translate_replacement as rep;
        assert_eq!(rep("$1s", 1), "${1}s");
        assert_eq!(rep("Grü$1e", 1), "Grü${1}e");
        assert_eq!(rep(r"\$$1 \\x", 1), r"$$${1} \x");
        let re = regex::Regex::new("(a)").unwrap();
        assert_eq!(re.replace_all("a", rep("$1x", 1).as_str()), "ax");
    }

    #[test]
    fn a_longer_group_number_is_read_only_while_the_group_exists() {
        use super::translate_replacement as rep;
        // LT's `VER:$12:PLU:$2` for a pattern with two groups: group 1, then the digit 2.
        assert_eq!(rep("VER:$12:PLU:$2", 2), "VER:${1}2:PLU:${2}");
        assert_eq!(rep("$12", 12), "${12}");
        assert_eq!(rep("$10", 3), "${1}0");
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
