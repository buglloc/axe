use crate::bre::parse_escape_value;
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

pub(crate) fn compile_replacement(replacement: &str) -> Vec<ReplacePart> {
    let chars: Vec<char> = replacement.chars().collect();
    let mut parts = Vec::new();
    let mut literal = Vec::new();
    let mut index = 0;
    let flush = |literal: &mut Vec<u8>, parts: &mut Vec<ReplacePart>| {
        if !literal.is_empty() {
            parts.push(ReplacePart::Literal(std::mem::take(literal)));
        }
    };
    while index < chars.len() {
        let ch = chars[index];
        index += 1;
        if ch == '&' {
            flush(&mut literal, &mut parts);
            parts.push(ReplacePart::Group(0));
            continue;
        }
        if ch != '\\' {
            let mut bytes = [0; 4];
            literal.extend_from_slice(ch.encode_utf8(&mut bytes).as_bytes());
            continue;
        }
        let Some(&next) = chars.get(index) else {
            literal.push(b'\\');
            break;
        };
        index += 1;
        let part = match next {
            '0'..='9' => Some(ReplacePart::Group((next as u8 - b'0') as usize)),
            'L' => Some(ReplacePart::Lower),
            'U' => Some(ReplacePart::Upper),
            'E' => Some(ReplacePart::EndCase),
            'l' => Some(ReplacePart::LowerNext),
            'u' => Some(ReplacePart::UpperNext),
            _ => None,
        };
        if let Some(part) = part {
            flush(&mut literal, &mut parts);
            parts.push(part);
            continue;
        }
        match next {
            'n' => literal.push(b'\n'),
            't' => literal.push(b'\t'),
            'r' => literal.push(b'\r'),
            'a' => literal.push(7),
            'f' => literal.push(12),
            'v' => literal.push(11),
            'x' | 'o' | 'd' => {
                if let Some(value) = parse_escape_value(next, &chars, &mut index) {
                    literal.push(value as u8);
                } else {
                    literal.push(next as u8);
                }
            }
            _ => {
                let mut bytes = [0; 4];
                literal.extend_from_slice(next.encode_utf8(&mut bytes).as_bytes());
            }
        }
    }
    flush(&mut literal, &mut parts);
    parts
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
        output.extend_from_slice(&text[copied..])
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
                Err(error) => (
                    std::str::from_utf8(&bytes[..error.valid_up_to()])
                        .expect("validated UTF-8 prefix"),
                    error
                        .error_len()
                        .unwrap_or(bytes.len() - error.valid_up_to()),
                ),
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
