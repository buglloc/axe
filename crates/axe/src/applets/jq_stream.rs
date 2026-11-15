use std::io::{self, BufRead};

use super::{Num, Val, json, normalize_input};

pub(super) trait Source: BufRead {
    fn file_index(&self) -> usize;
}

#[derive(Clone, Copy, PartialEq)]
enum Last {
    None,
    Array,
    Object,
    Colon,
    Comma,
    Value,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Normal,
    String,
    Escape,
}

// Only the open-container path and current scalar are retained. In particular,
// closing a container never constructs the container's complete JSON value.
pub(super) struct Stream<R> {
    reader: R,
    path: Vec<Val>,
    token: Vec<u8>,
    next: Option<Val>,
    output: Option<Val>,
    end: Option<Val>,
    last: Last,
    state: State,
    seq: bool,
    errors: bool,
    waiting_rs: bool,
    discard_record: bool,
    finished: bool,
    line: usize,
    column: usize,
    file_index: usize,
    record_position: usize,
}

impl<R: Source> Stream<R> {
    pub(super) fn new(reader: R, seq: bool, errors: bool) -> Self {
        Self {
            reader,
            path: Vec::new(),
            token: Vec::new(),
            next: None,
            output: None,
            end: None,
            last: Last::None,
            state: State::Normal,
            seq,
            errors,
            waiting_rs: seq,
            discard_record: false,
            finished: false,
            line: 1,
            column: 0,
            file_index: 0,
            record_position: 0,
        }
    }

    fn array(values: Vec<Val>) -> Val {
        Val::Arr(json::Rc::new(values))
    }

    fn path(&self) -> Val {
        Self::array(self.path.clone())
    }

    fn leaf(&self, value: Val) -> Val {
        Self::array(vec![self.path(), value])
    }

    fn take_output(&mut self) -> Option<Val> {
        let output = self.output.take();
        self.output = self.end.take();
        output
    }

    fn reset(&mut self) {
        self.path.clear();
        self.token.clear();
        self.next = None;
        self.output = None;
        self.end = None;
        self.last = Last::None;
        self.state = State::Normal;
    }

    fn error(&mut self, message: &str, eof: bool, rs: bool) -> io::Result<Val> {
        let message = format!(
            "{message}{} at line {}, column {}{}",
            if eof { " at EOF" } else { "" },
            self.line,
            self.column,
            if self.seq && !eof && !rs {
                " (need RS to resync)"
            } else {
                ""
            }
        );
        let path = self.path();
        self.reset();
        self.waiting_rs = false;
        self.discard_record = !self.seq && !eof && self.record_position != 0;
        if self.errors {
            Ok(Self::array(vec![Val::utf8_str(message), path]))
        } else {
            Err(io::Error::new(io::ErrorKind::InvalidData, message))
        }
    }

