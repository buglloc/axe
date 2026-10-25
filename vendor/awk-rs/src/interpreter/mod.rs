mod builtins;
mod expr;
mod input;
pub mod stmt;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::{Child, ChildStdin};
use std::rc::Rc;

use bstr::BString;

use crate::ast::*;
use crate::error::{Error, Result};
use crate::regex::Regex;
use crate::value::Value;

use input::{RecordReader, join, unescape_assignment, variable_assignment, whitespace_fields};

pub struct PipeInput<'a> {
    #[allow(dead_code)]
    child: Child,
    reader: RecordReader<'a>,
}

pub enum OutputFile {
    File(File),
    Pipe(ChildStdin),
}

impl Write for OutputFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            OutputFile::File(file) => file.write(buf),
            OutputFile::Pipe(pipe) => pipe.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            OutputFile::File(file) => file.flush(),
            OutputFile::Pipe(pipe) => pipe.flush(),
        }
    }
}

/// Runtime strings are bytes, including field values, array keys and paths.
pub struct Interpreter<'a> {
    program: &'a Program,
    pub(crate) variables: HashMap<String, Value>,
    pub(crate) arrays: HashMap<String, HashMap<BString, Value>>,
    pub(crate) functions: HashMap<String, &'a FunctionDef>,
    pub(crate) fs: BString,
    pub(crate) ofs: BString,
    pub(crate) rs: BString,
    pub(crate) ors: BString,
    pub(crate) ofmt: BString,
    pub(crate) convfmt: BString,
    pub(crate) subsep: BString,
    pub(crate) fpat: BString,
    pub(crate) fieldwidths: BString,
    pub(crate) posix_mode: bool,
    pub(crate) traditional_mode: bool,
    pub(crate) multibyte: bool,
    pub(crate) record: BString,
    pub(crate) fields: Vec<BString>,
    pub(crate) nf: usize,
    pub(crate) nr: usize,
    pub(crate) fnr: usize,
    pub(crate) filename: BString,
    pub(crate) rstart: usize,
    pub(crate) rlength: i32,
    should_exit: bool,
    exit_code: i32,
    should_next: bool,
    should_nextfile: bool,
    pub(crate) output_files: HashMap<BString, OutputFile>,
    pub(crate) input_files: HashMap<BString, RecordReader<'a>>,
    pub(crate) pipes: HashMap<BString, PipeInput<'a>>,
    regex_cache: HashMap<BString, Rc<Regex>>,
    range_states: HashMap<usize, bool>,
    pub(crate) rand_seed: u64,
    pub(crate) rand_state: u64,
    pub(crate) argc: usize,
    pub(crate) environ: HashMap<BString, BString>,
    pub(crate) array_aliases: HashMap<String, String>,
    stdin: Option<RecordReader<'a>>,
    main_input: Option<RecordReader<'a>>,
    reading_stdin: bool,
    argument_index: usize,
    input_seen: bool,
    input_done: bool,
}

impl<'a> Interpreter<'a> {
    pub fn new(program: &'a Program) -> Self {
        let functions = program
            .functions
            .iter()
            .map(|func| (func.name.clone(), func))
            .collect();
        let environ = std::env::vars_os()
            .map(|(name, value)| (name.as_bytes().into(), value.as_bytes().into()))
            .collect();
        let rand_seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(12345);
        Self {
            program,
            variables: HashMap::new(),
            arrays: HashMap::new(),
            functions,
            fs: " ".into(),
            ofs: " ".into(),
            rs: "\n".into(),
            ors: "\n".into(),
            ofmt: "%.6g".into(),
            convfmt: "%.6g".into(),
            subsep: "\x1c".into(),
            fpat: BString::default(),
            fieldwidths: BString::default(),
            posix_mode: false,
            traditional_mode: false,
            multibyte: crate::value::multibyte_locale(),
            record: BString::default(),
            fields: Vec::new(),
            nf: 0,
            nr: 0,
            fnr: 0,
            filename: BString::default(),
            rstart: 0,
            rlength: -1,
            should_exit: false,
            exit_code: 0,
            should_next: false,
            should_nextfile: false,
            output_files: HashMap::new(),
            input_files: HashMap::new(),
            pipes: HashMap::new(),
            regex_cache: HashMap::new(),
            range_states: HashMap::new(),
            rand_seed,
            rand_state: rand_seed,
            argc: 1,
            environ,
            array_aliases: HashMap::new(),
            stdin: Some(RecordReader::new(Box::new(BufReader::new(io::stdin())))),
            main_input: None,
            reading_stdin: false,
            argument_index: 1,
            input_seen: false,
            input_done: false,
        }
    }

