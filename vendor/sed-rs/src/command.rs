use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Address {
    /// A specific line number (1-indexed)
    Line(usize),
    /// The last line of input
    Last,
    /// Lines matching a regex pattern
    Regex(Vec<u8>),
    /// GNU extension: first~step (every step-th line starting at first)
    Step { first: usize, step: usize },
    /// GNU extension: an endpoint N lines after the first address
    Plus(usize),
    /// GNU extension: an endpoint at the next line number divisible by N
    Tilde(usize),
}

#[derive(Debug, Clone)]
pub enum AddressRange {
    /// No address — matches every line
    None,
    /// Single address, optionally negated
    Single { addr: Address, negated: bool },
    /// Range of two addresses (inclusive), optionally negated
    Range {
        start: Address,
        end: Address,
        negated: bool,
    },
}

#[derive(Debug, Clone)]
pub struct SubstituteCmd {
    pub pattern: Vec<u8>,
    pub replacement: Vec<u8>,
    pub global: bool,
    pub print: bool,
    pub nth: Option<usize>,
    pub case_insensitive: bool,
    pub write_file: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub enum Command {
    // -- Substitution & Transliteration --
    Substitute(SubstituteCmd),
    Transliterate {
        from: Vec<u8>,
        to: Vec<u8>,
    },

    // -- Output --
    Print,
    PrintFirstLine,
    PrintLineNumber,
    List,

    // -- Deletion --
    Delete,
    DeleteFirstLine,

    // -- Input --
    Next,
    NextAppend,

    // -- Hold space --
    HoldReplace,
    HoldAppend,
    GetReplace,
    GetAppend,
    Exchange,

    // -- Text insertion --
    Append(Vec<u8>),
    Insert(Vec<u8>),
    Change(Vec<u8>),

    // -- Branching --
    Label(Vec<u8>),
    Branch(Option<Vec<u8>>),
    BranchIfSub(Option<Vec<u8>>),
    BranchIfNotSub(Option<Vec<u8>>),

    // -- I/O --
    ReadFile(Vec<u8>),
    /// GNU extension: read one line per cycle from a file (`R`)
    ReadLine(Vec<u8>),
    WriteFile(Vec<u8>),
    WriteFirstLine(Vec<u8>),
    /// GNU extension: print the current input file name (`F`)
    PrintFileName,
    /// GNU extension: execute a shell command (`e`).
    /// `None` runs the pattern space and replaces it with the output;
    /// `Some(cmd)` runs `cmd` and sends its output to the stream.
    Execute(Option<Vec<u8>>),

    // -- Control --
    Quit(Option<i32>),
    QuitNoprint(Option<i32>),
    ClearPattern,
    Noop,

    // -- Grouping --
    Block(Vec<SedCommand>),
}

#[derive(Debug, Clone)]
pub struct SedCommand {
    pub address: AddressRange,
    pub command: Command,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse a sed program without requiring it to be valid UTF-8.
///
/// `str` and byte slices are both accepted for the benefit of the library API;
/// command-line scripts are passed here as their original argument bytes.
pub fn parse<S: AsRef<[u8]>>(script: S) -> Result<Vec<SedCommand>> {
    let mut parser = Parser::new(script.as_ref());
    parser.parse_script()
}

struct Parser {
    bytes: Vec<u8>,
    pos: usize,
}

impl Parser {
    fn new(input: &[u8]) -> Self {
        Self {
            bytes: input.to_vec(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.pos).copied();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    fn is_at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn consume_if(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Skip spaces and tabs (but NOT newlines).
    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.advance();
        }
    }

    /// Skip whitespace including newlines, semicolons, and comments.
    fn skip_blanks(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r' | b';') => {
                    self.advance();
                }
                Some(b'#') => self.skip_line(),
                _ => break,
            }
        }
    }

    /// Skip to end of current line.
    fn skip_line(&mut self) {
        while let Some(byte) = self.advance() {
            if byte == b'\n' {
                break;
            }
        }
    }

    // -- Top-level parsing --

    fn parse_script(&mut self) -> Result<Vec<SedCommand>> {
        let mut commands = Vec::new();
        loop {
            self.skip_blanks();
            if self.is_at_end() {
                break;
            }
            if let Some(cmd) = self.parse_one_command()? {
                commands.push(cmd);
            }
        }
        Ok(commands)
    }