    fn value(&mut self, value: Val) -> Result<(), &'static str> {
        if self.next.is_some() || self.last == Last::Value {
            return Err("Expected separator between values");
        }
        self.last = if self.path.is_empty() {
            Last::None
        } else {
            Last::Value
        };
        self.next = Some(value);
        Ok(())
    }

    fn literal(&mut self) -> Result<(), &'static str> {
        let token = if self.state == State::Normal {
            self.token.as_slice()
        } else {
            &self.token[1..]
        };
        if token.is_empty() {
            return Ok(());
        }
        let message = match token {
            [b't' | b'f', ..] | [b'n', b'u', ..] => "Invalid literal",
            [b'\'', ..] => "Invalid string literal; expected \", but got '",
            _ => "Invalid numeric literal",
        };
        let value = json::read::parse_single_jq(token).map_err(|_| message)?;
        let value = normalize_input(value).map_err(|_| message)?;
        if matches!(value, Val::Null) && token != b"null" {
            return Err(message);
        }
        self.token.clear();
        self.value(value)
    }

    fn string(&mut self) -> Result<(), &'static str> {
        let value = json::read::parse_single_jq(&self.token)
            .map_err(|_| string_error(&self.token))?;
        let value = normalize_input(value).map_err(|_| string_error(&self.token))?;
        self.token.clear();
        self.value(value)
    }

    fn top_value(&mut self) {
        if self.path.is_empty() {
            if let Some(value) = self.next.take() {
                self.output = Some(self.leaf(value));
            }
        }
    }

    fn structure(&mut self, byte: u8) -> Result<(), &'static str> {
        match byte {
            b'[' | b'{' => {
                if self.next.is_some() || self.last == Last::Value {
                    return Err("Expected a separator between values");
                }
                if self.last == Last::Object {
                    return Err(if byte == b'[' {
                        "Expected string key after '{', not '['"
                    } else {
                        "Expected string key after '{', not '{'"
                    });
                }
                if self.last == Last::Comma
                    && !matches!(self.path.last(), Some(Val::Num(_)))
                {
                    return Err(if byte == b'[' {
                        "Expected string key after ',' in object, not '['"
                    } else {
                        "Expected string key after ',' in object, not '{'"
                    });
                }
                self.path.push(if byte == b'[' {
                    Val::Num(Num::Int(0))
                } else {
                    Val::Null
                });
                self.last = if byte == b'[' { Last::Array } else { Last::Object };
            }
            b':' => {
                if matches!(self.path.last(), None | Some(Val::Num(_))) {
                    return Err("':' not as part of an object");
                }
                let Some(key) = self.next.as_ref() else {
                    return Err("Expected string key before ':'");
                };
                if !matches!(key, Val::TStr(_)) {
                    return Err("Object keys must be strings");
                }
                if self.last != Last::Value {
                    return Err("':' should follow a key");
                }
                *self.path.last_mut().expect("nonempty path") =
                    self.next.take().expect("key checked");
                self.last = Last::Colon;
            }
            b',' => {
                if self.last != Last::Value {
                    return Err("Expected value before ','");
                }
                let Some(key) = self.path.last() else {
                    return Err("',' not as part of an object or array");
                };
                if !matches!(key, Val::Num(_) | Val::TStr(_)) {
                    return Err("Objects must consist of key:value pairs");
                }
                if let Some(value) = self.next.take() {
                    self.output = Some(self.leaf(value));
                }
                let key = self.path.last_mut().expect("nonempty path");
                *key = match key {
                    Val::Num(Num::Int(index)) => Val::Num(Num::Int(*index + 1)),
                    _ => Val::Null,
                };
                self.last = Last::Comma;
            }
            b']' | b'}' => {
                let array = byte == b']';
                let Some(key) = self.path.last() else {
                    return Err(if array {
                        "Unmatched ']' at the top-level"
                    } else {
                        "Unmatched '}' at the top-level"
                    });
                };
                if self.last == Last::Comma {
                    return Err(if array {
                        "Expected another array element"
                    } else {
                        "Expected another key:value pair"
                    });
                }
                if array != matches!(key, Val::Num(_)) {
                    return Err(if array {
                        "Unmatched ']' in the middle of an object"
                    } else {
                        "Unmatched '}' in the middle of an array"
                    });
                }
                let empty = self.last == if array { Last::Array } else { Last::Object };
                if let Some(value) = self.next.take() {
                    if !array && !matches!(self.path.last(), Some(Val::TStr(_))) {
                        return Err("Objects must consist of key:value pairs");
                    }
                    let path = self.path();
                    self.output = Some(Self::array(vec![path.clone(), value]));
                    self.end = Some(Self::array(vec![path]));
                } else if !empty {
                    if !array && self.last == Last::Colon {
                        return Err("Missing value in key:value pair");
                    }
                    if !array && self.last != Last::Value {
                        return Err("Unmatched '}'");
                    }
                    self.output = Some(Self::array(vec![self.path()]));
                }
                self.path.pop();
                if empty {
                    let value = if array {
                        Self::array(Vec::new())
                    } else {
                        Val::obj(json::Map::default())
                    };
                    self.output = Some(self.leaf(value));
                }
                self.last = if self.path.is_empty() { Last::None } else { Last::Value };
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn scan(&mut self, byte: u8) -> Result<(), &'static str> {
        if self.seq && byte == b'\x1e' {
            if !self.path.is_empty() {
                return Err("Truncated value");
            }
            self.literal()?;
            if self.state == State::Normal {
                self.top_value();
                self.last = Last::None;
            } else {
                self.reset();
            }
            return Ok(());
        }
        if self.state != State::Normal {
            self.token.push(byte);
            if byte == b'"' && self.state == State::String {
                self.string()?;
                self.state = State::Normal;
                self.top_value();
            } else if byte == b'\\' && self.state == State::String {
                self.state = State::Escape;
            } else {
                self.state = State::String;
            }
            return Ok(());
        }
        if matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b'"' | b'[' | b']' | b'{' | b'}' | b',' | b':') {
            self.literal()?;
            self.top_value();
            match byte {
                b'"' => {
                    self.token.push(byte);
                    self.state = State::String;
                }
                b'[' | b']' | b'{' | b'}' | b',' | b':' => self.structure(byte)?,
                _ => {}
            }
        } else {
            self.token.push(byte);
        }
        Ok(())
    }

    fn eof(&mut self) -> Option<io::Result<Val>> {
        self.finished = true;
        let message = if self.waiting_rs {
            Some("Unfinished abandoned text")
        } else if self.state != State::Normal {
            Some("Unfinished string")
        } else if let Err(message) = self.literal() {
            Some(message)
        } else if !self.path.is_empty() {
            Some("Unfinished JSON term")
        } else {
            None
        };
        if let Some(message) = message {
            return Some(self.error(message, true, false));
        }
        self.top_value();
        self.take_output().map(Ok)
    }
}

