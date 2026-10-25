use std::ops::Range;

use bstr::BString;
use onig::{EncodedBytes, MatchParam, RegexOptions, Region, SearchOptions, Syntax, SyntaxOperator};

use crate::error::Result;

struct RegexPair {
    search: onig::Regex,
    longest: onig::Regex,
}

pub(crate) struct Regex {
    bytes: RegexPair,
    unicode: Option<RegexPair>,
}

pub(crate) struct Matcher<'r, 't> {
    regex: &'r RegexPair,
    text: &'t [u8],
    multibyte: bool,
    spans: Option<Vec<Range<usize>>>,
}

pub(crate) struct Captures {
    region: Region,
    base: usize,
}

impl Captures {
    pub(crate) fn pos(&self, index: usize) -> Option<(usize, usize)> {
        self.region
            .pos(index)
            .map(|(start, end)| (self.base + start, self.base + end))
    }
}

impl RegexPair {
    fn new(pattern: &[u8], multibyte: bool) -> Result<Self> {
        let options = RegexOptions::REGEX_OPTION_SINGLELINE | RegexOptions::REGEX_OPTION_MULTILINE;
        let mut syntax = Syntax::posix_extended().clone();
        syntax.enable_operators(
            SyntaxOperator::SYNTAX_OPERATOR_ESC_CONTROL_CHARS
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_OCTAL3
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_X_HEX2
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_W_WORD
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_S_WHITE_SPACE
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_LTGT_WORD_BEGIN_END
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_B_WORD_BOUND
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_AZ_BUF_ANCHOR,
        );
        let compile = |options| {
            if multibyte {
                onig::Regex::with_options(
                    std::str::from_utf8(pattern).expect("Unicode patterns are validated"),
                    options,
                    &syntax,
                )
            } else {
                onig::Regex::with_options_and_encoding(
                    EncodedBytes::ascii(pattern),
                    options,
                    &syntax,
                )
            }
        };
        Ok(Self {
            search: compile(options)?,
            longest: compile(options | RegexOptions::REGEX_OPTION_FIND_LONGEST)?,
        })
    }
}

impl Regex {
    pub(crate) fn new(pattern: &[u8], multibyte: bool) -> Result<Self> {
        let pattern = normalize_pattern(pattern);
        let bytes = RegexPair::new(&pattern, false)?;
        let unicode = if multibyte && std::str::from_utf8(&pattern).is_ok() {
            Some(RegexPair::new(&pattern, true)?)
        } else {
            None
        };
        Ok(Self { bytes, unicode })
    }

    pub(crate) fn matcher<'r, 't>(&'r self, text: &'t [u8]) -> Matcher<'r, 't> {
        let unicode = self.unicode.as_ref();
        let spans = if unicode.is_some() && std::str::from_utf8(text).is_err() {
            let mut spans = Vec::new();
            let mut base = 0;
            loop {
                match std::str::from_utf8(&text[base..]) {
                    Ok(_) => {
                        spans.push(base..text.len());
                        break;
                    }
                    Err(error) => {
                        let end = base + error.valid_up_to();
                        spans.push(base..end);
                        base = end + 1;
                    }
                }
            }
            Some(spans)
        } else {
            None
        };
        Matcher {
            regex: unicode.unwrap_or(&self.bytes),
            text,
            multibyte: unicode.is_some(),
            spans,
        }
    }

    pub(crate) fn is_match(&self, text: &[u8]) -> Result<bool> {
        Ok(self.matcher(text).locate(0)?.is_some())
    }

    pub(crate) fn find(&self, text: &[u8], from: usize) -> Result<Option<Range<usize>>> {
        self.matcher(text).find(from)
    }

