use std::io::BufRead;
use std::ops::Range;

use bstr::BString;
use regex_automata::nfa::thompson::{NFA, State};
use regex_automata::util::{look::Look, primitives::StateID, syntax};

use crate::error::{Error, Result};

pub(crate) struct RecordReader<'a> {
    reader: Box<dyn BufRead + 'a>,
    buffer: Vec<u8>,
    start: usize,
    offset: usize,
    eof: bool,
    separator: Option<RegexSeparator>,
}

impl<'a> RecordReader<'a> {
    pub(super) fn new(reader: Box<dyn BufRead + 'a>) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            start: 0,
            offset: 0,
            eof: false,
            separator: None,
        }
    }

    fn ensure(&mut self, at: usize) -> Result<bool> {
        while at >= self.buffer.len() && !self.eof {
            let bytes = self.reader.fill_buf()?;
            if bytes.is_empty() {
                self.eof = true;
            } else {
                self.buffer.extend_from_slice(bytes);
                let count = bytes.len();
                self.reader.consume(count);
            }
        }
        Ok(at < self.buffer.len())
    }

    fn complete_character(&mut self, at: usize) -> Result<()> {
        if !self.ensure(at)? {
            return Ok(());
        }
        let width = match self.buffer[at] {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Ok(()),
        };
        for offset in 1..width {
            if !self.ensure(at + offset)? {
                break;
            }
            if let Err(error) = std::str::from_utf8(&self.buffer[at..=at + offset]) {
                if error.error_len().is_some() {
                    break;
                }
            }
        }
        Ok(())
    }

    fn discard_consumed(&mut self) {
        // Retain a preceding UTF-8 character for word-boundary assertions.
        if self.start > 8192 && self.start > self.buffer.len() / 2 {
            let discard = self.start - 4;
            self.buffer.drain(..discard);
            self.start -= discard;
            self.offset += discard;
        }
    }

    fn record(&mut self, end: usize, next: usize) -> BString {
        let record = self.buffer[self.start..end].to_vec().into();
        self.start = next;
        record
    }

    pub(super) fn read_record(&mut self, rs: &[u8], multibyte: bool) -> Result<Option<BString>> {
        self.discard_consumed();
        if rs.is_empty() {
            return self.paragraph();
        }
        if rs.len() == 1 {
            let mut at = self.start;
            loop {
                if let Some(relative) = self.buffer[at..].iter().position(|byte| *byte == rs[0]) {
                    let end = at + relative;
                    return Ok(Some(self.record(end, end + 1)));
                }
                at = self.buffer.len();
                if !self.ensure(at)? {
                    return Ok((self.start < at).then(|| self.record(at, at)));
                }
            }
        }
        let mut separator = match self.separator.take() {
            Some(separator) if separator.pattern == rs && separator.multibyte == multibyte => {
                separator
            }
            _ => RegexSeparator::new(rs, multibyte)?,
        };
        let result = separator.read(self);
        self.separator = Some(separator);
        result
    }

    fn paragraph(&mut self) -> Result<Option<BString>> {
        while self.ensure(self.start)? {
            let blanks = self.buffer[self.start..]
                .iter()
                .take_while(|byte| **byte == b'\n')
                .count();
            self.start += blanks;
            self.discard_consumed();
            if self.start < self.buffer.len() {
                break;
            }
        }
        if !self.ensure(self.start)? {
            return Ok(None);
        }
        let mut at = self.start;
        loop {
            if !self.ensure(at)? {
                return Ok(Some(self.record(at, at)));
            }
            if self.buffer[at] != b'\n' {
                at += 1;
                continue;
            }
            if !self.ensure(at + 1)? {
                return Ok(Some(self.record(at, at + 1)));
            }
            if self.buffer[at + 1] != b'\n' {
                at += 1;
                continue;
            }
            return Ok(Some(self.record(at, at + 2)));
        }
    }
}

struct RegexSeparator {
    pattern: BString,
    multibyte: bool,
    unicode: bool,
    nfa: NFA,
    seeds: Vec<(StateID, usize)>,
    stack: Vec<(StateID, usize)>,
    reached: Vec<usize>,
}

impl RegexSeparator {
    fn new(pattern: &[u8], multibyte: bool) -> Result<Self> {
        let normalized = crate::regex::normalize_pattern(pattern);
        let unicode = multibyte && std::str::from_utf8(&normalized).is_ok();
        let source = if unicode {
            std::str::from_utf8(&normalized)
                .expect("Unicode pattern is valid")
                .to_owned()
        } else {
            let mut source = String::with_capacity(normalized.len());
            for &byte in normalized.iter() {
                if byte.is_ascii() {
                    source.push(byte as char);
                } else {
                    use std::fmt::Write;
                    write!(source, "\\x{byte:02x}").expect("writing into a String cannot fail");
                }
            }
            source
        };
        let nfa = NFA::compiler()
            .configure(NFA::config().utf8(false))
            .syntax(
                syntax::Config::new()
                    .utf8(false)
                    .unicode(unicode)
                    .octal(true)
                    .dot_matches_new_line(true),
            )
            .build(&source)
            .map_err(|error| Error::runtime(format!("invalid record separator: {error}")))?;
        let count = nfa.states().len();
        Ok(Self {
            pattern: pattern.into(),
            multibyte,
            unicode,
            nfa,
            seeds: Vec::with_capacity(count),
            stack: Vec::with_capacity(count),
            reached: vec![usize::MAX; count],
        })
    }

