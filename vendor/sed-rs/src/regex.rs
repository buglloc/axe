use std::ops::Range;

use onig::{
    EncodedBytes, MatchParam, Regex, RegexOptions, Region, SearchOptions, Syntax, SyntaxOperator,
};

use crate::bre::translate_pattern;
use crate::error::Result;

struct RegexPair {
    first: Regex,
    longest: Regex,
}

pub(crate) struct PosixRegex {
    bytes: RegexPair,
    utf8: Option<RegexPair>,
}

pub(crate) struct Captures {
    region: Region,
    base: usize,
}

impl Captures {
    pub(crate) fn new() -> Self {
        Self {
            region: Region::new(),
            base: 0,
        }
    }

    pub(crate) fn pos(&self, group: usize) -> Option<(usize, usize)> {
        self.region
            .pos(group)
            .map(|(start, end)| (self.base + start, self.base + end))
    }
}

pub(crate) struct Matcher<'regex, 'input> {
    pair: &'regex RegexPair,
    input: &'input [u8],
    utf8: bool,
    current: Option<Range<usize>>,
    next_span: usize,
    finished: bool,
}

pub(crate) fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|locale| {
            let locale = locale.to_ascii_lowercase();
            locale.contains("utf-8") || locale.contains("utf8")
        })
}

impl PosixRegex {
    pub(crate) fn new(pattern: &str, extended: bool, ignore_case: bool) -> Result<Self> {
        let translated = translate_pattern(pattern, extended);
        let mut syntax = *Syntax::gnu_regex();
        syntax.enable_operators(
            SyntaxOperator::SYNTAX_OPERATOR_ESC_X_BRACE_HEX8
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_CONTROL_CHARS
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_D_DIGIT
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_S_WHITE_SPACE
                | SyntaxOperator::SYNTAX_OPERATOR_ESC_AZ_BUF_ANCHOR,
        );
        let mut options =
            RegexOptions::REGEX_OPTION_SINGLELINE | RegexOptions::REGEX_OPTION_MULTILINE;
        if ignore_case {
            options |= RegexOptions::REGEX_OPTION_IGNORECASE;
        }
        let bytes = RegexPair::new(&translated, options, &syntax, false)?;
        let utf8 = if utf8_locale() {
            Some(RegexPair::new(&translated, options, &syntax, true)?)
        } else {
            None
        };
        Ok(Self { bytes, utf8 })
    }

    pub(crate) fn matcher<'regex, 'input>(
        &'regex self,
        input: &'input [u8],
    ) -> Matcher<'regex, 'input> {
        Matcher {
            pair: self.utf8.as_ref().unwrap_or(&self.bytes),
            input,
            utf8: self.utf8.is_some(),
            current: None,
            next_span: 0,
            finished: false,
        }
    }

    pub(crate) fn unicode_case(&self) -> bool {
        self.utf8.is_some()
    }

    pub(crate) fn is_match(&self, input: &[u8]) -> Result<bool> {
        Ok(self.matcher(input).find_start(0)?.is_some())
    }
}

impl RegexPair {
    fn new(pattern: &str, options: RegexOptions, syntax: &Syntax, utf8: bool) -> Result<Self> {
        let compile = |options| {
            if utf8 {
                Regex::with_options(pattern, options, syntax)
            } else {
                Regex::with_options_and_encoding(
                    EncodedBytes::ascii(pattern.as_bytes()),
                    options,
                    syntax,
                )
            }
        };
        Ok(Self {
            first: compile(options)?,
            longest: compile(options | RegexOptions::REGEX_OPTION_FIND_LONGEST)?,
        })
    }
}

impl Matcher<'_, '_> {
    fn next_valid_span(&mut self) -> Option<Range<usize>> {
        if self.finished {
            return None;
        }
        let start = self.next_span;
        if self.utf8 {
            if let Err(error) = std::str::from_utf8(&self.input[start..]) {
                let end = start + error.valid_up_to();
                // Invalid bytes are unmatchable characters, not a change of
                // locale. Keep empty-match positions on both sides of each byte.
                self.next_span = end + 1;
                return Some(start..end);
            }
        }
        self.finished = true;
        Some(start..self.input.len())
    }

    fn search_options(&self, span: &Range<usize>) -> SearchOptions {
        let mut options = SearchOptions::SEARCH_OPTION_NONE;
        // Oniguruma's buffer anchors also honor NOTBOL/NOTEOL. An invalid-byte
        // boundary must not become a new beginning or end of the pattern space.
        if span.start != 0 {
            options |= SearchOptions::SEARCH_OPTION_NOTBOL
        }
        if span.end != self.input.len() {
            options |= SearchOptions::SEARCH_OPTION_NOTEOL
        }
        options
    }

    fn find_start(&mut self, from: usize) -> Result<Option<usize>> {
        loop {
            if self.current.is_none() {
                self.current = self.next_valid_span()
            }
            let Some(span) = &self.current else {
                return Ok(None);
            };
            if from <= span.end {
                let input = &self.input[span.clone()];
                let start = self.pair.first.search_with_param(
                    EncodedBytes::from_parts(input, self.pair.first.encoding()),
                    from.saturating_sub(span.start),
                    input.len(),
                    self.search_options(span),
                    None,
                    MatchParam::default(),
                )?;
                if let Some(start) = start {
                    return Ok(Some(span.start + start));
                }
            }
            self.current = None;
        }
    }

    pub(crate) fn advance(&self, offset: usize) -> usize {
        let mut next = offset + 1;
        if self.utf8 {
            if let Some(span) = &self.current {
                while next < span.end && self.input[next] & 0xc0 == 0x80 {
                    next += 1
                }
            }
        }
        next
    }

    pub(crate) fn captures_at(&mut self, from: usize, captures: &mut Captures) -> Result<bool> {
        let Some(start) = self.find_start(from)? else {
            return Ok(false);
        };
        let span = self
            .current
            .as_ref()
            .expect("a found match belongs to the current span");
        let input = &self.input[span.clone()];
        captures.region.clear();
        captures.base = span.start;
        // The longest pass is anchored at the earliest start. FIND_LONGEST is
        // required at call time as well as compilation time by onig_match.
        let options = self.search_options(span)
            | SearchOptions::from_bits_retain(RegexOptions::REGEX_OPTION_FIND_LONGEST.bits());
        Ok(self
            .pair
            .longest
            .match_with_param(
                EncodedBytes::from_parts(input, self.pair.longest.encoding()),
                start - span.start,
                options,
                Some(&mut captures.region),
                MatchParam::default(),
            )?
            .is_some())
    }
}