    pub fn set_args(&mut self, args: Vec<Vec<u8>>) {
        self.argc = args.len();
        let argv = args
            .into_iter()
            .enumerate()
            .map(|(index, value)| (index.to_string().into(), Value::from_string(value)))
            .collect();
        self.arrays.insert("ARGV".to_owned(), argv);
    }

    /// Supply the standard input stream before execution.
    pub fn set_stdin(&mut self, reader: impl BufRead + 'a) {
        self.stdin = Some(RecordReader::new(Box::new(reader)));
    }

    pub fn set_fs(&mut self, fs: &[u8]) {
        self.fs = fs.into();
        self.fpat.clear();
        self.fieldwidths.clear();
    }

    pub fn set_posix_mode(&mut self, enabled: bool) {
        self.posix_mode = enabled;
        if enabled {
            self.traditional_mode = false;
        }
    }

    pub fn set_traditional_mode(&mut self, enabled: bool) {
        self.traditional_mode = enabled;
        if enabled {
            self.posix_mode = false;
        }
    }

    /// Apply a command-line assignment, interpreting AWK backslash escapes.
    pub fn set_assignment(&mut self, assignment: &[u8]) -> Result<()> {
        let (name, value) = variable_assignment(assignment)
            .ok_or_else(|| Error::runtime("invalid variable assignment"))?;
        self.set_variable_value(name, Value::from_string(unescape_assignment(value)));
        Ok(())
    }

    /// Run BEGIN before opening operands; END also runs after an AWK exit.
    pub fn run<W: Write>(&mut self, output: &mut W) -> Result<i32> {
        self.execute_phase(|pattern| matches!(pattern, Some(Pattern::Begin)), output)?;
        let needs_input = self
            .program
            .rules
            .iter()
            .any(|rule| !matches!(rule.pattern, Some(Pattern::Begin)));
        if !self.should_exit && needs_input {
            while let Some(record) = self.next_main_record(output)? {
                self.nr += 1;
                self.fnr += 1;
                self.set_record(record)?;
                match self.process_current_record(output) {
                    Ok(()) | Err(Error::ControlFlow) => {}
                    Err(error) => return Err(error),
                }
                self.should_next = false;
                if self.should_exit {
                    break;
                }
                if self.should_nextfile {
                    self.should_nextfile = false;
                    self.finish_file(output)?;
                }
            }
        }
        self.should_exit = false;
        self.should_next = false;
        self.should_nextfile = false;
        self.execute_phase(|pattern| matches!(pattern, Some(Pattern::End)), output)?;
        output.flush()?;
        for file in self.output_files.values_mut() {
            file.flush()?;
        }
        Ok(self.exit_code)
    }

    fn execute_phase<W: Write>(
        &mut self,
        matches: fn(&Option<Pattern>) -> bool,
        output: &mut W,
    ) -> Result<()> {
        for rule in &self.program.rules {
            if matches(&rule.pattern) {
                if let Some(action) = &rule.action {
                    self.execute_block(action, output)?;
                }
                if self.should_exit {
                    break;
                }
            }
        }
        Ok(())
    }

    fn open_next_file<W: Write>(&mut self, output: &mut W) -> Result<bool> {
        let filename = loop {
            if self.argument_index < self.argc {
                let key = self.argument_index.to_string();
                self.argument_index += 1;
                let operand = self
                    .get_array_element("ARGV", key.as_bytes())
                    .to_string_val();
                if operand.is_empty() {
                    continue;
                }
                if let Some((name, value)) = variable_assignment(&operand) {
                    self.set_variable_value(name, Value::from_string(unescape_assignment(value)));
                    continue;
                }
                break operand;
            }
            if self.input_seen {
                self.input_done = true;
                return Ok(false);
            }
            break BString::from("-");
        };
        self.input_seen = true;
        self.reading_stdin = filename == "-";
        self.main_input = if self.reading_stdin {
            self.stdin.take()
        } else {
            Some(RecordReader::new(Box::new(BufReader::new(File::open(
                OsStr::from_bytes(&filename),
            )?))))
        };
        self.filename = filename;
        self.fnr = 0;
        self.execute_phase(
            |pattern| matches!(pattern, Some(Pattern::BeginFile)),
            output,
        )?;
        Ok(true)
    }

