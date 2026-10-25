use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use jaq_all::jaq_core::{self, Ctx, DataT, Exn, Lut, ValT, Vars};
use jaq_all::jaq_std::input::{HasInputs, Inputs, RcIter};
use jaq_all::jaq_std::{self, ValT as _};
use jaq_all::json::{self, Num, Val};

const HELP: &str = "Usage: jq [OPTIONS] FILTER [FILE ...]\n\
\n\
Options:\n\
  -r, --raw-output       output strings without JSON quotes\n\
  -R, --raw-input        read each input line as a string\n\
  -j, --join-output      output strings without JSON quotes or newlines\n\
  -c, --compact-output   output compact JSON\n\
  -a, --ascii-output     escape non-ASCII characters in JSON output\n\
  -s, --slurp            read all inputs into an array\n\
  -S, --sort-keys        sort object keys in output\n\
  -e, --exit-status      set status from the last output value\n\
  -n, --null-input       run FILTER once with null input\n\
  -f, --from-file FILE   read FILTER from FILE\n\
      --args            bind remaining operands in $ARGS.positional\n\
      --arg NAME VALUE   bind VALUE as a string\n\
      --argjson NAME JSON bind VALUE as JSON\n\
      --rawfile NAME FILE bind the complete file as a string\n\
      --slurpfile NAME FILE bind the file's JSON values as an array\n\
      --stream          not supported by this applet\n\
  -h, --help             show this help\n\
  -V, --version          show version";

#[derive(Default)]
struct Options {
    raw_output: bool,
    raw_input: bool,
    join_output: bool,
    compact_output: bool,
    ascii_output: bool,
    slurp: bool,
    sort_keys: bool,
    exit_status: bool,
    null_input: bool,
    bindings: Vec<(String, Val)>,
    filter: Option<String>,
    positional: Vec<Val>,
    files: Vec<PathBuf>,
}

enum ParseOutcome {
    Run(Options),
    Exit(i32),
}

#[derive(Debug)]
enum RunError {
    Input(String),
    Compile(String),
    Filter(String),
    Halt(i32),
    Io(io::Error),
}

