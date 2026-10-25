use crate::error::Result;
use crate::regex::{Captures, PosixRegex};

pub(crate) enum ReplacePart {
    Literal(Vec<u8>),
    Group(usize),
    Lower,
    Upper,
    EndCase,
    LowerNext,
    UpperNext,
}

pub(crate) fn compile_replacement(replacement: &[u8]) -> Vec<ReplacePart> {
    let mut parts = Vec::new();
    let mut literal = Vec::new();
    let mut index = 0;
    let flush = |literal: &mut Vec<u8>, parts: &mut Vec<ReplacePart>| {
        if !literal.is_empty() {
            parts.push(ReplacePart::Literal(std::mem::take(literal)));
        }
    };
    while index < replacement.len() {
        let byte = replacement[index];
        index += 1;
        if byte == b'&' {
            flush(&mut literal, &mut parts);
            parts.push(ReplacePart::Group(0));
            continue;
        }
        if byte != b'\\' {
            literal.push(byte);
            continue;
        }
        let Some(&next) = replacement.get(index) else {
            literal.push(b'\\');
            break;
        };
        index += 1;
        let part = match next {
            b'0'..=b'9' => Some(ReplacePart::Group((next - b'0') as usize)),
            b'L' => Some(ReplacePart::Lower),
            b'U' => Some(ReplacePart::Upper),
            b'E' => Some(ReplacePart::EndCase),
            b'l' => Some(ReplacePart::LowerNext),
            b'u' => Some(ReplacePart::UpperNext),
            _ => None,
        };
        if let Some(part) = part {
            flush(&mut literal, &mut parts);
            parts.push(part);
            continue;
        }
        match next {
            b'n' => literal.push(b'\n'),
            b't' => literal.push(b'\t'),
            b'r' => literal.push(b'\r'),
            b'a' => literal.push(7),
            b'f' => literal.push(12),
            b'v' => literal.push(11),
            b'c' => {
                if let Some(&control) = replacement.get(index) {
                    index += 1;
                    literal.push(control & 0x1f);
                } else {
                    literal.push(b'c');
                }
            }
            b'x' | b'o' | b'd' => {
                let (radix, max) = match next {
                    b'x' => (16, 2),
                    b'o' => (8, 3),
                    _ => (10, 3),
                };
                let start = index;
                let mut value = 0u32;
                let mut count = 0;
                while count < max {
                    let Some(&digit) = replacement.get(index) else { break };
                    let Some(digit) = digit_value(digit, radix) else { break };
                    value = value * radix as u32 + digit as u32;
                    index += 1;
                    count += 1;
                }
                if count == 0 {
                    literal.push(next);
                    index = start;
                } else {
                    literal.push((value & 0xff) as u8);
                }
            }
            _ => literal.push(next),
        }
    }
    flush(&mut literal, &mut parts);
    parts
}

fn digit_value(byte: u8, radix: u8) -> Option<u8> {
    let value = match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => return None,
    };
    (value < radix).then_some(value)
}

pub(crate) fn apply_substitution(
    regex: &PosixRegex,
    text: &[u8],
    replacement: &[ReplacePart],
    global: bool,
    nth: Option<usize>,
) -> Result<Option<Vec<u8>>> {
    let mut output = None;
    let mut captures = Captures::new();
    let mut matcher = regex.matcher(text);
    let mut from = 0;
    let mut copied = 0;
    let mut previous_end = None;
    let mut count = 0;
    while from <= text.len() && matcher.captures_at(from, &mut captures)? {
        let Some((start, end)) = captures.pos(0) else {
            break;
        };
        if start == end && previous_end == Some(start) {
            from = matcher.advance(end);
            continue;
        }
        previous_end = Some(end);
        count += 1;
        let replace = match nth {
            Some(n) => count == n || (global && count > n),
            None => global || count == 1,
        };
        if replace {
            let result = output.get_or_insert_with(|| Vec::with_capacity(text.len()));
            result.extend_from_slice(&text[copied..start]);
            render_replacement(text, &captures, replacement, result, regex.unicode_case());
            copied = end;
            if !global {
                break;
            }
        }
        from = if start == end {
            matcher.advance(end)
        } else {
            end
        };
    }
    if let Some(output) = &mut output {
        output.extend_from_slice(&text[copied..]);
    }
    Ok(output)
}

fn render_replacement(
    text: &[u8],
    captures: &Captures,
    parts: &[ReplacePart],
    output: &mut Vec<u8>,
    unicode: bool,
) {
    let mut lower = None;
    let mut next_lower = None;
    for part in parts {
        let bytes = match part {
            ReplacePart::Literal(bytes) => bytes.as_slice(),
            ReplacePart::Group(group) => {
                let Some((start, end)) = captures.pos(*group) else {
                    continue;
                };
                &text[start..end]
            }
            ReplacePart::Lower => {
                lower = Some(true);
                continue;
            }
            ReplacePart::Upper => {
                lower = Some(false);
                continue;
            }
            ReplacePart::EndCase => {
                lower = None;
                next_lower = None;
                continue;
            }
            ReplacePart::LowerNext => {
                next_lower = Some(true);
                continue;
            }
            ReplacePart::UpperNext => {
                next_lower = Some(false);
                continue;
            }
        };
        append_case(bytes, lower, &mut next_lower, output, unicode);
    }
}

fn append_case(
    mut bytes: &[u8],
    lower: Option<bool>,
    next_lower: &mut Option<bool>,
    output: &mut Vec<u8>,
    unicode: bool,
) {
    if lower.is_none() && next_lower.is_none() {
        output.extend_from_slice(bytes);
        return;
    }
    while !bytes.is_empty() {
        let (text, invalid) = if unicode {
            match std::str::from_utf8(bytes) {
                Ok(text) => (text, 0),
                Err(error) => {
                    let valid_end = error.valid_up_to();
                    let text = match std::str::from_utf8(&bytes[..valid_end]) {
                        Ok(text) => text,
                        Err(_) => "",
                    };
                    (
                        text,
                        error
                            .error_len()
                            .unwrap_or(bytes.len() - valid_end),
                    )
                }
            }
        } else {
            ("", bytes.len())
        };
        for ch in text.chars() {
            let ch = match next_lower.take().or(lower) {
                Some(true) => ch.to_lowercase().next().unwrap_or(ch),
                Some(false) => {
                    let mut uppercase = ch.to_uppercase();
                    let first = uppercase.next().unwrap_or(ch);
                    if uppercase.next().is_none() {
                        first
                    } else {
                        ch
                    }
                }
                None => ch,
            };
            let mut encoded = [0; 4];
            output.extend_from_slice(ch.encode_utf8(&mut encoded).as_bytes());
        }
        for &byte in &bytes[text.len()..text.len() + invalid] {
            output.push(match next_lower.take().or(lower) {
                Some(true) => byte.to_ascii_lowercase(),
                Some(false) => byte.to_ascii_uppercase(),
                None => byte,
            });
        }
        bytes = &bytes[text.len() + invalid..];
    }
}
