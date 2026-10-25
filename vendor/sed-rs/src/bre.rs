// Translation of sed regex dialects into Oniguruma regex syntax.
//
// GNU sed defaults to POSIX basic regular expressions (BRE), where
// \(...\) groups, \{m,n\} is an interval, and bare `( ) { } + ? |` are
// literals. `-E` switches to extended REs (EREs). The translator deliberately
// works on bytes: a C-locale script may contain arbitrary non-UTF-8 bytes.

pub(crate) fn translate_pattern<S: AsRef<[u8]>>(pattern: S, extended: bool) -> Vec<u8> {
    let bytes = pattern.as_ref();
    let mut out = Vec::with_capacity(bytes.len() + 8);
    let mut i = 0;
    let mut at_subexpr_start = true;
    let mut prev_atom = false;

    while i < bytes.len() {
        let byte = bytes[i];
        match byte {
            b'\\' if i + 1 < bytes.len() => {
                let next = bytes[i + 1];
                i += 2;
                match next {
                    b'x' | b'o' | b'd' => {
                        let (radix, max) = match next {
                            b'x' => (16, 2),
                            b'o' => (8, 3),
                            _ => (10, 3),
                        };
                        let start = i;
                        let mut value = 0u32;
                        let mut count = 0;
                        while count < max && i < bytes.len() {
                            let Some(digit) = digit(bytes[i], radix) else { break };
                            value = value * radix as u32 + digit as u32;
                            i += 1;
                            count += 1;
                        }
                        if count == 0 {
                            if next == b'd' {
                                out.extend_from_slice(b"\\d");
                            } else {
                                out.push(next);
                            }
                            // `start` is intentionally unused after deciding
                            // that this was a bare escape; the following byte
                            // belongs to the rest of the pattern.
                            let _ = start;
                        } else {
                            out.extend_from_slice(format!("\\x{{{value:X}}}").as_bytes());
                        }
                        prev_atom = true;
                        at_subexpr_start = false;
                    }
                    b'A' | b'Z' | b'z' => {
                        out.push(next);
                        prev_atom = true;
                        at_subexpr_start = false;
                    }
                    b'(' | b'|' if !extended => {
                        out.push(next);
                        at_subexpr_start = true;
                        prev_atom = false;
                    }
                    b')' if !extended => {
                        out.push(b')');
                        prev_atom = true;
                        at_subexpr_start = false;
                    }
                    b'{' | b'}' | b'+' | b'?' if !extended => {
                        out.push(next);
                        at_subexpr_start = false;
                    }
                    _ => {
                        out.push(b'\\');
                        out.push(next);
                        prev_atom = true;
                        at_subexpr_start = false;
                    }
                }
            }
            b'\\' => {
                out.push(b'\\');
                i += 1;
            }
            b'[' => {
                copy_bracket_expression(bytes, &mut i, &mut out);
                prev_atom = true;
                at_subexpr_start = false;
            }
            b'(' | b')' | b'{' | b'}' | b'+' | b'?' | b'|' if !extended => {
                out.push(b'\\');
                out.push(byte);
                i += 1;
                prev_atom = true;
                at_subexpr_start = false;
            }
            b'^' if !extended => {
                if at_subexpr_start {
                    out.extend_from_slice(b"\\A");
                    prev_atom = false;
                } else {
                    out.extend_from_slice(b"\\^");
                    prev_atom = true;
                }
                at_subexpr_start = false;
                i += 1;
            }
            b'$' if !extended => {
                let is_anchor = i + 1 == bytes.len()
                    || (bytes.get(i + 1) == Some(&b'\\')
                        && matches!(bytes.get(i + 2), Some(b')' | b'|')));
                if is_anchor {
                    out.extend_from_slice(b"\\z");
                    prev_atom = false;
                } else {
                    out.extend_from_slice(b"\\$");
                    prev_atom = true;
                }
                at_subexpr_start = false;
                i += 1;
            }
            b'^' | b'$' if extended => {
                out.extend_from_slice(if byte == b'^' { b"\\A" } else { b"\\z" });
                i += 1;
                prev_atom = false;
                at_subexpr_start = false;
            }
            b'*' if !extended && !prev_atom => {
                out.extend_from_slice(b"\\*");
                i += 1;
                prev_atom = true;
                at_subexpr_start = false;
            }
            _ => {
                out.push(byte);
                i += 1;
                if !(byte == b'*' && !extended) {
                    prev_atom = true;
                }
                at_subexpr_start = false;
            }
        }
    }
    out
}

