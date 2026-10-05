// AXE modification: suppress the header row only when every header is empty.

//! Parser for `ps -o` format specifications.
//!
//! Procps-ng accepts several spelling conventions in the same `-o`
//! invocation:
//!
//! ```text
//!     -o pid,ppid,comm        # comma-separated
//!     -o pid ppid comm        # space-separated (multi-arg)
//!     -o "pid ppid,comm"      # mixed in one quoted arg
//!     -o pid=ID,ppid=PARENT   # `=label` overrides the default header
//!     -o pid=                 # `=` with empty label suppresses headers
//! ```
//!
//! Multiple `-o` flags are concatenated. Empty fields (a stray comma)
//! are dropped silently to match procps. Unknown column names produce
//! a single error mentioning the offender — we don't list candidates
//! to keep the message terse.

use crate::fields::{Field, lookup, lookup_aix};

/// One resolved column in the `-o` output.
pub struct FieldSpec {
    pub field: &'static Field,
    /// `None` = use the field's default header. `Some("")` leaves this
    /// column's header blank; all columns must be blank to suppress the row.
    pub label: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parse one or more raw `-o` argument values into resolved field
/// specs. Each input string is itself a comma- or space-separated list.
///
/// Tokens of the form `%X` (where `X` is a single character) are
/// resolved via the AIX/SVR4 code table; everything else goes through
/// the named-field lookup. So `%p` is the AIX code for `pid`, but
/// `%cpu` (3 chars after `%`) is the literal field name.
pub fn parse_format_spec(specs: &[String]) -> Result<Vec<FieldSpec>, ParseError> {
    let mut out = Vec::new();
    for raw in specs {
        for token in split_tokens(raw) {
            if token.is_empty() {
                continue;
            }
            let (name, label) = match token.split_once('=') {
                Some((n, l)) => (n, Some(l.to_string())),
                None => (token.as_str(), None),
            };
            let field = resolve_field(name)
                .ok_or_else(|| ParseError(format!("ps: unknown column name: {name}")))?;
            out.push(FieldSpec { field, label });
        }
    }
    Ok(out)
}

/// Look up `name` as either an AIX `%X` code (when it's exactly two
/// characters starting with `%`) or a named field.
fn resolve_field(name: &str) -> Option<&'static Field> {
    if name.len() == 2
        && let Some(code) = name.strip_prefix('%')
    {
        return lookup_aix(code);
    }
    lookup(name)
}

/// Returns true only when every output column has an empty header.
pub fn suppress_headers(specs: &[FieldSpec]) -> bool {
    specs
        .iter()
        .all(|s| matches!(&s.label, Some(l) if l.is_empty()))
}

/// Split on commas AND whitespace, but only outside `=label` rhs:
/// procps allows `pid=My PID,ppid` to mean two columns where the
/// first one's header is `My PID`. The `=label` portion extends to
/// the next comma or the end of the argument; comma resets the
/// state for the next column.
fn split_tokens(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut buf = String::new();
    let mut after_eq = false;
    for ch in raw.chars() {
        match ch {
            ',' => {
                tokens.push(std::mem::take(&mut buf));
                after_eq = false;
            }
            c if c.is_whitespace() && !after_eq => {
                if !buf.is_empty() {
                    tokens.push(std::mem::take(&mut buf));
                }
            }
            '=' => {
                buf.push('=');
                after_eq = true;
            }
            c => buf.push(c),
        }
    }
    if !buf.is_empty() {
        tokens.push(buf);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(specs: &[FieldSpec]) -> Vec<&'static str> {
        specs.iter().map(|s| s.field.name).collect()
    }

    #[test]
    fn comma_separated() {
        let s = parse_format_spec(&["pid,ppid,comm".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "ppid", "comm"]);
        assert!(s.iter().all(|f| f.label.is_none()));
    }

    #[test]
    fn space_separated_multi_arg() {
        let s = parse_format_spec(&["pid".into(), "ppid".into(), "comm".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "ppid", "comm"]);
    }

    #[test]
    fn space_separated_single_arg() {
        let s = parse_format_spec(&["pid ppid comm".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "ppid", "comm"]);
    }

    #[test]
    fn mixed_separators() {
        let s = parse_format_spec(&["pid,ppid comm".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "ppid", "comm"]);
    }

    #[test]
    fn label_override() {
        let s = parse_format_spec(&["pid=ID,comm=Process Name".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "comm"]);
        assert_eq!(s[0].label.as_deref(), Some("ID"));
        assert_eq!(s[1].label.as_deref(), Some("Process Name"));
    }

    #[test]
    fn all_empty_labels_suppress_headers() {
        let all_empty = parse_format_spec(&["pid=,comm=".into()]).unwrap();
        assert!(suppress_headers(&all_empty));
        let mixed = parse_format_spec(&["pid=,comm".into()]).unwrap();
        assert!(!suppress_headers(&mixed));
        let named = parse_format_spec(&["pid=,comm=NAME".into()]).unwrap();
        assert!(!suppress_headers(&named));
    }

    #[test]
    fn aliases_resolve_to_canonical() {
        let s = parse_format_spec(&["tid,ucmd,cmd".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "comm", "args"]);
    }

    #[test]
    fn unknown_column_errors() {
        let err = match parse_format_spec(&["pid,nosuchfield".into()]) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(err.0.contains("nosuchfield"), "{}", err.0);
    }

    #[test]
    fn empty_tokens_dropped() {
        let s = parse_format_spec(&["pid,,comm".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "comm"]);
    }

    #[test]
    fn aix_codes_resolve_to_named_fields() {
        let s = parse_format_spec(&["%U %p %a".into()]).unwrap();
        assert_eq!(names(&s), ["user", "pid", "args"]);
    }

    #[test]
    fn aix_codes_are_case_sensitive() {
        // %P → ppid, %p → pid: distinct.
        let s = parse_format_spec(&["%P %p".into()]).unwrap();
        assert_eq!(names(&s), ["ppid", "pid"]);
    }

    #[test]
    fn aix_with_label_override() {
        // Comma-separated, since `=label` extends through whitespace
        // to the next comma (matches procps's named-field rule).
        let s = parse_format_spec(&["%p=ID,%a=COMMAND".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "args"]);
        assert_eq!(s[0].label.as_deref(), Some("ID"));
        assert_eq!(s[1].label.as_deref(), Some("COMMAND"));
    }

    #[test]
    fn percent_cpu_is_named_not_aix() {
        // %cpu is 4 chars total, not the 2-char `%X` shape, so it
        // resolves via named lookup to the `%cpu` field.
        let s = parse_format_spec(&["%cpu".into()]).unwrap();
        assert_eq!(names(&s), ["%cpu"]);
    }

    #[test]
    fn aix_codes_can_mix_with_named_fields() {
        let s = parse_format_spec(&["%p,user,%a".into()]).unwrap();
        assert_eq!(names(&s), ["pid", "user", "args"]);
    }

    #[test]
    fn unknown_aix_code_errors() {
        let err = match parse_format_spec(&["%Z".into()]) {
            Err(e) => e,
            Ok(_) => panic!("expected error"),
        };
        assert!(err.0.contains("%Z"), "{}", err.0);
    }
}