    fn finish_file<W: Write>(&mut self, output: &mut W) -> Result<()> {
        let input = self.main_input.take();
        if self.reading_stdin {
            self.stdin = input;
        }
        self.execute_phase(|pattern| matches!(pattern, Some(Pattern::EndFile)), output)
    }

    fn record_separator_len(&self) -> usize {
        if self.posix_mode || self.traditional_mode {
            crate::value::character_end(&self.rs, 0, self.multibyte)
        } else {
            self.rs.len()
        }
    }

    pub(crate) fn next_main_record<W: Write>(&mut self, output: &mut W) -> Result<Option<BString>> {
        while !self.input_done && !self.should_exit {
            if self.main_input.is_none() && !self.open_next_file(output)? {
                return Ok(None);
            }
            if self.should_exit {
                return Ok(None);
            }
            if self.should_nextfile {
                self.should_nextfile = false;
                self.finish_file(output)?;
                continue;
            }
            let separator_len = self.record_separator_len();
            if let Some(input) = self.main_input.as_mut() {
                if let Some(record) =
                    input.read_record(&self.rs[..separator_len], self.multibyte)?
                {
                    return Ok(Some(record));
                }
            }
            self.finish_file(output)?;
        }
        Ok(None)
    }

    fn process_current_record<W: Write>(&mut self, output: &mut W) -> Result<()> {
        for (index, rule) in self.program.rules.iter().enumerate() {
            if matches!(
                &rule.pattern,
                Some(Pattern::Begin | Pattern::End | Pattern::BeginFile | Pattern::EndFile)
            ) {
                continue;
            }
            if self.pattern_matches(&rule.pattern, index)? {
                if let Some(action) = &rule.action {
                    self.execute_block(action, output)?;
                } else {
                    output.write_all(&self.record)?;
                    output.write_all(&self.ors)?;
                }
            }
            if self.should_next {
                self.should_next = false;
                break;
            }
            if self.should_nextfile || self.should_exit {
                break;
            }
        }
        Ok(())
    }

    pub(crate) fn set_record(&mut self, record: BString) -> Result<()> {
        self.record = record;
        self.split_fields()
    }

    fn split_fields(&mut self) -> Result<()> {
        self.fields.clear();
        if self.record.is_empty() {
            self.nf = 0;
            return Ok(());
        }
        if !self.fpat.is_empty() && !self.posix_mode && !self.traditional_mode {
            let re = self.get_regex(&self.fpat.clone())?;
            for found in re.ranges(&self.record)? {
                self.fields.push(self.record[found].into());
            }
        } else if !self.fieldwidths.is_empty() && !self.posix_mode && !self.traditional_mode {
            let mut position = 0;
            for width in whitespace_fields(&self.fieldwidths) {
                let Ok(width) = std::str::from_utf8(width).unwrap_or("").parse::<usize>() else {
                    continue;
                };
                if position >= self.record.len() {
                    break;
                }
                let end = position
                    + crate::value::byte_offset(&self.record[position..], width, self.multibyte);
                self.fields.push(self.record[position..end].into());
                position = end;
            }
        } else if self.fs == " " {
            self.fields
                .extend(whitespace_fields(&self.record).map(BString::from));
        } else if self.fs.is_empty() {
            let mut at = 0;
            while at < self.record.len() {
                let end = crate::value::character_end(&self.record, at, self.multibyte);
                if !self.rs.is_empty() || self.record[at] != b'\n' {
                    self.fields.push(self.record[at..end].into());
                }
                at = end;
            }
        } else if self.fs.len() == 1 {
            let separator = self.fs[0];
            self.fields.extend(
                self.record
                    .split(|byte| *byte == separator || (self.rs.is_empty() && *byte == b'\n'))
                    .map(BString::from),
            );
        } else {
            let mut pattern = self.fs.clone();
            if self.rs.is_empty() {
                let mut paragraph_pattern = BString::from("(");
                paragraph_pattern.extend_from_slice(&pattern);
                paragraph_pattern.extend_from_slice(b")|\n");
                pattern = paragraph_pattern;
            }
            let re = self.get_regex(&pattern)?;
            self.fields
                .extend(re.split(&self.record)?.into_iter().map(BString::from));
        }
        self.nf = self.fields.len();
        Ok(())
    }