impl<R: Source> Iterator for Stream<R> {
    type Item = io::Result<Val>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(output) = self.take_output() {
            return Some(Ok(output));
        }
        if self.finished {
            return None;
        }
        loop {
            let byte = match self.reader.fill_buf() {
                Ok([]) => return self.eof(),
                Ok(bytes) => bytes[0],
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            };
            self.reader.consume(1);
            let file_index = self.reader.file_index();
            if file_index != self.file_index {
                self.file_index = file_index;
                self.record_position = 0;
                self.discard_record = false;
            }
            // jq abandons the current fgets record (up to 4095 bytes) on error,
            // without advancing the parser's diagnostic line/column counters.
            self.record_position += 1;
            if byte == b'\n' || self.record_position == 4095 {
                self.record_position = 0;
            }
            if self.discard_record {
                if self.record_position == 0 {
                    self.discard_record = false;
                }
                continue;
            }
            if byte == b'\n' {
                self.line += 1;
                self.column = 0;
            } else {
                self.column += 1;
            }
            if self.waiting_rs {
                if byte == b'\x1e' {
                    self.waiting_rs = false;
                }
                continue;
            }
            if let Err(message) = self.scan(byte) {
                return Some(self.error(message, false, byte == b'\x1e'));
            }
            if let Some(output) = self.take_output() {
                return Some(Ok(output));
            }
        }
    }
}

fn string_error(token: &[u8]) -> &'static str {
    let token = &token[..token.len().saturating_sub(1)];
    let mut index = 1;
    while index < token.len() {
        let byte = token[index];
        index += 1;
        if byte < 0x20 {
            return "Invalid string: control characters from U+0000 through U+001F must be escaped";
        }
        if byte != b'\\' {
            continue;
        }
        let Some(&escape) = token.get(index) else {
            return "Expected escape character at end of string";
        };
        index += 1;
        match escape {
            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
            b'u' => {
                let Some(hex) = token.get(index..index + 4) else {
                    return "Invalid \\uXXXX escape";
                };
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return "Invalid characters in \\uXXXX escape";
                }
                let code = hex.iter().fold(0_u16, |code, byte| {
                    code * 16 + char::from(*byte).to_digit(16).unwrap_or(0) as u16
                });
                index += 4;
                if (0xd800..=0xdbff).contains(&code) {
                    let Some(pair) = token.get(index..index + 6) else {
                        return "Invalid \\uXXXX\\uXXXX surrogate pair escape";
                    };
                    if pair[..2] != *b"\\u" || !pair[2..].iter().all(u8::is_ascii_hexdigit) {
                        return "Invalid \\uXXXX\\uXXXX surrogate pair escape";
                    }
                    let low = pair[2..].iter().fold(0_u16, |code, byte| {
                        code * 16 + char::from(*byte).to_digit(16).unwrap_or(0) as u16
                    });
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return "Invalid \\uXXXX\\uXXXX surrogate pair escape";
                    }
                    index += 6;
                }
            }
            _ => return "Invalid escape",
        }
    }
    "Invalid string"
}
