//! JSON support.
use crate::{Map, Num, Rc, Val};
use alloc::{string::ToString, vec::Vec};
use core::fmt::{self, Formatter};
use hifijson::token::{Expect, Lex};
use hifijson::{LexAlloc, Read as _, SliceLexer};
#[cfg(feature = "std")]
use std::io;

/// Eat JSON whitespace, NUL framing, and (for sequence input) record separators.
fn ws_tk_jq<L: Lex>(lexer: &mut L, seq: bool) -> Option<u8> {
    loop {
        lexer.eat_whitespace();
        match lexer.peek_next() {
            Some(b'\0') => {
                lexer.take_next();
            }
            Some(b'\x1e') if seq => {
                lexer.take_next();
            }
            next => return next,
        }
    }
}

/// Eat whitespace/comments, then peek at next character.
fn ws_tk<L: Lex>(lexer: &mut L) -> Option<u8> {
    loop {
        lexer.eat_whitespace();
        match lexer.peek_next() {
            Some(b'#') => lexer.skip_until(|c| c == b'\n'),
            next => return next,
        }
    }
}

/// Parse error.
#[derive(Debug)]
pub struct Error(usize, hifijson::Error);

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "byte offset {}: {}", self.0, self.1)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// Parse exactly one JSON value.
pub fn parse_single(slice: &[u8]) -> Result<Val, Error> {
    let offset = |rest: &[u8]| rest.as_ptr() as usize - slice.as_ptr() as usize;
    let mut lexer = SliceLexer::new(slice);
    lexer
        .exactly_one(ws_tk, parse)
        .map_err(|e| Error(offset(lexer.as_slice()), e))
}

/// Parse a sequence of JSON values.
pub fn parse_many(slice: &[u8]) -> impl Iterator<Item = Result<Val, Error>> + '_ {
    let offset = |rest: &[u8]| rest.as_ptr() as usize - slice.as_ptr() as usize;
    let mut lexer = SliceLexer::new(slice);
    core::iter::from_fn(move || {
        Some(parse(ws_tk(&mut lexer)?, &mut lexer).map_err(|e| Error(offset(lexer.as_slice()), e)))
    })
}

#[cfg(feature = "std")]
/// Read a sequence of JSON values.
pub fn read_many<'a>(read: impl io::BufRead + 'a) -> impl Iterator<Item = io::Result<Val>> + 'a {
    let invalid_data = |e| io::Error::new(io::ErrorKind::InvalidData, e);
    let mut lexer = hifijson::IterLexer::new(read.bytes());
    core::iter::from_fn(move || {
        let v = ws_tk(&mut lexer).map(|next| parse(next, &mut lexer).map_err(invalid_data));
        // always return I/O error if present, regardless of the output value!
        lexer.error.take().map(Err).or(v)
    })
}

/// Parse exactly one jq-compatible JSON value.
///
/// Unlike [`parse_single`], this accepts jq's extended number spellings and
/// NUL framing, while still using the jaq JSON lexer and value builder.
pub fn parse_single_jq(slice: &[u8]) -> Result<Val, Error> {
    let offset = |rest: &[u8]| rest.as_ptr() as usize - slice.as_ptr() as usize;
    let mut lexer = SliceLexer::new(slice);
    lexer
        .exactly_one(|lexer| ws_tk_jq(lexer, false), |next, lexer| {
            parse_with(next, lexer, true)
        })
        .map_err(|e| Error(offset(lexer.as_slice()), e))
}

/// Parse a sequence of jq-compatible JSON values.
pub fn parse_many_jq(slice: &[u8]) -> impl Iterator<Item = Result<Val, Error>> + '_ {
    let offset = |rest: &[u8]| rest.as_ptr() as usize - slice.as_ptr() as usize;
    let mut lexer = SliceLexer::new(slice);
    core::iter::from_fn(move || {
        let next = ws_tk_jq(&mut lexer, false)?;
        Some(parse_with(next, &mut lexer, true).map_err(|e| Error(offset(lexer.as_slice()), e)))
    })
}

#[cfg(feature = "std")]
/// Read a sequence of jq-compatible JSON values.
pub fn read_many_jq<'a>(
    read: impl io::BufRead + 'a,
) -> impl Iterator<Item = io::Result<Val>> + 'a {
    read_many_jq_mode(read, false)
}

#[cfg(feature = "std")]
/// Read a sequence of jq-compatible JSON sequence values.
pub fn read_many_jq_seq<'a>(
    read: impl io::BufRead + 'a,
) -> impl Iterator<Item = io::Result<Val>> + 'a {
    read_many_jq_mode(read, true)
}