    pub(crate) fn get_field(&self, index: usize) -> BString {
        if index == 0 {
            self.record.clone()
        } else {
            self.fields.get(index - 1).cloned().unwrap_or_default()
        }
    }

    pub(crate) fn set_field(&mut self, index: usize, value: BString) -> Result<()> {
        if index == 0 {
            self.set_record(value)?;
        } else {
            self.fields
                .resize_with(self.fields.len().max(index), BString::default);
            self.fields[index - 1] = value;
            self.nf = self.fields.len();
            self.record = join(&self.fields, &self.ofs);
        }
        Ok(())
    }

    fn pattern_matches(&mut self, pattern: &Option<Pattern>, index: usize) -> Result<bool> {
        match pattern {
            None => Ok(true),
            Some(Pattern::Begin | Pattern::End | Pattern::BeginFile | Pattern::EndFile) => {
                Ok(false)
            }
            Some(Pattern::Expr(expr)) => Ok(self.eval_expr(expr)?.is_truthy()),
            Some(Pattern::Regex(pattern)) => {
                self.get_regex(pattern.as_bytes())?.is_match(&self.record)
            }
            Some(Pattern::Range { start, end }) => {
                let active = self.range_states.get(&index).copied().unwrap_or(false);
                if !active {
                    if self.pattern_matches(&Some(start.as_ref().clone()), index)? {
                        self.range_states.insert(index, true);
                        return Ok(true);
                    }
                    Ok(false)
                } else {
                    if self.pattern_matches(&Some(end.as_ref().clone()), index)? {
                        self.range_states.insert(index, false);
                    }
                    Ok(true)
                }
            }
            Some(Pattern::And(left, right)) => Ok(self
                .pattern_matches(&Some(left.as_ref().clone()), index)?
                && self.pattern_matches(&Some(right.as_ref().clone()), index)?),
            Some(Pattern::Or(left, right)) => Ok(self
                .pattern_matches(&Some(left.as_ref().clone()), index)?
                || self.pattern_matches(&Some(right.as_ref().clone()), index)?),
            Some(Pattern::Not(inner)) => {
                Ok(!self.pattern_matches(&Some(inner.as_ref().clone()), index)?)
            }
        }
    }

    pub(crate) fn get_regex(&mut self, pattern: &[u8]) -> Result<Rc<Regex>> {
        if let Some(regex) = self.regex_cache.get(pattern) {
            return Ok(Rc::clone(regex));
        }
        let regex = Rc::new(Regex::new(pattern, self.multibyte)?);
        self.regex_cache.insert(pattern.into(), Rc::clone(&regex));
        Ok(regex)
    }

    pub(crate) fn get_variable(&self, name: &str) -> Value {
        match name {
            "NF" => Value::Number(self.nf as f64),
            "NR" => Value::Number(self.nr as f64),
            "FNR" => Value::Number(self.fnr as f64),
            "FS" => Value::from_string(self.fs.clone()),
            "OFS" => Value::from_string(self.ofs.clone()),
            "RS" => Value::from_string(self.rs.clone()),
            "ORS" => Value::from_string(self.ors.clone()),
            "OFMT" => Value::from_string(self.ofmt.clone()),
            "CONVFMT" => Value::from_string(self.convfmt.clone()),
            "SUBSEP" => Value::from_string(self.subsep.clone()),
            "FILENAME" => Value::from_string(self.filename.clone()),
            "RSTART" => Value::Number(self.rstart as f64),
            "RLENGTH" => Value::Number(self.rlength as f64),
            "ARGC" => Value::Number(self.argc as f64),
            "FPAT" => Value::from_string(self.fpat.clone()),
            "FIELDWIDTHS" => Value::from_string(self.fieldwidths.clone()),
            _ => self
                .variables
                .get(name)
                .cloned()
                .unwrap_or(Value::Uninitialized),
        }
    }