pub fn jq(args: Vec<OsString>) -> i32 {
    let options = match parse_args(args) {
        Ok(ParseOutcome::Run(options)) => options,
        Ok(ParseOutcome::Exit(code)) => return code,
        Err(error) => {
            eprintln!("jq: {error}");
            return 2;
        }
    };

    match run(options) {
        Ok(code) => code,
        Err(RunError::Halt(code)) => code,
        Err(RunError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(RunError::Io(error)) => {
            eprintln!("jq: {error}");
            2
        }
        Err(RunError::Input(error)) => {
            eprintln!("jq: parse error: {error}");
            5
        }
        Err(RunError::Compile(error)) => {
            eprintln!("jq: error: {error}");
            3
        }
        Err(RunError::Filter(error)) => {
            eprintln!("jq: error: {error}");
            5
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(message) | Self::Compile(message) | Self::Filter(message) => {
                formatter.write_str(message)
            }
            Self::Halt(code) => write!(formatter, "halted with status {code}"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

fn parse_args(args: Vec<OsString>) -> Result<ParseOutcome, String> {
    let mut options = Options::default();
    let mut args = args.into_iter().skip(1);
    let mut positional_only = false;
    let mut string_args = false;
    let mut from_file = false;
    let mut program = None;

    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        if !positional_only && text == "--" {
            positional_only = true;
            continue;
        }
        if !positional_only && text.starts_with("--") {
            match text.as_ref() {
                "--raw-output" => options.raw_output = true,
                "--raw-input" => options.raw_input = true,
                "--join-output" => {
                    options.raw_output = true;
                    options.join_output = true;
                }
                "--compact-output" => options.compact_output = true,
                "--ascii-output" => options.ascii_output = true,
                "--slurp" => options.slurp = true,
                "--sort-keys" => options.sort_keys = true,
                "--exit-status" => options.exit_status = true,
                "--null-input" => options.null_input = true,
                "--from-file" => from_file = true,
                "--args" => string_args = true,
                "--arg" | "--argjson" | "--rawfile" | "--slurpfile" => {
                    let name = next_arg(&mut args, &text)?.to_string_lossy().into_owned();
                    let value = next_arg(&mut args, &text)?;
                    if options
                        .bindings
                        .iter()
                        .any(|(existing, _)| existing == &name)
                    {
                        continue;
                    }
                    let value = match text.as_ref() {
                        "--arg" => lossy_value(value.into_encoded_bytes()),
                        "--argjson" => {
                            parse_single_json(value.as_encoded_bytes()).map_err(|error| {
                                format!("invalid JSON value for --argjson {name}: {error}")
                            })?
                        }
                        "--rawfile" => lossy_value(
                            fs::read(&value)
                                .map_err(|error| format!("{}: {error}", value.to_string_lossy()))?,
                        ),
                        "--slurpfile" => {
                            let file = File::open(&value)
                                .map_err(|error| format!("{}: {error}", value.to_string_lossy()))?;
                            let mut reader = JsonInput::new(BufReader::new(file));
                            let mut values = Vec::new();
                            while let Some(value) = reader
                                .next_value()
                                .map_err(|error| format!("{}: {error}", value.to_string_lossy()))?
                            {
                                values.push(value);
                            }
                            Val::Arr(json::Rc::new(values))
                        }
                        _ => unreachable!(),
                    };
                    options.bindings.push((name, value));
                }
                "--stream" | "--stream-errors" => {
                    return Err(format!("{text} is not supported by this applet"));
                }
                "--help" => {
                    println!("{HELP}");
                    return Ok(ParseOutcome::Exit(0));
                }
                "--version" => {
                    println!("jq (axe {}, jaq)", env!("CARGO_PKG_VERSION"));
                    return Ok(ParseOutcome::Exit(0));
                }
                _ => return Err(format!("unknown option: {text}")),
            }
            continue;
        }
        if !positional_only && text.starts_with('-') && text != "-" {
            for flag in text[1..].chars() {
                match flag {
                    'r' => options.raw_output = true,
                    'R' => options.raw_input = true,
                    'j' => {
                        options.raw_output = true;
                        options.join_output = true;
                    }
                    'c' => options.compact_output = true,
                    'a' => options.ascii_output = true,
                    's' => options.slurp = true,
                    'S' => options.sort_keys = true,
                    'e' => options.exit_status = true,
                    'n' => options.null_input = true,
                    'f' => from_file = true,
                    'h' => {
                        println!("{HELP}");
                        return Ok(ParseOutcome::Exit(0));
                    }
                    'V' => {
                        println!("jq (axe {}, jaq)", env!("CARGO_PKG_VERSION"));
                        return Ok(ParseOutcome::Exit(0));
                    }
                    _ => return Err(format!("unknown option: -{flag}")),
                }
            }
            continue;
        }

        if program.is_none() {
            program = Some(arg);
        } else if string_args {
            options
                .positional
                .push(lossy_value(arg.into_encoded_bytes()));
        } else {
            options.files.push(PathBuf::from(arg));
        }
    }

    options.filter = Some(if from_file {
        let path = program.ok_or_else(|| "--from-file requires a program filename".to_string())?;
        let source =
            fs::read(&path).map_err(|error| format!("{}: {error}", path.to_string_lossy()))?;
        if source.contains(&0) {
            return Err("program file contains NUL bytes".to_string());
        }
        String::from_utf8_lossy(&source).into_owned()
    } else {
        program.map_or_else(
            || ".".to_string(),
            |program| program.to_string_lossy().into_owned(),
        )
    });
    let named = Val::obj(
        options
            .bindings
            .iter()
            .map(|(name, value)| (Val::utf8_str(name.clone()), value.clone()))
            .collect(),
    );
    let args = Val::obj(
        [
            (
                Val::utf8_str("positional"),
                Val::Arr(json::Rc::new(std::mem::take(&mut options.positional))),
            ),
            (Val::utf8_str("named"), named),
        ]
        .into_iter()
        .collect(),
    );
    if let Some((_, value)) = options.bindings.iter_mut().find(|(name, _)| name == "ARGS") {
        *value = args;
    } else {
        options.bindings.push(("ARGS".to_string(), args));
    }
    Ok(ParseOutcome::Run(options))
}

fn next_arg(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<OsString, String> {
    args.next()
        .ok_or_else(|| format!("{option} expects another argument"))
}

fn lossy_value(bytes: Vec<u8>) -> Val {
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
    Val::utf8_str(text)
}

fn run(options: Options) -> Result<i32, RunError> {
    let variable_names = options
        .bindings
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let filter = jaq_all::compile_with(
        options.filter.as_deref().expect("filter checked by parser"),
        jaq_all::defs().filter(|def| !matches!(def.name, "halt_error" | "tostring")),
        jq_funs(),
        &variable_names,
    )
    .map_err(|reports| {
        let mut message = String::new();
        for report in reports {
            message.push_str(&jaq_all::load::FileReportsDisp::new(&report).to_string());
        }
        RunError::Compile(message.trim_end().to_string())
    })?;
    let output_options = OutputOptions {
        compact: options.compact_output,
        sort_keys: options.sort_keys,
        ascii: options.ascii_output,
        raw: options.raw_output,
        join: options.join_output,
    };
    let filename = RefCell::new(Val::Null);
    let inputs = FileInputs::new(options.files, options.raw_input, options.slurp, &filename);
    let stdout = io::stdout();
    let mut stdout = io::BufWriter::new(stdout.lock());
    let result = execute(
        &filter,
        Vars::new(options.bindings.into_iter().map(|(_, value)| value)),
        inputs,
        &filename,
        options.null_input,
        &mut stdout,
        &output_options,
    );
    stdout.flush().map_err(RunError::Io)?;
    let last = result?;
    if options.exit_status {
        Ok(match last {
            Some(true) => 0,
            Some(false) => 1,
            None => 4,
        })
    } else {
        Ok(0)
    }
}

struct JqKind;

// jaq-all's runner takes one immutable filename, but jq tracks input/inputs consumption.
struct JqData<'a> {
    lut: &'a Lut<JqKind>,
    inputs: Inputs<'a, Val>,
    input_filename: &'a RefCell<Val>,
}

impl DataT for JqKind {
    type V<'a> = Val;
    type Data<'a> = &'a JqData<'a>;
}

impl<'a> jaq_core::data::HasLut<'a, JqKind> for &'a JqData<'a> {
    fn lut(&self) -> &'a Lut<JqKind> {
        self.lut
    }
}

impl<'a> HasInputs<'a, Val> for &'a JqData<'a> {
    fn inputs(&self) -> Inputs<'a, Val> {
        self.inputs
    }
}

fn jq_funs() -> impl Iterator<Item = jaq_core::native::Fun<JqKind>> {
    use jaq_core::native::{bome, v};
    let native = jaq_core::native::run::<JqKind>;
    jaq_core::funs::<JqKind>()
        .chain(jaq_std::funs::<JqKind>().filter(|(name, _, _)| *name != "stderr_empty"))
        .chain(
            json::funs::<JqKind>().filter(|(name, _, _)| !matches!(*name, "fromjson" | "tojson")),
        )
        .chain(
            jaq_std::input::funs::<JqKind>()
                .into_vec()
                .into_iter()
                .filter(|(name, _, _)| *name != "input")
                .map(native),
        )
        .chain([
            native(("input_filename", v(0), |cv| {
                bome(Ok(cv.0.data().input_filename.borrow().clone()))
            })),
            native(("input", v(0), |cv| {
                let mut inputs = cv.0.data().inputs;
                bome(
                    inputs
                        .next()
                        .unwrap_or_else(|| Err("break".to_string()))
                        .map_err(json::Error::str),
                )
            })),
            native(("fromjson", v(0), |cv| {
                bome(
                    cv.1.try_as_utf8_bytes_owned()
                        .and_then(|bytes| parse_single_json(&bytes).map_err(json::Error::str)),
                )
            })),
            native(("tojson", v(0), |cv| bome(json_text(&cv.1)))),
            native(("tostring", v(0), |cv| {
                bome(if matches!(&cv.1, Val::TStr(_)) {
                    Ok(cv.1)
                } else {
                    json_text(&cv.1)
                })
            })),
            native(("stderr_empty", v(0), |cv| match write_stderr(&cv.1) {
                Ok(()) => Box::new(std::iter::empty()),
                Err(error) => bome(Err(json::Error::str(error))),
            })),
            native(("halt_error", v(0), |cv| {
                Box::new(std::iter::once(halt_with_payload(&cv.1, 5)))
            })),
            native(("halt_error", v(1), |mut cv| {
                let code = cv.0.pop_var();
                let result = code
                    .as_isize()
                    .ok_or_else(|| json::Error::typ(code, "integer"))
                    .and_then(|code| i32::try_from(code).map_err(json::Error::str))
                    .map_err(Exn::from)
                    .and_then(|code| halt_with_payload(&cv.1, code));
                Box::new(std::iter::once(result))
            })),
        ])
}

fn halt_with_payload<'a>(value: &Val, code: i32) -> json::ValX<'a> {
    if !matches!(value, Val::Null) {
        write_stderr(value).map_err(|error| Exn::from(json::Error::str(error)))?;
    }
    Err(Exn::halt(code))
}

fn write_stderr(value: &Val) -> Result<(), RunError> {
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    if let Val::TStr(text) = value {
        stderr
            .write_all(String::from_utf8_lossy(text).as_bytes())
            .map_err(RunError::Io)
    } else {
        write_json(
            &mut stderr,
            &OutputOptions {
                compact: true,
                ..OutputOptions::default()
            },
            value,
        )
    }
}

fn execute(
    filter: &jaq_core::Filter<JqKind>,
    vars: Vars<Val>,
    inputs: impl Iterator<Item = Result<Val, RunError>>,
    filename: &RefCell<Val>,
    null_input: bool,
    output: &mut dyn Write,
    options: &OutputOptions,
) -> Result<Option<bool>, RunError> {
    let io_error = RefCell::new(None);
    let inputs = RcIter::new(inputs.map(|result| {
        result.map_err(|error| {
            let message = error.to_string();
            if let RunError::Io(error) = error {
                *io_error.borrow_mut() = Some(error);
            }
            message
        })
    }));
    let data = JqData {
        lut: &filter.lut,
        inputs: &inputs,
        input_filename: filename,
    };
    let ctx = Ctx::<JqKind>::new(&data, vars);
    let mut null = Some(Val::Null);
    let mut inputs = data.inputs;
    let mut last = None;
    loop {
        let input = if null_input {
            null.take().map(Ok)
        } else {
            inputs.next()
        };
        let Some(input) = input else { break };
        let input = input.map_err(|message| {
            io_error
                .borrow_mut()
                .take()
                .map_or_else(|| RunError::Input(message), RunError::Io)
        })?;
        for value in filter.id.run((ctx.clone(), input)) {
            let value = value.map_err(|error| {
                io_error
                    .borrow_mut()
                    .take()
                    .map_or_else(|| map_exception(error), RunError::Io)
            })?;
            if options.raw && !options.ascii {
                if let Val::TStr(text) = &value {
                    output
                        .write_all(String::from_utf8_lossy(text).as_bytes())
                        .map_err(RunError::Io)?;
                } else {
                    write_json(output, options, &value)?;
                }
            } else {
                write_json(output, options, &value)?;
            }
            if !options.join {
                output.write_all(b"\n").map_err(RunError::Io)?;
            }
            last = Some(value.as_bool());
        }
    }
    if let Some(error) = io_error.borrow_mut().take() {
        return Err(RunError::Io(error));
    }
    Ok(last)
}

struct FileInputs<'a> {
    files: std::vec::IntoIter<PathBuf>,
    current: Option<JsonInput<Box<dyn BufRead>>>,
    filename: &'a RefCell<Val>,
    raw: bool,
    slurp: bool,
    finished: bool,
}

impl<'a> FileInputs<'a> {
    fn new(mut files: Vec<PathBuf>, raw: bool, slurp: bool, filename: &'a RefCell<Val>) -> Self {
        if files.is_empty() {
            files.push(PathBuf::from("-"));
        }
        Self {
            files: files.into_iter(),
            current: None,
            filename,
            raw,
            slurp,
            finished: false,
        }
    }

    fn open_next(&mut self) -> Result<bool, RunError> {
        let Some(path) = self.files.next() else {
            return Ok(false);
        };
        let (reader, filename): (Box<dyn BufRead>, Val) = if path.as_os_str() == "-" {
            (
                Box::new(BufReader::new(io::stdin())),
                Val::utf8_str("<stdin>"),
            )
        } else {
            let file = File::open(&path).map_err(|error| {
                RunError::Io(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", path.display()),
                ))
            })?;
            (
                Box::new(BufReader::new(file)),
                Val::utf8_str(
                    String::from_utf8_lossy(path.as_os_str().as_encoded_bytes()).into_owned(),
                ),
            )
        };
        *self.filename.borrow_mut() = filename;
        self.current = Some(JsonInput::new(reader));
        Ok(true)
    }

    fn read_next(&mut self) -> Result<Option<Val>, RunError> {
        loop {
            if self.current.is_none() && !self.open_next()? {
                return Ok(None);
            }
            let reader = self.current.as_mut().expect("opened input");
            let value = if self.raw {
                let mut bytes = Vec::new();
                let length = reader
                    .reader
                    .read_until(b'\n', &mut bytes)
                    .map_err(RunError::Io)?;
                if length == 0 {
                    None
                } else {
                    if bytes.last() == Some(&b'\n') {
                        bytes.pop();
                    }
                    Some(lossy_value(bytes))
                }
            } else {
                reader.next_value()?
            };
            if value.is_some() {
                return Ok(value);
            }
            self.current = None;
        }
    }

    fn slurp_all(&mut self) -> Result<Val, RunError> {
        if self.raw {
            let mut bytes = Vec::new();
            while self.open_next()? {
                self.current
                    .as_mut()
                    .expect("opened input")
                    .reader
                    .read_to_end(&mut bytes)
                    .map_err(RunError::Io)?;
                self.current = None;
            }
            Ok(lossy_value(bytes))
        } else {
            let mut values = Vec::new();
            while let Some(value) = self.read_next()? {
                values.push(value);
            }
            Ok(Val::Arr(json::Rc::new(values)))
        }
    }
}

