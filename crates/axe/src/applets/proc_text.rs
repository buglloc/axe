//! Display form of text a process chooses for itself: `comm`, thread names, and
//! `cmdline` arguments.
//!
//! Any process can put arbitrary bytes there, so the output must not depend on
//! the locale and must never reach the terminal as control sequences. Printable
//! ASCII stays as is, a backslash doubles, and every other byte becomes `\ooo`,
//! the form psmisc `pstree` prints.

use std::borrow::Cow;
use std::fmt::{self, Write as _};

pub(crate) struct ProcText<'a>(pub(crate) &'a [u8]);

impl ProcText<'_> {
    /// Returns the byte length and escaped width of the longest prefix whose
    /// escaped form fits in `limit` columns; `limit == 0` means no limit.
    /// The prefix never ends inside an escape.
    pub(crate) fn prefix(&self, limit: usize) -> (usize, usize) {
        let mut width = 0;
        for (index, &byte) in self.0.iter().enumerate() {
            let next = escaped_width(byte);
            if limit != 0 && width + next > limit {
                return (index, width);
            }
            width += next;
        }
        (self.0.len(), width)
    }

    fn escaped(&self) -> Cow<'_, str> {
        if let Ok(text) = std::str::from_utf8(self.0)
            && text.bytes().all(|byte| escaped_width(byte) == 1)
        {
            return Cow::Borrowed(text);
        }
        let mut text = String::with_capacity(self.0.len() * 2);
        for &byte in self.0 {
            match escaped_width(byte) {
                1 => text.push(char::from(byte)),
                2 => text.push_str("\\\\"),
                _ => {
                    let _ = write!(text, "\\{byte:03o}");
                }
            }
        }
        Cow::Owned(text)
    }
}

impl fmt::Display for ProcText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(&self.escaped())
    }
}

fn escaped_width(byte: u8) -> usize {
    match byte {
        b'\\' => 2,
        0x20..=0x7e => 1,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_bytes_like_psmisc_pstree() {
        for (raw, shown) in [
            (b"plain name".as_slice(), "plain name"),
            (b"ab\x7fc", r"ab\177c"),
            (b"bad\xff\xfe", r"bad\377\376"),
            ("тест".as_bytes(), r"\321\202\320\265\321\201\321\202"),
            (b"a\\b\t", r"a\\b\011"),
            (b"\x1b[2J", r"\033[2J"),
        ] {
            assert_eq!(ProcText(raw).to_string(), shown);
            assert_eq!(ProcText(raw).prefix(0), (raw.len(), shown.len()));
        }
    }

    #[test]
    fn padding_counts_escaped_columns() {
        assert_eq!(format!("{:<8}|", ProcText(b"a\xff")), r"a\377   |");
        assert_eq!(format!("{:>6}|", ProcText(b"ab")), "    ab|");
    }

    #[test]
    fn prefix_never_splits_an_escape() {
        let text = ProcText(b"ab\xffcd");
        assert_eq!(text.prefix(5), (2, 2));
        assert_eq!(text.prefix(6), (3, 6));
        assert_eq!(text.prefix(7), (4, 7));
    }
}