    fn read(&mut self, reader: &mut RecordReader<'_>) -> Result<Option<BString>> {
        self.seeds.clear();
        let mut best: Option<Range<usize>> = None;
        let mut at = reader.start;
        loop {
            let has_byte = reader.ensure(at)?;
            self.reached.fill(usize::MAX);
            self.stack.clear();
            self.stack.extend(self.seeds.drain(..));
            if best.is_none() {
                self.stack.push((self.nfa.start_anchored(), at));
            }
            while let Some((id, start)) = self.stack.pop() {
                if self.reached[id.as_usize()] <= start {
                    continue;
                }
                self.reached[id.as_usize()] = start;
                match self.nfa.state(id) {
                    State::Look { look, next } => {
                        let matches = match look {
                            Look::Start => reader.offset + at == 0,
                            Look::End => !has_byte,
                            _ => {
                                if self.unicode {
                                    reader.complete_character(at)?;
                                }
                                self.nfa.look_matcher().matches(*look, &reader.buffer, at)
                            }
                        };
                        if matches {
                            self.stack.push((*next, start));
                        }
                    }
                    State::Union { alternates } => {
                        self.stack.extend(alternates.iter().map(|id| (*id, start)));
                    }
                    State::BinaryUnion { alt1, alt2 } => {
                        self.stack.push((*alt1, start));
                        self.stack.push((*alt2, start));
                    }
                    State::Capture { next, .. } => self.stack.push((*next, start)),
                    State::Match { .. } if at > start => {
                        if best.as_ref().is_none_or(|found| {
                            start < found.start || (start == found.start && at > found.end)
                        }) {
                            best = Some(start..at);
                        }
                    }
                    _ => {}
                }
            }

            let mut live = false;
            if has_byte {
                let byte = reader.buffer[at];
                for (index, state) in self.nfa.states().iter().enumerate() {
                    let start = self.reached[index];
                    if start == usize::MAX || best.as_ref().is_some_and(|found| start > found.start)
                    {
                        continue;
                    }
                    let next = match state {
                        State::ByteRange { trans } => {
                            trans.matches_byte(byte).then_some(trans.next)
                        }
                        State::Sparse(transitions) => transitions.matches_byte(byte),
                        State::Dense(transitions) => transitions.matches_byte(byte),
                        _ => None,
                    };
                    if let Some(next) = next {
                        live = true;
                        self.seeds.push((next, start));
                    }
                }
            }
            if (!live || !has_byte) && best.is_some() {
                let found = best.expect("a separator match was found");
                return Ok(Some(reader.record(found.start, found.end)));
            }
            if !has_byte {
                return Ok((reader.start < at).then(|| reader.record(at, at)));
            }
            at += 1;
        }
    }
}

pub(super) fn variable_assignment(argument: &[u8]) -> Option<(&str, &[u8])> {
    let equals = argument.iter().position(|byte| *byte == b'=')?;
    let (name, value) = argument.split_at(equals);
    let (first, rest) = name.split_first()?;
    if !(*first == b'_' || first.is_ascii_alphabetic())
        || !rest
            .iter()
            .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return None;
    }
    Some((
        std::str::from_utf8(name).expect("variable name is ASCII"),
        &value[1..],
    ))
}

pub(super) fn unescape_assignment(value: &[u8]) -> BString {
    let mut result = Vec::with_capacity(value.len());
    let mut at = 0;
    while at < value.len() {
        let byte = value[at];
        at += 1;
        if byte != b'\\' || at == value.len() {
            result.push(byte);
            continue;
        }
        let escaped = value[at];
        at += 1;
        let simple = match escaped {
            b'n' => Some(b'\n'),
            b't' => Some(b'\t'),
            b'r' => Some(b'\r'),
            b'b' => Some(8),
            b'f' => Some(12),
            b'a' => Some(7),
            b'v' => Some(11),
            _ => None,
        };
        if let Some(byte) = simple {
            result.push(byte);
        } else if escaped == b'x' {
            let start = at;
            let mut number = 0;
            while at < value.len() && at - start < 2 && value[at].is_ascii_hexdigit() {
                number = number * 16 + (value[at] as char).to_digit(16).expect("hex digit") as u8;
                at += 1;
            }
            result.push(if at == start { b'x' } else { number });
        } else if matches!(escaped, b'0'..=b'7') {
            let mut number = u16::from(escaped - b'0');
            let mut count = 1;
            while at < value.len() && count < 3 && matches!(value[at], b'0'..=b'7') {
                number = number * 8 + u16::from(value[at] - b'0');
                at += 1;
                count += 1;
            }
            result.push(number as u8);
        } else {
            result.push(escaped);
        }
    }
    result.into()
}

pub(super) fn whitespace_fields(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes
        .split(|byte| matches!(byte, b' ' | b'\t' | b'\n'))
        .filter(|field| !field.is_empty())
}

pub(super) fn join(parts: &[BString], separator: &[u8]) -> BString {
    let size = parts.iter().map(|part| part.len()).sum::<usize>()
        + separator.len() * parts.len().saturating_sub(1);
    let mut result = Vec::with_capacity(size);
    for (index, part) in parts.iter().enumerate() {
        if index != 0 {
            result.extend_from_slice(separator);
        }
        result.extend_from_slice(part);
    }
    result.into()
}