impl Iterator for FileInputs<'_> {
    type Item = Result<Val, RunError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        if self.slurp {
            self.finished = true;
            return Some(self.slurp_all());
        }
        match self.read_next() {
            Ok(Some(value)) => Some(Ok(value)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

enum JsonFrame {
    Array(Vec<Val>),
    Object(json::Map),
    Key(Val),
}

struct JsonInput<R> {
    reader: R,
    line: usize,
    column: usize,
}

impl<R: BufRead> JsonInput<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            line: 1,
            column: 0,
        }
    }

    fn peek(&mut self) -> Result<Option<u8>, RunError> {
        self.reader
            .fill_buf()
            .map(|bytes| bytes.first().copied())
            .map_err(RunError::Io)
    }

    fn take(&mut self) -> Result<Option<u8>, RunError> {
        let next = self.peek()?;
        if let Some(byte) = next {
            self.reader.consume(1);
            if byte == b'\n' {
                self.line += 1;
                self.column = 0;
            } else {
                self.column += 1;
            }
        }
        Ok(next)
    }

    fn error(&self, message: &str) -> RunError {
        RunError::Input(format!(
            "{message} at line {}, column {}",
            self.line, self.column
        ))
    }

    fn whitespace(&mut self) -> Result<(), RunError> {
        while matches!(self.peek()?, Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.take()?;
        }
        Ok(())
    }

    fn next_value(&mut self) -> Result<Option<Val>, RunError> {
        let mut stack = Vec::new();
        let mut value = None;
        loop {
            if stack.is_empty() && value.is_some() {
                return Ok(value);
            }
            self.whitespace()?;
            let Some(next) = self.peek()? else {
                return if stack.is_empty() {
                    Ok(None)
                } else {
                    Err(self.error("Unfinished JSON term at EOF"))
                };
            };
            match next {
                b'[' | b'{' => {
                    self.take()?;
                    if value.is_some() {
                        return Err(self.error("Expected separator between values"));
                    }
                    if stack.len() >= 10_000 {
                        return Err(self.error("Exceeds depth limit for parsing"));
                    }
                    stack.push(if next == b'[' {
                        JsonFrame::Array(Vec::new())
                    } else {
                        JsonFrame::Object(json::Map::default())
                    });
                }
                b':' => {
                    self.take()?;
                    let key = value
                        .take()
                        .ok_or_else(|| self.error("Expected string key before ':'"))?;
                    if !matches!(stack.last(), Some(JsonFrame::Object(_))) {
                        return Err(self.error("':' not as part of an object"));
                    }
                    if !matches!(key, Val::TStr(_)) {
                        return Err(self.error("Object keys must be strings"));
                    }
                    stack.push(JsonFrame::Key(key));
                }
                b',' => {
                    self.take()?;
                    let item = value
                        .take()
                        .ok_or_else(|| self.error("Expected value before ','"))?;
                    match stack.last_mut() {
                        Some(JsonFrame::Array(values)) => values.push(item),
                        Some(JsonFrame::Key(_)) => {
                            let Some(JsonFrame::Key(key)) = stack.pop() else {
                                unreachable!()
                            };
                            let Some(JsonFrame::Object(values)) = stack.last_mut() else {
                                unreachable!()
                            };
                            values.insert(key, item);
                        }
                        Some(JsonFrame::Object(_)) => {
                            return Err(self.error("Objects must consist of key:value pairs"));
                        }
                        None => return Err(self.error("',' not as part of an object or array")),
                    }
                }
                b']' => {
                    self.take()?;
                    let Some(JsonFrame::Array(mut values)) = stack.pop() else {
                        return Err(self.error("Unmatched ']'"));
                    };
                    if let Some(item) = value.take() {
                        values.push(item);
                    } else if !values.is_empty() {
                        return Err(self.error("Expected another array element"));
                    }
                    value = Some(Val::Arr(json::Rc::new(values)));
                }
                b'}' => {
                    self.take()?;
                    if let Some(item) = value.take() {
                        let Some(JsonFrame::Key(key)) = stack.pop() else {
                            return Err(self.error("Objects must consist of key:value pairs"));
                        };
                        let Some(JsonFrame::Object(mut values)) = stack.pop() else {
                            unreachable!()
                        };
                        values.insert(key, item);
                        value = Some(Val::obj(values));
                    } else {
                        let Some(JsonFrame::Object(values)) = stack.pop() else {
                            return Err(self.error("Unmatched '}'"));
                        };
                        if !values.is_empty() {
                            return Err(self.error("Expected another key-value pair"));
                        }
                        value = Some(Val::obj(values));
                    }
                }
                _ => {
                    if value.is_some() {
                        return Err(self.error("Expected separator between values"));
                    }
                    value = Some(if next == b'"' {
                        self.string()?
                    } else {
                        self.literal()?
                    });
                }
            }
        }
    }

    fn string(&mut self) -> Result<Val, RunError> {
        self.take()?;
        let mut bytes = vec![b'"'];
        loop {
            let byte = self
                .take()?
                .ok_or_else(|| self.error("Unfinished string at EOF"))?;
            bytes.push(byte);
            if byte == b'"' {
                break;
            }
            if byte == b'\\' {
                bytes.push(
                    self.take()?
                        .ok_or_else(|| self.error("Unfinished string at EOF"))?,
                );
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        serde_json::from_str::<String>(&text)
            .map(Val::utf8_str)
            .map_err(|error| self.error(&format!("Invalid string: {error}")))
    }

    fn literal(&mut self) -> Result<Val, RunError> {
        let mut bytes = Vec::new();
        while let Some(byte) = self.peek()? {
            if matches!(
                byte,
                b' ' | b'\t' | b'\r' | b'\n' | b'[' | b']' | b'{' | b'}' | b',' | b':' | b'"'
            ) {
                break;
            }
            bytes.push(byte);
            self.take()?;
        }
        let number = match bytes.as_slice() {
            b"null" => return Ok(Val::Null),
            b"true" => return Ok(Val::Bool(true)),
            b"false" => return Ok(Val::Bool(false)),
            b"NaN" | b"-NaN" | b"+NaN" => Some(Num::Float(f64::NAN)),
            b"Infinity" | b"+Infinity" => Some(Num::Float(f64::INFINITY)),
            b"-Infinity" => Some(Num::Float(f64::NEG_INFINITY)),
            number => {
                // jq accepts a leading plus on input, but JSON output numbers must not have one.
                let number = number
                    .strip_prefix(b"+")
                    .filter(|unsigned| !unsigned.starts_with(b"-"))
                    .unwrap_or(number);
                if valid_number(number) {
                    json::read::parse_single_num(number)
                } else {
                    None
                }
            }
        };
        match number {
            Some(number) => Ok(Val::Num(number)),
            None => {
                self.take()?;
                Err(self.error("Invalid numeric literal"))
            }
        }
    }
}

fn valid_number(bytes: &[u8]) -> bool {
    let mut index = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        _ => return false,
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return false;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return false;
        }
    }
    index == bytes.len()
}