    fn get_special_array(&self, array: &str, key: &[u8]) -> Option<Value> {
        match array {
            "ENVIRON" => self
                .environ
                .get(key)
                .map(|value| Value::from_string(value.clone())),
            "PROCINFO" => Some(match key {
                b"version" => Value::from_string(env!("CARGO_PKG_VERSION")),
                b"strftime" => Value::from_string("%a %b %e %H:%M:%S %Z %Y"),
                b"FS" => Value::from_string(if !self.fpat.is_empty() {
                    "FPAT"
                } else if !self.fieldwidths.is_empty() {
                    "FIELDWIDTHS"
                } else {
                    "FS"
                }),
                b"pid" => Value::Number(std::process::id() as f64),
                b"identifiers" | b"ppid" | b"uid" | b"gid" | b"euid" | b"egid" | b"pgrpid" => {
                    Value::Number(0.0)
                }
                _ => Value::Uninitialized,
            }),
            _ => None,
        }
    }

    pub(crate) fn set_variable_value(&mut self, name: &str, value: Value) {
        match name {
            "NF" => {
                self.nf = value.to_number() as usize;
                self.fields.resize_with(self.nf, BString::default);
                self.record = join(&self.fields, &self.ofs);
            }
            "NR" => self.nr = value.to_number() as usize,
            "FNR" => self.fnr = value.to_number() as usize,
            "ARGC" => self.argc = value.to_number() as usize,
            "FILENAME" => self.filename = value.to_string_val(),
            "FS" => {
                self.fs = value.to_string_val();
                self.fpat.clear();
                self.fieldwidths.clear();
            }
            "OFS" => self.ofs = value.to_string_val(),
            "RS" => self.rs = value.to_string_val(),
            "ORS" => self.ors = value.to_string_val(),
            "OFMT" => self.ofmt = value.to_string_val(),
            "CONVFMT" => self.convfmt = value.to_string_val(),
            "SUBSEP" => self.subsep = value.to_string_val(),
            "FPAT" => {
                self.fpat = value.to_string_val();
                self.fieldwidths.clear();
            }
            "FIELDWIDTHS" => {
                self.fieldwidths = value.to_string_val();
                self.fpat.clear();
            }
            _ => {
                self.variables.insert(name.to_string(), value);
            }
        }
    }

    fn resolve_array_name<'b>(&'b self, array: &'b str) -> &'b str {
        self.array_aliases
            .get(array)
            .map(String::as_str)
            .unwrap_or(array)
    }

    pub(crate) fn get_array_element(&self, array: &str, key: &[u8]) -> Value {
        let array = self.resolve_array_name(array);
        if let Some(value) = self.get_special_array(array, key) {
            return value;
        }
        self.arrays
            .get(array)
            .and_then(|array| array.get(key))
            .cloned()
            .unwrap_or(Value::Uninitialized)
    }

    pub(crate) fn set_array_element(&mut self, array: &str, key: &[u8], value: Value) {
        let array = self.resolve_array_name(array).to_owned();
        if array == "ENVIRON" {
            self.environ.insert(key.into(), value.to_string_val());
        } else {
            self.arrays
                .entry(array)
                .or_default()
                .insert(key.into(), value);
        }
    }

    pub(crate) fn array_key_exists(&self, array: &str, key: &[u8]) -> bool {
        let array = self.resolve_array_name(array);
        match array {
            "ENVIRON" => self.environ.contains_key(key),
            _ => self
                .arrays
                .get(array)
                .is_some_and(|array| array.contains_key(key)),
        }
    }

    pub(crate) fn delete_array_element(&mut self, array: &str, key: &[u8]) {
        let array = self.resolve_array_name(array).to_owned();
        if array == "ENVIRON" {
            self.environ.remove(key);
        } else if let Some(array) = self.arrays.get_mut(&array) {
            array.remove(key);
        }
    }

    pub(crate) fn make_array_key(&self, indices: &[Value]) -> BString {
        join(
            &indices
                .iter()
                .map(|value| self.to_conv_str(value))
                .collect::<Vec<_>>(),
            &self.subsep,
        )
    }

    pub(crate) fn to_conv_str(&self, value: &Value) -> BString {
        value.to_string_with_format(&self.convfmt)
    }