#[cfg(feature = "std")]
fn read_many_jq_mode<'a>(
    read: impl io::BufRead + 'a,
    seq: bool,
) -> impl Iterator<Item = io::Result<Val>> + 'a {
    let invalid_data = |e| io::Error::new(io::ErrorKind::InvalidData, e);
    let mut lexer = hifijson::IterLexer::new(read.bytes());
    core::iter::from_fn(move || {
        let v = ws_tk_jq(&mut lexer, seq)
            .map(|next| parse_with(next, &mut lexer, true).map_err(invalid_data));
        if seq && v.as_ref().is_some_and(Result::is_err) {
            while lexer.peek_next().is_some_and(|byte| byte != b'\x1e') {
                lexer.take_next();
            }
        }
        // always return I/O error if present, regardless of the output value!
        lexer.error.take().map(Err).or(v)
    })
}

/// Parse a single number.
pub fn parse_single_num(slice: &[u8]) -> Option<Num> {
    Some(match slice {
        b"Infinity" => Num::Float(f64::INFINITY),
        b"NaN" => Num::Float(f64::NAN),
        _ => {
            let mut lexer = SliceLexer::new(slice);
            parse_num(&mut lexer)
                .ok()
                .filter(|_| lexer.as_slice().is_empty())?
        }
    })
}

/// Parse a JSON string as byte or text string, preserving invalid UTF-8 as-is.
fn parse_string<L: LexAlloc>(lexer: &mut L, bytes: bool) -> Result<Vec<u8>, hifijson::Error> {
    let on_string = |bytes: &mut L::Bytes, out: &mut Vec<u8>| {
        out.extend(bytes.as_ref());
        Ok(())
    };
    let s = lexer.str_fold(Vec::new(), on_string, |lexer, out| {
        use hifijson::escape::Error;
        match lexer.take_next().ok_or(Error::Eof)? {
            b'u' if bytes => Err(Error::InvalidKind(b'u'))?,
            b'x' if bytes => out.push(lexer.hex()?),
            c => match lexer.escape(c) {
                Ok(character) => out.extend(character.encode_utf8(&mut [0; 4]).as_bytes()),
                Err(Error::InvalidChar(_)) if !bytes => out.extend(b"\xef\xbf\xbd"),
                Err(error) => return Err(hifijson::str::Error::from(error)),
            },
        }
        Ok(())
    });
    s.map_err(hifijson::Error::Str)
}

fn parse_num<L: LexAlloc>(lexer: &mut L) -> Result<Num, hifijson::Error> {
    let num = hifijson::num::Num::signed_digits();
    let (num, parts) = lexer.num_string_with(num).unvalidated();
    let num = num.as_ref();
    Ok(match num {
        "+" if lexer.strip_prefix(b"Infinity") => Num::Float(f64::INFINITY),
        "-" if lexer.strip_prefix(b"Infinity") => Num::Float(f64::NEG_INFINITY),
        _ if num.ends_with(|c: char| c.is_ascii_digit()) => {
            if parts.is_int() {
                Num::from_str_radix(num, 10).unwrap()
            } else {
                Num::Dec(num.to_string().into())
            }
        }
        _ => Err(hifijson::num::Error::ExpectedDigit)?,
    })
}