fn parse_single_json(bytes: &[u8]) -> Result<Val, RunError> {
    let mut reader = JsonInput::new(io::Cursor::new(bytes));
    let value = reader
        .next_value()?
        .ok_or_else(|| reader.error("Expected JSON value"))?;
    if reader.next_value()?.is_some() {
        return Err(reader.error("Unexpected extra JSON value"));
    }
    Ok(value)
}

#[derive(Default)]
struct OutputOptions {
    compact: bool,
    sort_keys: bool,
    ascii: bool,
    raw: bool,
    join: bool,
}

fn json_text(value: &Val) -> Result<Val, json::Error> {
    let mut bytes = Vec::new();
    write_json(
        &mut bytes,
        &OutputOptions {
            compact: true,
            ..OutputOptions::default()
        },
        value,
    )
    .map_err(json::Error::str)?;
    Ok(Val::utf8_str(bytes))
}

fn validate_json(value: &Val) -> Result<(), RunError> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Val::BStr(_) => {
                return Err(RunError::Filter(
                    "byte strings are not JSON values".to_string(),
                ));
            }
            Val::Arr(values) => pending.extend(values.iter()),
            Val::Obj(values) => {
                for (key, value) in values.iter() {
                    if !matches!(key, Val::TStr(_)) {
                        return Err(RunError::Filter("object keys must be strings".to_string()));
                    }
                    pending.push(value);
                }
            }
            Val::Num(Num::Dec(number)) if !valid_number(number.as_bytes()) => {
                return Err(RunError::Filter("invalid JSON number".to_string()));
            }
            _ => {}
        }
    }
    Ok(())
}