    fn parse_one_command(&mut self) -> Result<Option<SedCommand>> {
        self.skip_blanks();
        if self.is_at_end() {
            return Ok(None);
        }

        let address = self.parse_address_range()?;
        self.skip_spaces();
        let has_address = !matches!(address, AddressRange::None);
        let reject_address = |message: &str| -> Result<()> {
            if has_address {
                Err(Error::Parse(message.into()))
            } else {
                Ok(())
            }
        };

        let Some(byte) = self.advance() else {
            reject_address("missing command")?;
            return Ok(None);
        };
        let command = match byte {
            b'{' => Command::Block(self.parse_block()?),
            b'}' => {
                reject_address("missing command")?;
                return Ok(None);
            }
            b's' => self.parse_substitute()?,
            b'y' => self.parse_transliterate()?,
            b'd' => Command::Delete,
            b'D' => Command::DeleteFirstLine,
            b'p' => Command::Print,
            b'P' => Command::PrintFirstLine,
            b'=' => Command::PrintLineNumber,
            b'l' => Command::List,
            b'q' => Command::Quit(self.parse_optional_int()),
            b'Q' => Command::QuitNoprint(self.parse_optional_int()),
            b'h' => Command::HoldReplace,
            b'H' => Command::HoldAppend,
            b'g' => Command::GetReplace,
            b'G' => Command::GetAppend,
            b'x' => Command::Exchange,
            b'n' => Command::Next,
            b'N' => Command::NextAppend,
            b'z' => Command::ClearPattern,
            b'a' => Command::Append(self.parse_text_arg()),
            b'i' => Command::Insert(self.parse_text_arg()),
            b'c' => Command::Change(self.parse_text_arg()),
            b'r' => Command::ReadFile(self.parse_filename_arg()),
            b'w' => Command::WriteFile(self.parse_filename_arg()),
            b'W' => Command::WriteFirstLine(self.parse_filename_arg()),
            b'R' => Command::ReadLine(self.parse_filename_arg()),
            b'F' => Command::PrintFileName,
            b'e' => {
                let arg = self.parse_rest_of_line();
                Command::Execute((!arg.is_empty()).then_some(arg))
            }
            // GNU `v [version]` asserts a minimum sed version and enables
            // GNU extensions. They are always on here, so it's a no-op.
            b'v' => {
                let _ = self.parse_label_arg();
                Command::Noop
            }
            b'b' => Command::Branch(self.parse_label_arg()),
            b't' => Command::BranchIfSub(self.parse_label_arg()),
            b'T' => Command::BranchIfNotSub(self.parse_label_arg()),
            b':' => {
                reject_address(": doesn't want any addresses")?;
                let label = self.parse_label_arg().unwrap_or_default();
                Command::Label(label)
            }
            b'#' => {
                reject_address("comments don't accept any addresses")?;
                self.skip_line();
                Command::Noop
            }
            b';' => {
                reject_address("missing command")?;
                return self.parse_one_command();
            }
            byte if byte.is_ascii_whitespace() => {
                reject_address("missing command")?;
                return self.parse_one_command();
            }
            byte => {
                return Err(Error::Parse(format!(
                    "unknown command: {:?}",
                    String::from_utf8_lossy(&[byte])
                )));
            }
        };

        Ok(Some(SedCommand { address, command }))
    }

    // -- Address parsing --

    fn parse_address_range(&mut self) -> Result<AddressRange> {
        let Some(addr1) = self.parse_address()? else {
            if self.consume_if(b'!') {
                return Err(Error::Parse("'!' without preceding address".into()));
            }
            return Ok(AddressRange::None);
        };

        self.skip_spaces();
        if self.consume_if(b',') {
            self.skip_spaces();
            let addr2 = match self.peek() {
                Some(b'+') => {
                    self.advance();
                    Address::Plus(self.parse_number())
                }
                Some(b'~') => {
                    self.advance();
                    Address::Tilde(self.parse_number())
                }
                _ => self
                    .parse_address()?
                    .ok_or_else(|| Error::Parse("expected address after ','".into()))?,
            };
            self.skip_spaces();
            let negated = self.consume_if(b'!');
            Ok(AddressRange::Range {
                start: addr1,
                end: addr2,
                negated,
            })
        } else {
            let negated = self.consume_if(b'!');
            Ok(AddressRange::Single {
                addr: addr1,
                negated,
            })
        }
    }

