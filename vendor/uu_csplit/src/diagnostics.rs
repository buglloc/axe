// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! Maps a [`CsplitError`] onto the operand it came from, so that
//! [`uucore::diagnostics`] can render it with a caret.

use std::ffi::OsString;

use uucore::diagnostics::{Snapshot, ValueOptions};
use uucore::translate;

use crate::csplit_error::CsplitError;

/// The operands a caret may point at, as they were typed.
///
/// The errors name a pattern or a format but not where it was written, so the
/// operands are carried alongside them to the point where the report is drawn.
pub struct Operands<'a> {
    /// The pattern operands, in the order they appear on the line.
    pub patterns: &'a [&'a str],
    /// The value of `-b`/`--suffix-format`, when it was given.
    pub suffix_format: Option<&'a str>,
    /// The value of `-n`/`--digits`, when it was given.
    pub digits: Option<&'a str>,
}

/// The options whose value can be a separate argument, which an operand must
/// not be counted as.
const VALUE_OPTIONS: ValueOptions = ValueOptions {
    shorts: &['b', 'f', 'n'],
    longs: &["suffix-format", "prefix", "digits"],
};

/// Render `err` against `args` — the whole argument list, program name
/// included — where `operands` are the operands as typed.
///
/// Returns `false` when the error cannot be tied to one of them, in which case
/// the caller should fall back to the plain one-line message.
pub fn render(args: &[OsString], operands: &Operands, err: &CsplitError) -> bool {
    let snapshot = Snapshot::with_program(args);
    let index_of_pattern = |pattern: &str| index_of_pattern(args, &snapshot, operands, pattern);
    let message = err.to_string();

    match err {
        CsplitError::InvalidPattern(pattern, problem) => {
            let Some(index) = index_of_pattern(pattern) else {
                return false;
            };
            // Without a problem there is no offset to trust, so the operand is
            // underlined whole.
            let Some(problem) = problem else {
                return snapshot.render(
                    index,
                    &message,
                    None,
                    Some(&translate!("csplit-diag-help-pattern")),
                );
            };
            snapshot.render_inside_at(
                index,
                pattern,
                problem.span.clone(),
                &message,
                problem.label.as_deref(),
                Some(&translate!("csplit-diag-help-pattern")),
            )
        }
        // Neither error says which operand it is about, so the pattern is
        // found by the line number it carries.
        CsplitError::LineNumberIsZero => render_line_number(args, &snapshot, operands, 0, &message),
        CsplitError::LineNumberSmallerThanPrevious(current, _) => {
            render_line_number(args, &snapshot, operands, *current, &message)
        }
        CsplitError::InvalidNumber(_) => {
            let Some(digits) = operands.digits else {
                return false;
            };
            snapshot.render_option_value(
                digits,
                Some('n'),
                Some("digits"),
                0..digits.len(),
                &message,
                None,
                Some(&translate!("csplit-diag-help-digits")),
            )
        }
        // Both messages describe the format as a whole rather than a character
        // of it, so the whole value is underlined.
        CsplitError::SuffixFormatIncorrect | CsplitError::SuffixFormatTooManyPercents => {
            let Some(format) = operands.suffix_format else {
                return false;
            };
            snapshot.render_option_value(
                format,
                Some('b'),
                Some("suffix-format"),
                0..format.len(),
                &message,
                None,
                Some(&translate!("csplit-diag-help-suffix-format")),
            )
        }
        // Raised while reading the input rather than while parsing the line,
        // and named by the pattern as csplit rewrites it rather than as it was
        // typed, so there is nothing to point a caret at.
        _ => false,
    }
}

/// Render `message` against the pattern operand holding the line number
/// `line_number`.
fn render_line_number(
    args: &[OsString],
    snapshot: &Snapshot,
    operands: &Operands,
    line_number: usize,
    message: &str,
) -> bool {
    let Some(pattern) = operands
        .patterns
        .iter()
        .find(|p| p.parse::<usize>() == Ok(line_number))
    else {
        return false;
    };
    let Some(index) = index_of_pattern(args, snapshot, operands, pattern) else {
        return false;
    };
    snapshot.render(
        index,
        message,
        None,
        Some(&translate!("csplit-diag-help-line-number")),
    )
}

/// Where in `args` the pattern operand `pattern` sits.
///
/// The patterns are the operands following FILE. The argument arrived at is
/// checked against the pattern as typed, so a walk thrown off by a spelling
/// [`VALUE_OPTIONS`] does not know draws nothing rather than the wrong thing.
fn index_of_pattern(
    args: &[OsString],
    snapshot: &Snapshot,
    operands: &Operands,
    pattern: &str,
) -> Option<usize> {
    let nth = operands.patterns.iter().position(|p| *p == pattern)?;
    // FILE is the first operand; the patterns follow it.
    let index = snapshot.index_of_operand(nth + 1, &VALUE_OPTIONS)?;
    (args[index].as_encoded_bytes() == pattern.as_bytes()).then_some(index)
}