enum WriteFrame<'a> {
    Value(&'a Val, usize),
    Array(&'a [Val], usize, usize),
    Object(&'a json::Map, Option<Vec<usize>>, usize, usize),
}

fn write_json(
    output: &mut dyn Write,
    options: &OutputOptions,
    value: &Val,
) -> Result<(), RunError> {
    validate_json(value)?;
    let mut pending = vec![WriteFrame::Value(value, 0)];
    while let Some(frame) = pending.pop() {
        match frame {
            WriteFrame::Value(value, depth) => match value {
                Val::Null => output.write_all(b"null").map_err(RunError::Io)?,
                Val::Bool(value) => output
                    .write_all(if *value { b"true" } else { b"false" })
                    .map_err(RunError::Io)?,
                Val::Num(Num::Float(number)) if number.is_nan() => {
                    output.write_all(b"null").map_err(RunError::Io)?;
                }
                Val::Num(Num::Float(number)) if number.is_infinite() => {
                    output
                        .write_all(if number.is_sign_negative() {
                            b"-1.7976931348623157e+308"
                        } else {
                            b"1.7976931348623157e+308"
                        })
                        .map_err(RunError::Io)?;
                }
                Val::Num(number) => write!(output, "{number}").map_err(RunError::Io)?,
                Val::TStr(text) => {
                    write_string(output, text, options.ascii).map_err(RunError::Io)?
                }
                Val::BStr(_) => unreachable!("validated JSON value"),
                Val::Arr(values) => {
                    output.write_all(b"[").map_err(RunError::Io)?;
                    pending.push(WriteFrame::Array(values, 0, depth));
                }
                Val::Obj(values) => {
                    output.write_all(b"{").map_err(RunError::Io)?;
                    let order = options.sort_keys.then(|| {
                        let mut order = (0..values.len()).collect::<Vec<_>>();
                        order.sort_by(|left, right| {
                            values
                                .get_index(*left)
                                .unwrap()
                                .0
                                .cmp(values.get_index(*right).unwrap().0)
                        });
                        order
                    });
                    pending.push(WriteFrame::Object(values, order, 0, depth));
                }
            },
            WriteFrame::Array(values, index, depth) => {
                if let Some(value) = values.get(index) {
                    write_separator(output, options, index, depth + 1).map_err(RunError::Io)?;
                    pending.push(WriteFrame::Array(values, index + 1, depth));
                    pending.push(WriteFrame::Value(value, depth + 1));
                } else {
                    write_end(output, options, values.len(), depth, b"]").map_err(RunError::Io)?;
                }
            }
            WriteFrame::Object(values, order, index, depth) => {
                if index < values.len() {
                    let key_index = order.as_ref().map_or(index, |order| order[index]);
                    let (key, value) = values.get_index(key_index).expect("object output index");
                    write_separator(output, options, index, depth + 1).map_err(RunError::Io)?;
                    let Val::TStr(key) = key else {
                        unreachable!("validated JSON key")
                    };
                    write_string(output, key, options.ascii).map_err(RunError::Io)?;
                    output
                        .write_all(if options.compact { b":" } else { b": " })
                        .map_err(RunError::Io)?;
                    pending.push(WriteFrame::Object(values, order, index + 1, depth));
                    pending.push(WriteFrame::Value(value, depth + 1));
                } else {
                    write_end(output, options, values.len(), depth, b"}").map_err(RunError::Io)?;
                }
            }
        }
    }
    Ok(())
}

fn write_separator(
    output: &mut dyn Write,
    options: &OutputOptions,
    index: usize,
    depth: usize,
) -> io::Result<()> {
    if index != 0 {
        output.write_all(b",")?;
    }
    if !options.compact {
        output.write_all(b"\n")?;
        write_indent(output, depth)?;
    }
    Ok(())
}

fn write_end(
    output: &mut dyn Write,
    options: &OutputOptions,
    length: usize,
    depth: usize,
    end: &[u8],
) -> io::Result<()> {
    if !options.compact && length != 0 {
        output.write_all(b"\n")?;
        write_indent(output, depth)?;
    }
    output.write_all(end)
}

fn write_indent(output: &mut dyn Write, depth: usize) -> io::Result<()> {
    for _ in 0..depth {
        output.write_all(b"  ")?;
    }
    Ok(())
}

fn write_string(output: &mut dyn Write, bytes: &[u8], ascii: bool) -> io::Result<()> {
    output.write_all(b"\"")?;
    let text = String::from_utf8_lossy(bytes);
    let mut start = 0;
    for (index, character) in text.char_indices() {
        let escape = match character {
            '"' => Some("\\\""),
            '\\' => Some("\\\\"),
            '\u{08}' => Some("\\b"),
            '\u{0c}' => Some("\\f"),
            '\n' => Some("\\n"),
            '\r' => Some("\\r"),
            '\t' => Some("\\t"),
            _ => None,
        };
        if escape.is_some() || character <= '\u{1f}' || (ascii && !character.is_ascii()) {
            output.write_all(&text.as_bytes()[start..index])?;
            if let Some(escape) = escape {
                output.write_all(escape.as_bytes())?;
            } else {
                for unit in character.encode_utf16(&mut [0; 2]) {
                    write!(output, "\\u{unit:04x}")?;
                }
            }
            start = index + character.len_utf8();
        }
    }
    output.write_all(&text.as_bytes()[start..])?;
    output.write_all(b"\"")
}

fn map_exception(exception: Exn<'_, Val>) -> RunError {
    match exception.get_err() {
        Ok(error) => RunError::Filter(error.to_string()),
        Err(exception) => match exception.get_halt() {
            Ok(code) => RunError::Halt(code),
            Err(_) => RunError::Filter("unexpected filter control flow".to_string()),
        },
    }
}