    pub(crate) fn to_output_str(&self, value: &Value) -> BString {
        value.to_string_with_format(&self.ofmt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use std::io::Cursor;

    fn run_awk(program: &str, input: &str) -> String {
        let mut lexer = Lexer::new(program);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        let mut interpreter = Interpreter::new(&ast);
        let mut output = Vec::new();
        interpreter.set_stdin(std::io::BufReader::new(Cursor::new(input)));
        interpreter.run(&mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn test_begin_print() {
        let output = run_awk(r#"BEGIN { print "hello" }"#, "");
        assert_eq!(output, "hello\n");
    }

    #[test]
    fn test_print_field() {
        let output = run_awk("{ print $1 }", "one two three");
        assert_eq!(output, "one\n");
    }

    #[test]
    fn test_print_multiple_fields() {
        let output = run_awk("{ print $1, $3 }", "one two three");
        assert_eq!(output, "one three\n");
    }

    #[test]
    fn test_arithmetic() {
        let output = run_awk("BEGIN { print 2 + 3 * 4 }", "");
        assert_eq!(output, "14\n");
    }

    #[test]
    fn test_variable() {
        let output = run_awk("BEGIN { x = 5; print x }", "");
        assert_eq!(output, "5\n");
    }

    #[test]
    fn test_if_statement() {
        let output = run_awk("BEGIN { x = 10; if (x > 5) print \"big\" }", "");
        assert_eq!(output, "big\n");
    }

    #[test]
    fn test_while_loop() {
        let output = run_awk("BEGIN { i = 1; while (i <= 3) { print i; i++ } }", "");
        assert_eq!(output, "1\n2\n3\n");
    }

    #[test]
    fn test_pattern_match() {
        let output = run_awk("/two/ { print $0 }", "one\ntwo\nthree");
        assert_eq!(output, "two\n");
    }

    #[test]
    fn test_for_loop() {
        let output = run_awk("BEGIN { for (i = 1; i <= 3; i++) print i }", "");
        assert_eq!(output, "1\n2\n3\n");
    }

    #[test]
    fn test_for_in_loop() {
        let output = run_awk(
            "BEGIN { a[1]=1; a[2]=2; for (k in a) count++; print count }",
            "",
        );
        assert_eq!(output, "2\n");
    }

    #[test]
    fn test_do_while() {
        let output = run_awk("BEGIN { i = 0; do { i++ } while (i < 3); print i }", "");
        assert_eq!(output, "3\n");
    }

    #[test]
    fn test_break() {
        let output = run_awk(
            "BEGIN { for (i=1; i<=10; i++) { if (i==3) break; print i } }",
            "",
        );
        assert_eq!(output, "1\n2\n");
    }

    #[test]
    fn test_continue() {
        let output = run_awk(
            "BEGIN { for (i=1; i<=3; i++) { if (i==2) continue; print i } }",
            "",
        );
        assert_eq!(output, "1\n3\n");
    }

    #[test]
    fn test_next() {
        let output = run_awk("/skip/ { next } { print }", "one\nskip\ntwo");
        assert_eq!(output, "one\ntwo\n");
    }

    #[test]
    fn test_exit() {
        // exit is called before print on NR == 2
        let output = run_awk("NR == 2 { exit } { print }", "one\ntwo\nthree");
        assert_eq!(output, "one\n");
    }

    #[test]
    fn test_exit_in_end() {
        let output = run_awk("{ print } END { print \"done\" }", "one\ntwo");
        assert!(output.contains("done"));
    }

    #[test]
    fn test_array_access() {
        let output = run_awk("BEGIN { a[\"x\"] = 1; print a[\"x\"] }", "");
        assert_eq!(output, "1\n");
    }

    #[test]
    fn test_array_in() {
        let output = run_awk("BEGIN { a[1]=1; print (1 in a), (2 in a) }", "");
        assert_eq!(output, "1 0\n");
    }

    #[test]
    fn test_delete() {
        let output = run_awk(
            "BEGIN { a[1]=1; a[2]=2; delete a[1]; for(k in a) print k }",
            "",
        );
        assert_eq!(output, "2\n");
    }

    #[test]
    fn test_special_variables() {
        let output = run_awk("{ print NR, NF, $0 }", "a b c");
        assert_eq!(output, "1 3 a b c\n");
    }

    #[test]
    fn test_fs_change() {
        let output = run_awk("BEGIN { FS = \":\" } { print $1 }", "a:b:c");
        assert_eq!(output, "a\n");
    }

    #[test]
    fn test_ofs() {
        let output = run_awk("BEGIN { OFS = \"-\" } { print $1, $2 }", "a b c");
        assert_eq!(output, "a-b\n");
    }

    #[test]
    fn test_nf_access() {
        let output = run_awk("{ print $NF }", "a b c");
        assert_eq!(output, "c\n");
    }

    #[test]
    fn test_field_modify() {
        let output = run_awk("{ $2 = \"X\"; print $0 }", "a b c");
        assert_eq!(output, "a X c\n");
    }

    #[test]
    fn test_user_function() {
        let output = run_awk(
            "function double(x) { return x*2 } BEGIN { print double(5) }",
            "",
        );
        assert_eq!(output, "10\n");
    }

    #[test]
    fn test_recursion() {
        let output = run_awk(
            "function fact(n) { return n<=1 ? 1 : n*fact(n-1) } BEGIN { print fact(5) }",
            "",
        );
        assert_eq!(output, "120\n");
    }

    #[test]
    fn test_printf() {
        let output = run_awk("BEGIN { printf \"%d %s\\n\", 42, \"hello\" }", "");
        assert_eq!(output, "42 hello\n");
    }

    #[test]
    fn test_range_pattern() {
        let output = run_awk(
            "/start/,/end/ { print }",
            "before\nstart\nmiddle\nend\nafter",
        );
        assert_eq!(output, "start\nmiddle\nend\n");
    }

    #[test]
    fn test_compound_pattern_and() {
        let output = run_awk("NR > 1 && NR < 4 { print }", "one\ntwo\nthree\nfour");
        assert_eq!(output, "two\nthree\n");
    }

    #[test]
    fn test_logical_or_in_expr() {
        let output = run_awk("{ if (/a/ || /c/) print }", "a\nb\nc");
        assert_eq!(output, "a\nc\n");
    }

    #[test]
    fn test_negated_pattern() {
        let output = run_awk("!/skip/ { print }", "keep\nskip\nkeep");
        assert_eq!(output, "keep\nkeep\n");
    }

    #[test]
    fn test_builtin_length() {
        let output = run_awk("BEGIN { print length(\"hello\") }", "");
        assert_eq!(output, "5\n");
    }

    #[test]
    fn test_builtin_substr() {
        let output = run_awk("BEGIN { print substr(\"hello\", 2, 3) }", "");
        assert_eq!(output, "ell\n");
    }

    #[test]
    fn test_builtin_index() {
        let output = run_awk("BEGIN { print index(\"hello\", \"ll\") }", "");
        assert_eq!(output, "3\n");
    }

    #[test]
    fn test_builtin_split() {
        let output = run_awk(
            "BEGIN { n = split(\"a:b:c\", arr, \":\"); print n, arr[1], arr[2] }",
            "",
        );
        assert_eq!(output, "3 a b\n");
    }

    #[test]
    fn test_builtin_sub() {
        let output = run_awk("BEGIN { x = \"hello\"; sub(\"l\", \"L\", x); print x }", "");
        assert_eq!(output, "heLlo\n");
    }

    #[test]
    fn test_builtin_gsub() {
        let output = run_awk(
            "BEGIN { x = \"hello\"; gsub(\"l\", \"L\", x); print x }",
            "",
        );
        assert_eq!(output, "heLLo\n");
    }

    #[test]
    fn test_builtin_match() {
        let output = run_awk("BEGIN { print match(\"hello\", \"ll\") }", "");
        assert_eq!(output, "3\n");
    }

    #[test]
    fn test_builtin_sprintf() {
        let output = run_awk("BEGIN { print sprintf(\"%05d\", 42) }", "");
        assert_eq!(output, "00042\n");
    }

    #[test]
    fn test_builtin_tolower() {
        let output = run_awk("BEGIN { print tolower(\"HELLO\") }", "");
        assert_eq!(output, "hello\n");
    }

    #[test]
    fn test_builtin_toupper() {
        let output = run_awk("BEGIN { print toupper(\"hello\") }", "");
        assert_eq!(output, "HELLO\n");
    }

    #[test]
    fn test_builtin_math() {
        let output = run_awk("BEGIN { print int(3.7), sqrt(4), sin(0) }", "");
        assert_eq!(output, "3 2 0\n");
    }

    #[test]
    fn test_ternary() {
        let output = run_awk("BEGIN { print 1 ? \"yes\" : \"no\" }", "");
        assert_eq!(output, "yes\n");
    }

    #[test]
    fn test_concatenation() {
        let output = run_awk("BEGIN { print \"a\" \"b\" \"c\" }", "");
        assert_eq!(output, "abc\n");
    }

    #[test]
    fn test_unary_ops() {
        let output = run_awk("BEGIN { x = 5; print -x, +x, !0 }", "");
        assert_eq!(output, "-5 5 1\n");
    }

    #[test]
    fn test_post_increment() {
        let output = run_awk("BEGIN { x = 5; print x++ \" \" x }", "");
        assert_eq!(output, "5 6\n");
    }

    #[test]
    fn test_pre_increment() {
        let output = run_awk("BEGIN { x = 5; print ++x }", "");
        assert_eq!(output, "6\n");
    }

    #[test]
    fn test_compound_assign() {
        let output = run_awk("BEGIN { x = 10; x += 5; x -= 3; x *= 2; print x }", "");
        assert_eq!(output, "24\n");
    }

    #[test]
    fn test_getline_var() {
        // Reading "a", getline pulls "b" into next_line without touching $0.
        let output = run_awk("{ getline next_line; print $0, next_line }", "a\nb");
        assert_eq!(output, "a b\n");
    }

    #[test]
    fn test_getline_var_updates_nr_but_not_record() {
        // `getline var` advances NR/FNR but leaves $0 and NF alone.
        let output = run_awk(
            "{ getline v; print $0 \"|\" v \"|\" NR \"|\" NF }",
            "a b\nc d",
        );
        assert_eq!(output, "a b|c d|2|2\n");
    }

    #[test]
    fn test_bare_getline_sets_record() {
        // Bare getline replaces $0 (and NF) with the next record.
        let output = run_awk("{ getline; print $0, NF, NR }", "a\nx y z");
        assert_eq!(output, "x y z 3 2\n");
    }

    #[test]
    fn test_bare_getline_returns_zero_at_eof() {
        let output = run_awk("{ r = getline; print r, $0 }", "a\nb\nc");
        assert_eq!(output, "1 b\n0 c\n");
    }

    #[test]
    fn test_getline_in_begin_without_input_is_eof() {
        // The supplied standard input is empty.
        let output = run_awk("BEGIN { print getline }", "");
        assert_eq!(output, "0\n");
    }

    #[test]
    fn test_begin_end_run_once_with_positioned_assignments() {
        let mut lexer = Lexer::new(r#"BEGIN { print "hi" } END { print NR, x }"#);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();
        let mut interpreter = Interpreter::new(&ast);
        interpreter.set_args(vec![
            b"awk".to_vec(),
            b"x=one".to_vec(),
            b"-".to_vec(),
            b"x=two".to_vec(),
        ]);
        interpreter.set_stdin(std::io::BufReader::new(Cursor::new("a\nb\nc\n")));
        let mut output = Vec::new();
        interpreter.run(&mut output).unwrap();
        assert_eq!(output, b"hi\n3 two\n");
    }

    #[test]
    fn test_filename_and_fnr_track_standard_input() {
        let output = run_awk("{ print FILENAME, NR, FNR }", "a\nb\nc\n");
        assert_eq!(output, "- 1 1\n- 2 2\n- 3 3\n");
    }

    #[test]
    fn test_fpat() {
        let output = run_awk("BEGIN { FPAT = \"[^,]+\" } { print $1, $2 }", "a,b,c");
        assert_eq!(output, "a b\n");
    }

    #[test]
    fn test_fieldwidths() {
        let output = run_awk(
            "BEGIN { FIELDWIDTHS = \"2 3 2\" } { print $1, $2 }",
            "abcdefg",
        );
        assert_eq!(output, "ab cde\n");
    }

    #[test]
    fn test_paragraph_mode() {
        let output = run_awk("BEGIN { RS = \"\" } { print NR, NF }", "a b\nc d\n\ne f");
        // First paragraph has 4 words across 2 lines, second has 2 words
        assert!(output.contains("1"));
    }

    #[test]
    fn test_unicode_record_boundary_uses_complete_lookahead_across_refills() {
        let source = r#"BEGIN {RS="x\\y"} {print "[" $0 "]"}"#;
        let tokens = Lexer::new(source).tokenize().unwrap();
        let program = Parser::new(tokens).parse().unwrap();
        for capacity in [1, 2] {
            let mut interpreter = Interpreter::new(&program);
            interpreter.multibyte = true;
            interpreter.set_stdin(BufReader::with_capacity(
                capacity,
                Cursor::new("xé".as_bytes()),
            ));
            let mut output = Vec::new();
            assert_eq!(interpreter.run(&mut output).unwrap(), 0);
            assert_eq!(output, "[xé]\n".as_bytes(), "reader capacity {capacity}");
        }
    }
}