fn digit(byte: u8, radix: u8) -> Option<u8> {
    let value = match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => return None,
    };
    (value < radix).then_some(value)
}

fn copy_bracket_expression(bytes: &[u8], index: &mut usize, out: &mut Vec<u8>) {
    out.push(b'[');
    *index += 1;
    if bytes.get(*index) == Some(&b'^') {
        out.push(b'^');
        *index += 1;
    }
    if bytes.get(*index) == Some(&b']') {
        out.push(b']');
        *index += 1;
    }
    while *index < bytes.len() {
        let byte = bytes[*index];
        if byte == b'[' && matches!(bytes.get(*index + 1), Some(b':' | b'.' | b'=')) {
            let kind = bytes[*index + 1];
            out.extend_from_slice(&[b'[', kind]);
            *index += 2;
            while *index < bytes.len()
                && !(bytes[*index] == kind && bytes.get(*index + 1) == Some(&b']'))
            {
                out.push(bytes[*index]);
                *index += 1;
            }
            if *index < bytes.len() {
                out.extend_from_slice(&[kind, b']']);
                *index += 2;
            }
        } else if byte == b']' {
            out.push(b']');
            *index += 1;
            return;
        } else if byte == b'\\' && bytes.get(*index + 1) == Some(&b']') {
            out.extend_from_slice(b"\\\\");
            *index += 1;
        } else {
            out.push(byte);
            *index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bre_swaps_group_escapes() {
        assert_eq!(translate_pattern(r"\(a\)", false), b"(a)");
        assert_eq!(translate_pattern("(a)", false), br"\(a\)");
    }

    #[test]
    fn bre_swaps_repetition_escapes() {
        assert_eq!(translate_pattern(r"a\+b\?", false), b"a+b?");
        assert_eq!(translate_pattern("a+b?", false), br"a\+b\?");
        assert_eq!(translate_pattern(r"a\{2,3\}", false), b"a{2,3}");
        assert_eq!(translate_pattern("a{2}", false), br"a\{2\}");
    }

    #[test]
    fn bre_anchors() {
        assert_eq!(translate_pattern("^a$", false), br"\Aa\z");
        assert_eq!(translate_pattern("a^b", false), br"a\^b");
        assert_eq!(translate_pattern("a$b", false), br"a\$b");
        assert_eq!(translate_pattern(r"\(^a$\)", false), br"(\Aa\z)");
    }

    #[test]
    fn bre_leading_star_is_literal() {
        assert_eq!(translate_pattern("*a", false), br"\*a");
        assert_eq!(translate_pattern("a*", false), b"a*");
        assert_eq!(translate_pattern(r"\(*\)", false), br"(\*)");
    }

    #[test]
    fn ere_passthrough() {
        assert_eq!(translate_pattern("(a|b)+c?", true), b"(a|b)+c?");
        assert_eq!(translate_pattern("a{2,3}", true), b"a{2,3}");
    }

    #[test]
    fn char_escapes_both_modes() {
        assert_eq!(translate_pattern(r"\x41", false), br"\x{41}");
        assert_eq!(translate_pattern(r"\o101", false), br"\x{41}");
        assert_eq!(translate_pattern(r"\d065", false), br"\x{41}");
        assert_eq!(translate_pattern(r"\x41", true), br"\x{41}");
    }

    #[test]
    fn digit_class_preserved_without_digits() {
        assert_eq!(translate_pattern(r"\d", false), br"\d");
        assert_eq!(translate_pattern(r"\w\s", true), br"\w\s");
    }

    #[test]
    fn bracket_expression_verbatim() {
        assert_eq!(translate_pattern("[a+?(]", false), b"[a+?(]");
        assert_eq!(translate_pattern("[^]a]", false), b"[^]a]");
        assert_eq!(translate_pattern("[[:alpha:]+]", false), b"[[:alpha:]+]");
    }

    #[test]
    fn bracket_backslash_before_closing_is_literal() {
        assert_eq!(translate_pattern(r"[\]", false), br"[\\]");
        assert_eq!(translate_pattern(r"[a\]b", false), br"[a\\]b");
        assert_eq!(translate_pattern(r"[\n\t]", false), br"[\n\t]");
    }

    #[test]
    fn invalid_bytes_are_preserved() {
        assert_eq!(translate_pattern(b"[\xff]", false), b"[\xff]");
    }
}