fn parse_jq_value<L: LexAlloc>(lexer: &mut L) -> Result<Val, hifijson::Error> {
    let mut bytes = Vec::new();
    while let Some(byte) = lexer.peek_next() {
        if matches!(
            byte,
            b' ' | b'\t' | b'\r' | b'\n' | b'\0' | b'\x1e' | b'[' | b']' | b'{' | b'}'
                | b',' | b':'
        ) {
            break;
        }
        bytes.push(lexer.take_next().ok_or(hifijson::token::Expect::Value)?);
    }
    let lower = bytes
        .iter()
        .map(u8::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if lower == b"null" {
        return Ok(Val::Null);
    }
    let special = match lower.as_slice() {
        b"nan" | b"+nan" | b"-nan" => Some(f64::NAN),
        b"infinity" | b"+infinity" => Some(f64::INFINITY),
        b"-infinity" => Some(f64::NEG_INFINITY),
        _ => None,
    };
    if let Some(number) = special {
        return Ok(Val::Num(Num::Float(number)));
    }
    if !valid_jq_number(&bytes) {
        return Err(hifijson::num::Error::ExpectedDigit.into());
    }
    let text = core::str::from_utf8(&bytes)
        .map_err(|_| hifijson::num::Error::ExpectedDigit)?;
    let unsigned = text.strip_prefix('+').unwrap_or(text);
    let decimal = unsigned.contains(['.', 'e', 'E']);
    let number = if decimal {
        Num::Dec(Rc::new(unsigned.to_string()))
    } else if unsigned.starts_with('-')
        && unsigned.as_bytes()[1..].iter().all(|byte| *byte == b'0')
    {
        Num::Float(-0.0)
    } else {
        Num::from_str_radix(unsigned, 10).ok_or(hifijson::num::Error::ExpectedDigit)?
    };
    Ok(Val::Num(number))
}

fn valid_jq_number(bytes: &[u8]) -> bool {
    let mut index = usize::from(matches!(bytes.first(), Some(& (b'+' | b'-'))));
    let digits_before = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    let before = index - digits_before;
    let mut after = 0;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
            after += 1;
        }
    }
    if before == 0 && after == 0 {
        return false;
    }
    if matches!(bytes.get(index), Some(& (b'e' | b'E'))) {
        index += 1;
        if matches!(bytes.get(index), Some(& (b'+' | b'-'))) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len()
}


/// Parse a JSON value, given an initial non-whitespace character and a lexer.
///
/// If the underlying lexer reads input fallibly (for example [`hifijson::IterLexer`]),
/// the error returned by this function might be misleading.
/// In that case, always check whether the lexer contains an error.
fn parse<L: LexAlloc>(next: u8, lexer: &mut L) -> Result<Val, hifijson::Error> {
    parse_with(next, lexer, false)
}

fn parse_with<L: LexAlloc>(
    next: u8,
    lexer: &mut L,
    jq: bool,
) -> Result<Val, hifijson::Error> {
    Ok(match next {
        b'n' if !jq && lexer.strip_prefix(b"null") => Val::Null,
        b't' if lexer.strip_prefix(b"true") => Val::Bool(true),
        b'f' if lexer.strip_prefix(b"false") => Val::Bool(false),
        b'b' if lexer.strip_prefix(b"b\"") => Val::byte_str(parse_string(lexer, true)?),
        b'N' if !jq && lexer.strip_prefix(b"NaN") => Val::Num(Num::Float(f64::NAN)),
        b'I' if !jq && lexer.strip_prefix(b"Infinity") => Val::Num(Num::Float(f64::INFINITY)),
        b'.' | b'0'..=b'9' | b'+' | b'-' | b'n' | b'N' | b'i' | b'I' if jq => {
            parse_jq_value(lexer)?
        }
        b'0'..=b'9' | b'+' | b'-' => Val::Num(parse_num(lexer)?),
        b'"' => Val::utf8_str(parse_string(lexer.discarded(), false)?),
        b'[' => Val::Arr({
            let mut arr = Vec::new();
            if jq {
                lexer
                    .discarded()
                    .seq(b']', |lexer| ws_tk_jq(lexer, false), |next, lexer| {
                        arr.push(parse_with(next, lexer, true)?);
                        Ok::<_, hifijson::Error>(())
                    })?;
            } else {
                lexer.discarded().seq(b']', ws_tk, |next, lexer| {
                    arr.push(parse(next, lexer)?);
                    Ok::<_, hifijson::Error>(())
                })?;
            }
            arr.into()
        }),
        b'{' => Val::obj({
            let mut obj = Map::default();
            if jq {
                lexer
                    .discarded()
                    .seq(b'}', |lexer| ws_tk_jq(lexer, false), |next, lexer| {
                        let key = parse_with(next, lexer, true)?;
                        lexer
                            .expect(|lexer| ws_tk_jq(lexer, false), b':')
                            .ok_or(Expect::Colon)?;
                        let value = parse_with(
                            ws_tk_jq(lexer, false).ok_or(Expect::Value)?,
                            lexer,
                            true,
                        )?;
                        obj.insert(key, value);
                        Ok::<_, hifijson::Error>(())
                    })?;
            } else {
                lexer.discarded().seq(b'}', ws_tk, |next, lexer| {
                    let key = parse(next, lexer)?;
                    lexer.expect(ws_tk, b':').ok_or(Expect::Colon)?;
                    let value = parse(ws_tk(lexer).ok_or(Expect::Value)?, lexer)?;
                    obj.insert(key, value);
                    Ok::<_, hifijson::Error>(())
                })?;
            }
            obj
        }),
        _ => Err(Expect::Value)?,
    })
}