    pub(crate) fn ranges(&self, text: &[u8]) -> Result<Vec<Range<usize>>> {
        let matcher = self.matcher(text);
        let mut ranges = Vec::new();
        let mut from = 0;
        let mut previous_end = None;
        while let Some(found) = matcher.find(from)? {
            from = if found.is_empty() {
                matcher.advance(found.end)
            } else {
                found.end
            };
            if found.is_empty() && previous_end == Some(found.start) {
                continue;
            }
            previous_end = Some(found.end);
            ranges.push(found);
        }
        Ok(ranges)
    }

    pub(crate) fn split<'a>(&self, text: &'a [u8]) -> Result<Vec<&'a [u8]>> {
        let matcher = self.matcher(text);
        let mut fields = Vec::new();
        let mut last = 0;
        let mut from = 0;
        while let Some(found) = matcher.find(from)? {
            if found.is_empty() {
                from = matcher.advance(found.end);
            } else {
                fields.push(&text[last..found.start]);
                last = found.end;
                from = found.end;
            }
        }
        fields.push(&text[last..]);
        Ok(fields)
    }
}

impl Matcher<'_, '_> {
    fn options(&self, span: &Range<usize>) -> SearchOptions {
        let mut options = SearchOptions::SEARCH_OPTION_NONE;
        if span.start != 0 {
            options |= SearchOptions::SEARCH_OPTION_NOTBOL;
        }
        if span.end != self.text.len() {
            options |= SearchOptions::SEARCH_OPTION_NOTEOL;
        }
        options
    }

    fn search_span(&self, span: &Range<usize>, from: usize) -> Result<Option<usize>> {
        let text = &self.text[span.clone()];
        Ok(self.regex.search.search_with_param(
            EncodedBytes::from_parts(text, self.regex.search.encoding()),
            from.max(span.start) - span.start,
            text.len(),
            self.options(span),
            None,
            MatchParam::default(),
        )?)
    }

    fn locate(&self, from: usize) -> Result<Option<(Range<usize>, usize)>> {
        if from > self.text.len() {
            return Ok(None);
        }
        if let Some(spans) = &self.spans {
            let first = spans.partition_point(|span| span.end < from);
            for span in &spans[first..] {
                if let Some(start) = self.search_span(span, from)? {
                    return Ok(Some((span.clone(), start)));
                }
            }
            Ok(None)
        } else {
            let span = 0..self.text.len();
            Ok(self.search_span(&span, from)?.map(|start| (span, start)))
        }
    }

    pub(crate) fn advance(&self, at: usize) -> usize {
        if at == self.text.len() {
            at + 1
        } else {
            crate::value::character_end(self.text, at, self.multibyte)
        }
    }

    fn find(&self, from: usize) -> Result<Option<Range<usize>>> {
        let Some((span, start)) = self.locate(from)? else {
            return Ok(None);
        };
        // FIND_LONGEST searches can prefer a later start. Pin the second pass
        // to the earliest start to obtain POSIX leftmost-longest matching.
        Ok(self
            .regex
            .longest
            .match_with_param(
                EncodedBytes::from_parts(&self.text[span.clone()], self.regex.longest.encoding()),
                start,
                self.options(&span)
                    | SearchOptions::from_bits_retain(
                        RegexOptions::REGEX_OPTION_FIND_LONGEST.bits(),
                    ),
                None,
                MatchParam::default(),
            )?
            .map(|length| span.start + start..span.start + start + length))
    }

    pub(crate) fn captures(&self, from: usize) -> Result<Option<Captures>> {
        let Some((span, start)) = self.locate(from)? else {
            return Ok(None);
        };
        let mut region = Region::new();
        let matched = self.regex.longest.match_with_param(
            EncodedBytes::from_parts(&self.text[span.clone()], self.regex.longest.encoding()),
            start,
            self.options(&span)
                | SearchOptions::from_bits_retain(RegexOptions::REGEX_OPTION_FIND_LONGEST.bits()),
            Some(&mut region),
            MatchParam::default(),
        )?;
        Ok(matched.map(|_| Captures {
            region,
            base: span.start,
        }))
    }
}