    fn parse_address(&mut self) -> Result<Option<Address>> {
        match self.peek() {
            Some(byte) if byte.is_ascii_digit() => {
                let n = self.parse_number();
                if self.consume_if(b'~') {
                    let step = self.parse_number();
                    Ok(Some(Address::Step { first: n, step }))
                } else {
                    Ok(Some(Address::Line(n)))
                }
            }
            Some(b'$') => {
                self.advance();
                Ok(Some(Address::Last))
            }
            Some(b'/') => {
                self.advance();
                let pattern = self.parse_regex_delimited(b'/', false)?;
                Ok(Some(Address::Regex(pattern)))
            }
            Some(b'\\') => {
                self.advance();
                let delim = self
                    .advance()
                    .ok_or_else(|| Error::Parse("expected delimiter after '\\'".into()))?;
                let pattern = self.parse_regex_delimited(delim, false)?;
                Ok(Some(Address::Regex(pattern)))
            }
            _ => Ok(None),
        }
    }

    fn parse_number(&mut self) -> usize {
        let mut number = 0usize;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_digit() {
                number = number
                    .saturating_mul(10)
                    .saturating_add((byte - b'0') as usize);
                self.advance();
            } else {
                break;
            }
        }
        number
    }

    fn parse_optional_int(&mut self) -> Option<i32> {
        self.skip_spaces();
        if self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            Some(self.parse_number().min(i32::MAX as usize) as i32)
        } else {
            None
        }
    }

    // -- Delimited content parsing --

    /// Decode one GNU sed escape after a backslash. Unknown escapes return
    /// `None` so callers can preserve regex/replacement metacharacters.
    fn decode_escape(&mut self) -> Option<Vec<u8>> {
        let next = self.advance()?;
        match next {
            b'a' => Some(vec![7]),
            b'f' => Some(vec![12]),
            b'n' => Some(vec![b'\n']),
            b'r' => Some(vec![b'\r']),
            b't' => Some(vec![b'\t']),
            b'v' => Some(vec![11]),
            b'c' => self.advance().map(|byte| vec![byte & 0x1f]),
            b'x' => self.parse_escape_digits(16, 2),
            b'o' => self.parse_escape_digits(8, 3),
            b'd' => self.parse_escape_digits(10, 3),
            b'0' => self.parse_escape_digits(8, 3).or_else(|| Some(vec![0])),
            b'\\' => Some(vec![b'\\']),
            _ => None,
        }
    }

    fn parse_escape_digits(&mut self, radix: u8, max: usize) -> Option<Vec<u8>> {
        let start = self.pos;
        let mut value = 0u32;
        let mut count = 0;
        while count < max {
            let Some(byte) = self.peek() else { break };
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                _ => break,
            };
            if digit >= radix {
                break;
            }
            value = value.saturating_mul(radix as u32).saturating_add(digit as u32);
            self.advance();
            count += 1;
        }
        if count == 0 {
            self.pos = start;
            None
        } else {
            Some(vec![(value & 0xff) as u8])
        }
    }

    /// Parse content between matching delimiters, handling GNU byte escapes.
    fn parse_regex_delimited(&mut self, delim: u8, decode_backspace: bool) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        loop {
            match self.advance() {
                None => {
                    return Err(Error::Parse(format!(
                        "unterminated regex (expected closing {:?})",
                        String::from_utf8_lossy(&[delim])
                    )));
                }
                Some(byte) if byte == delim => return Ok(output),
                Some(b'\\') => {
                    if self.peek() == Some(delim) {
                        if decode_backspace {
                            output.push(delim);
                        } else {
                            output.push(b'\\');
                            output.push(delim);
                        }
                        self.advance();
                    } else if !decode_backspace && self.peek() == Some(b'b') {
                        output.extend_from_slice(b"\\b");
                        self.advance();
                    } else {
                        let before = self.pos;
                        match self.decode_escape() {
                            Some(decoded) => output.extend(decoded),
                            None => {
                                if decode_backspace {
                                    output.extend_from_slice(&self.bytes[before..self.pos]);
                                } else {
                                    output.push(b'\\');
                                    output.extend_from_slice(&self.bytes[before..self.pos]);
                                }
                            }
                        }
                    }
                }
                Some(byte) => output.push(byte),
            }
        }
    }

    /// Parse content between delimiters for s/// replacement strings.
    /// Escaped delimiters are unescaped, while backreferences and case
    /// controls remain in the replacement language for later compilation.
    fn parse_replacement_delimited(&mut self, delim: u8) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        loop {
            match self.advance() {
                None => {
                    return Err(Error::Parse(format!(
                        "unterminated replacement (expected closing {:?})",
                        String::from_utf8_lossy(&[delim])
                    )));
                }
                Some(byte) if byte == delim => return Ok(output),
                Some(b'\\') => {
                    if self.peek() == Some(delim) {
                        output.push(delim);
                        self.advance();
                    } else {
                        output.push(b'\\');
                        if let Some(next) = self.advance() {
                            output.push(next);
                        }
                    }
                }
                Some(byte) => output.push(byte),
            }
        }
    }

    // -- Command-specific parsing --

    fn parse_substitute(&mut self) -> Result<Command> {
        let delim = self
            .advance()
            .ok_or_else(|| Error::Parse("missing delimiter for s command".into()))?;
        let pattern = self.parse_regex_delimited(delim, false)?;
        let replacement = self.parse_replacement_delimited(delim)?;

        let mut global = false;
        let mut print = false;
        let mut nth = None;
        let mut case_insensitive = false;
        let mut write_file = None;
        loop {
            match self.peek() {
                Some(b'g') => {
                    self.advance();
                    global = true;
                }
                Some(b'p') => {
                    self.advance();
                    print = true;
                }
                Some(b'i' | b'I') => {
                    self.advance();
                    case_insensitive = true;
                }
                Some(b'w') => {
                    self.advance();
                    self.skip_spaces();
                    write_file = Some(self.parse_filename_arg());
                    break;
                }
                Some(byte) if byte.is_ascii_digit() => nth = Some(self.parse_number()),
                _ => break,
            }
        }

        Ok(Command::Substitute(SubstituteCmd {
            pattern,
            replacement,
            global,
            print,
            nth,
            case_insensitive,
            write_file,
        }))
    }

    fn parse_transliterate(&mut self) -> Result<Command> {
        let delim = self
            .advance()
            .ok_or_else(|| Error::Parse("missing delimiter for y command".into()))?;
        let from = self.parse_regex_delimited(delim, true)?;
        let to = self.parse_regex_delimited(delim, true)?;
        Ok(Command::Transliterate { from, to })
    }

    /// Parse text argument for a/i/c commands.
    fn parse_text_arg(&mut self) -> Vec<u8> {
        if self.peek() == Some(b'\\') {
            self.advance();
        }
        match self.peek() {
            Some(b'\n') => {
                self.advance();
            }
            Some(b' ' | b'\t') => {
                self.advance();
            }
            _ => {}
        }

        let mut text = Vec::new();
        loop {
            match self.peek() {
                None => break,
                Some(b'\n') => {
                    if text.last() == Some(&b'\\') {
                        text.pop();
                        text.push(b'\n');
                        self.advance();
                    } else {
                        break;
                    }
                }
                Some(byte) => {
                    text.push(byte);
                    self.advance();
                }
            }
        }
        text
    }

    fn parse_label_arg(&mut self) -> Option<Vec<u8>> {
        self.skip_spaces();
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-') {
                self.advance();
            } else {
                break;
            }
        }
        (self.pos != start).then(|| self.bytes[start..self.pos].to_vec())
    }

    fn parse_filename_arg(&mut self) -> Vec<u8> {
        self.skip_spaces();
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if matches!(byte, b'\n' | b';') {
                break;
            }
            self.advance();
        }
        let mut filename = self.bytes[start..self.pos].to_vec();
        while matches!(filename.last(), Some(b' ' | b'\t' | b'\r')) {
            filename.pop();
        }
        filename
    }

    fn parse_rest_of_line(&mut self) -> Vec<u8> {
        self.skip_spaces();
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == b'\n' {
                break;
            }
            self.advance();
        }
        let mut result = self.bytes[start..self.pos].to_vec();
        while matches!(result.last(), Some(b' ' | b'\t' | b'\r')) {
            result.pop();
        }
        result
    }

    fn parse_block(&mut self) -> Result<Vec<SedCommand>> {
        let mut commands = Vec::new();
        loop {
            self.skip_blanks();
            if self.is_at_end() {
                return Err(Error::Parse("unterminated block (missing '}')".into()));
            }
            if self.peek() == Some(b'}') {
                self.advance();
                break;
            }
            if let Some(cmd) = self.parse_one_command()? {
                commands.push(cmd);
            }
        }
        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_substitute() {
        let cmds = parse("s/foo/bar/g").unwrap();
        assert_eq!(cmds.len(), 1);
        match &cmds[0].command {
            Command::Substitute(s) => {
                assert_eq!(s.pattern, b"foo");
                assert_eq!(s.replacement, b"bar");
                assert!(s.global);
            }
            other => panic!("expected Substitute, got {other:?}"),
        }
    }

    #[test]
    fn parse_substitute_custom_delim() {
        let cmds = parse("s|foo|bar|").unwrap();
        match &cmds[0].command {
            Command::Substitute(s) => {
                assert_eq!(s.pattern, b"foo");
                assert_eq!(s.replacement, b"bar");
                assert!(!s.global);
            }
            other => panic!("expected Substitute, got {other:?}"),
        }
    }

    #[test]
    fn parse_address_line() {
        let cmds = parse("3d").unwrap();
        assert!(matches!(
            &cmds[0].address,
            AddressRange::Single {
                addr: Address::Line(3),
                negated: false
            }
        ));
    }

    #[test]
    fn parse_address_relative_ranges() {
        assert!(matches!(
            parse("1,+2p").unwrap()[0].address,
            AddressRange::Range {
                start: Address::Line(1),
                end: Address::Plus(2),
                negated: false
            }
        ));
        assert!(matches!(
            parse("1,~3p").unwrap()[0].address,
            AddressRange::Range {
                start: Address::Line(1),
                end: Address::Tilde(3),
                negated: false
            }
        ));
    }

    #[test]
    fn parse_address_regex() {
        match &parse("/^foo/d").unwrap()[0].address {
            AddressRange::Single {
                addr: Address::Regex(re),
                negated: false,
            } => assert_eq!(re, b"^foo"),
            other => panic!("unexpected address: {other:?}"),
        }
    }

    #[test]
    fn parse_transliterate() {
        match &parse("y/abc/xyz/").unwrap()[0].command {
            Command::Transliterate { from, to } => {
                assert_eq!(from, b"abc");
                assert_eq!(to, b"xyz");
            }
            other => panic!("expected Transliterate, got {other:?}"),
        }
    }

    #[test]
    fn parse_labels_and_branches() {
        let cmds = parse(":loop\ns/foo/bar/\nt loop").unwrap();
        assert_eq!(cmds.len(), 3);
        assert!(matches!(&cmds[0].command, Command::Label(label) if label == b"loop"));
        assert!(matches!(&cmds[2].command, Command::BranchIfSub(Some(label)) if label == b"loop"));
    }

    #[test]
    fn parse_append_text() {
        match &parse("a hello world").unwrap()[0].command {
            Command::Append(text) => assert_eq!(text, b"hello world"),
            other => panic!("expected Append, got {other:?}"),
        }
    }

    #[test]
    fn address_without_command_is_error() {
        assert!(parse("123").is_err());
        assert!(parse("1,5").is_err());
        assert!(parse("/foo/").is_err());
        assert!(parse("$").is_err());
        assert!(parse("1;p").is_err());
        assert!(parse("1\np").is_err());
    }

    #[test]
    fn empty_script_is_ok() {
        assert!(parse("").is_ok());
        assert!(parse("  \n; # comment\n").is_ok());
    }

    #[test]
    fn byte_escapes_are_decoded_without_utf8_conversion() {
        let commands = parse(b"s/[\\o377]/X/;y/\\xFF/X/").unwrap();
        match &commands[0].command {
            Command::Substitute(sub) => assert_eq!(sub.pattern, b"[\xff]"),
            _ => panic!("expected substitution"),
        }
        match &commands[1].command {
            Command::Transliterate { from, to } => {
                assert_eq!(from, b"\xff");
                assert_eq!(to, b"X");
            }
            _ => panic!("expected transliteration"),
        }
    }
}