pub(crate) fn normalize_pattern(pattern: &[u8]) -> BString {
    let mut normalized = Vec::with_capacity(pattern.len());
    let mut at = 0;
    let mut in_class = false;
    let mut class_first = false;
    let mut class_negation = false;
    let mut class_item = None;
    while at < pattern.len() {
        let byte = pattern[at];
        if byte == b'\\' && at + 1 < pattern.len() {
            let escaped = pattern[at + 1];
            if let Some((literal, next)) = decode_escape(pattern, at + 1) {
                push_literal(&mut normalized, literal, in_class);
                if in_class {
                    class_first = false;
                }
                at = next;
                continue;
            }
            match escaped {
                b'y' if !in_class => normalized.extend_from_slice(b"\\b"),
                b'`' if !in_class => normalized.extend_from_slice(b"\\A"),
                b'\'' if !in_class => normalized.extend_from_slice(b"\\z"),
                b'B' | b'y' if in_class => normalized.push(escaped),
                b'w' | b'W' | b's' | b'S' | b'B' => {
                    normalized.extend_from_slice(&pattern[at..at + 2])
                }
                byte if byte.is_ascii_alphanumeric() => normalized.push(byte),
                _ => normalized.extend_from_slice(&pattern[at..at + 2]),
            }
            if in_class {
                class_first = false;
            }
            at += 2;
            continue;
        }
        if in_class {
            normalized.push(byte);
            if let Some(marker) = class_item {
                if byte == marker && pattern.get(at + 1) == Some(&b']') {
                    normalized.push(b']');
                    at += 2;
                    class_item = None;
                    continue;
                }
            } else if byte == b'['
                && pattern
                    .get(at + 1)
                    .is_some_and(|byte| matches!(byte, b':' | b'.' | b'='))
            {
                let marker = pattern[at + 1];
                normalized.push(marker);
                at += 2;
                class_item = Some(marker);
                class_first = false;
                continue;
            } else if byte == b']' && !class_first {
                in_class = false;
            }
            if byte == b'^' && class_first && class_negation {
                class_negation = false;
            } else {
                class_first = false;
            }
        } else {
            match byte {
                b'^' => normalized.extend_from_slice(b"\\A"),
                b'$' => normalized.extend_from_slice(b"\\z"),
                b'[' => {
                    normalized.push(byte);
                    in_class = true;
                    class_first = true;
                    class_negation = true;
                }
                _ => normalized.push(byte),
            }
        }
        at += 1;
    }
    normalized.into()
}

fn decode_escape(pattern: &[u8], at: usize) -> Option<(u8, usize)> {
    let byte = pattern[at];
    let control = match byte {
        b'n' => Some(b'\n'),
        b'r' => Some(b'\r'),
        b't' => Some(b'\t'),
        b'b' => Some(8),
        b'f' => Some(12),
        b'a' => Some(7),
        b'v' => Some(11),
        _ => None,
    };
    if let Some(control) = control {
        return Some((control, at + 1));
    }
    let (base, first, maximum) = if matches!(byte, b'0'..=b'7') {
        (8, at, 3)
    } else if byte == b'x' {
        (16, at + 1, 2)
    } else {
        return None;
    };
    let mut next = first;
    let mut number = 0_u16;
    while next < pattern.len() && next - first < maximum {
        let Some(digit) = (pattern[next] as char).to_digit(base) else {
            break;
        };
        number = number * base as u16 + digit as u16;
        next += 1;
    }
    (next != first).then_some((number as u8, next))
}

fn push_literal(pattern: &mut Vec<u8>, byte: u8, in_class: bool) {
    if matches!(
        byte,
        b'.' | b'^'
            | b'$'
            | b'['
            | b']'
            | b'('
            | b')'
            | b'{'
            | b'}'
            | b'*'
            | b'+'
            | b'?'
            | b'|'
            | b'\\'
    ) || (in_class && byte == b'-')
    {
        pattern.push(b'\\');
    }
    pattern.push(byte);
}
