use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use jaq_all::jaq_core::{self, Ctx, DataT, Exn, Lut, ValT, Vars};
use jaq_all::jaq_std;
use jaq_all::jaq_std::input::{HasInputs, Inputs, RcIter};
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
      --tab              use tabs for indentation\n\
      --indent n         use n spaces for indentation (-1 through 7)\n\
      --seq              parse and emit JSON-sequence records\n\
      --raw-output0      output raw values followed by NUL\n\
      --args              bind remaining operands in $ARGS.positional\n\
      --jsonargs          parse remaining operands as JSON in $ARGS.positional\n\
      --arg NAME VALUE   bind VALUE as a string\n\
      --argjson NAME JSON bind VALUE as JSON\n\
      --rawfile NAME FILE bind the complete file as a string\n\
      --slurpfile NAME FILE bind the file's JSON values as an array\n\
      --stream           not supported by this applet\n\
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
    indent: Option<i8>,
    seq: bool,
    raw_output0: bool,
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
    let mut argument_mode = None::<bool>;
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
                "--tab" => options.indent = Some(-1),
                "--indent" => {
                    let value = next_arg(&mut args, &text)?;
                    let indent = value
                        .to_string_lossy()
                        .parse::<i8>()
                        .map_err(|_| "--indent takes a number between -1 and 7".to_string())?;
                    if !(-1..=7).contains(&indent) {
                        return Err("--indent takes a number between -1 and 7".to_string());
                    }
                    options.indent = Some(indent);
                }
                "--seq" => options.seq = true,
                "--raw-output0" => {
                    options.raw_output = true;
                    options.raw_output0 = true;
                }
                "--args" => argument_mode = Some(false),
                "--jsonargs" => argument_mode = Some(true),
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
                            let bytes = fs::read(&value)
                                .map_err(|error| format!("{}: {error}", value.to_string_lossy()))?;
                            let mut values = Vec::new();
                            for parsed in json::read::parse_many_jq(strip_bom(&bytes)) {
                                let parsed = parsed.map_err(|error| {
                                    format!("{}: {error}", value.to_string_lossy())
                                })?;
                                values.push(normalize_input(parsed).map_err(|error| {
                                    format!("{}: {error}", value.to_string_lossy())
                                })?);
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
        } else if let Some(json_args) = argument_mode {
            let value = if json_args {
                parse_single_json(arg.as_encoded_bytes())
                    .map_err(|error| format!("invalid JSON argument: {error}"))?
            } else {
                lossy_value(arg.into_encoded_bytes())
            };
            options.positional.push(value);
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
    if !options.bindings.iter().any(|(name, _)| name == "__loc__") {
        let line = options
            .filter
            .as_deref()
            .and_then(|filter| {
                filter.find("$__loc__").map(|offset| {
                    filter[..offset]
                        .bytes()
                        .filter(|byte| *byte == b'\n')
                        .count()
                        + 1
                })
            })
            .unwrap_or(1);
        options.bindings.push((
            "__loc__".to_string(),
            Val::obj(
                [
                    (Val::utf8_str("file"), Val::utf8_str("<top-level>")),
                    (
                        Val::utf8_str("line"),
                        Val::Num(Num::Int(isize::try_from(line).unwrap_or(isize::MAX))),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
        ));
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
        indent: options.indent,
        seq: options.seq,
        raw_output0: options.raw_output0,
    };
    let filename = RefCell::new(Val::Null);
    let inputs = FileInputs::new(
        options.files,
        options.raw_input,
        options.slurp,
        options.seq,
        &filename,
    );
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
                let result = halt_status(&code)
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

fn halt_status(value: &Val) -> Result<i32, json::Error> {
    let number = match value {
        Val::Num(Num::Int(number)) => number.to_string(),
        Val::Num(Num::BigInt(number)) => number.to_string(),
        Val::Num(Num::Dec(number)) => number.as_str().to_string(),
        Val::Num(Num::Float(number)) if number.is_finite() => format!("{:.0}", number.trunc()),
        Val::Num(Num::Float(_)) => return Ok(0),
        _ => return Err(json::Error::typ(value.clone(), "number")),
    };
    if number.starts_with('-') {
        return Ok(0);
    }
    let number = number.strip_prefix('+').unwrap_or(&number);
    let (coefficient, exponent) = number
        .split_once(['e', 'E'])
        .map_or((number, 0_i64), |(coefficient, exponent)| {
            (coefficient, exponent.parse().unwrap_or(0))
        });
    let (integer, fractional) = coefficient
        .split_once('.')
        .map_or((coefficient, ""), |(integer, fractional)| {
            (integer, fractional)
        });
    let decimal_position = i64::try_from(integer.len()).unwrap_or(i64::MAX) + exponent;
    if decimal_position <= 0 {
        return Ok(0);
    }
    let mut digits = integer
        .bytes()
        .chain(fractional.bytes())
        .filter(u8::is_ascii_digit);
    let count = usize::try_from(decimal_position)
        .unwrap_or(usize::MAX)
        .min(integer.len() + fractional.len());
    let mut status = 0_u16;
    for _ in 0..count {
        let digit = digits.next().map_or(0, |digit| digit - b'0');
        status = (status * 10 + u16::from(digit)) % 256;
    }
    Ok(i32::from(status))
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
        )?;
        stderr.write_all(b"\n").map_err(RunError::Io)
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
            if options.seq && !options.raw {
                output.write_all(b"\x1e").map_err(RunError::Io)?;
            }
            if options.raw && !options.ascii {
                if let Val::TStr(text) = &value {
                    let bytes = String::from_utf8_lossy(text);
                    if options.raw_output0 && bytes.as_bytes().contains(&0) {
                        return Err(RunError::Filter(
                            "Cannot dump a string containing NUL with --raw-output0 option"
                                .to_string(),
                        ));
                    }
                    output.write_all(bytes.as_bytes()).map_err(RunError::Io)?;
                } else {
                    write_json(output, options, &value)?;
                }
            } else {
                write_json(output, options, &value)?;
            }
            if !options.join || options.raw_output0 {
                output
                    .write_all(if options.raw_output0 { b"\0" } else { b"\n" })
                    .map_err(RunError::Io)?;
            }
            last = Some(value.as_bool());
        }
    }
    if let Some(error) = io_error.borrow_mut().take() {
        return Err(RunError::Io(error));
    }
    Ok(last)
}

struct InputStream<'a> {
    files: std::vec::IntoIter<PathBuf>,
    current: Option<Box<dyn Read>>,
    filename: &'a RefCell<Val>,
    first: bool,
    bom: Vec<u8>,
    buffer: Vec<u8>,
    offset: usize,
}

impl<'a> InputStream<'a> {
    fn new(mut files: Vec<PathBuf>, filename: &'a RefCell<Val>, strip_bom: bool) -> Self {
        if files.is_empty() {
            files.push(PathBuf::from("-"));
        }
        Self {
            files: files.into_iter(),
            current: None,
            filename,
            first: strip_bom,
            bom: Vec::new(),
            buffer: Vec::new(),
            offset: 0,
        }
    }

    fn open_next(&mut self) -> io::Result<bool> {
        let Some(path) = self.files.next() else {
            return Ok(false);
        };
        if path.as_os_str() == "-" {
            self.current = Some(Box::new(io::stdin()));
            *self.filename.borrow_mut() = Val::utf8_str("<stdin>");
        } else {
            let file = File::open(&path).map_err(|error| {
                io::Error::new(error.kind(), format!("{}: {error}", path.display()))
            })?;
            self.current = Some(Box::new(file));
            *self.filename.borrow_mut() = Val::utf8_str(
                String::from_utf8_lossy(path.as_os_str().as_encoded_bytes()).into_owned(),
            );
        }
        Ok(true)
    }

    fn fill_buffer(&mut self) -> io::Result<bool> {
        if self.offset < self.buffer.len() {
            return Ok(true);
        }
        self.buffer.clear();
        self.offset = 0;
        loop {
            if self.current.is_none() && !self.open_next()? {
                if !self.bom.is_empty() {
                    self.buffer.extend_from_slice(&self.bom);
                    self.bom.clear();
                    self.first = false;
                    return Ok(true);
                }
                return Ok(false);
            }
            let mut chunk = [0_u8; 8192];
            let Some(reader) = self.current.as_mut() else {
                continue;
            };
            let length = reader.read(&mut chunk)?;
            if length == 0 {
                self.current = None;
                continue;
            }
            for byte in &chunk[..length] {
                if self.first {
                    self.bom.push(*byte);
                    if (self.bom.len() == 1 && self.bom[0] != 0xef)
                        || (self.bom.len() == 2 && self.bom[1] != 0xbb)
                        || (self.bom.len() == 3 && self.bom[2] != 0xbf)
                    {
                        self.buffer.extend_from_slice(&self.bom);
                        self.bom.clear();
                        self.first = false;
                    } else if self.bom.len() == 3 {
                        self.bom.clear();
                        self.first = false;
                    }
                } else {
                    self.buffer.push(*byte);
                }
            }
            if !self.buffer.is_empty() {
                return Ok(true);
            }
        }
    }
}

impl Read for InputStream<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || !self.fill_buffer()? {
            return Ok(0);
        }
        let available = &self.buffer[self.offset..];
        let length = available.len().min(output.len());
        output[..length].copy_from_slice(&available[..length]);
        self.offset += length;
        Ok(length)
    }
}

impl BufRead for InputStream<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.fill_buffer()?;
        Ok(&self.buffer[self.offset..])
    }

    fn consume(&mut self, amount: usize) {
        self.offset = self.offset.saturating_add(amount).min(self.buffer.len());
    }
}

struct SharedBufRead<'a> {
    stream: std::rc::Rc<RefCell<InputStream<'a>>>,
    buffer: Vec<u8>,
    offset: usize,
}

impl<'a> SharedBufRead<'a> {
    fn new(stream: std::rc::Rc<RefCell<InputStream<'a>>>) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            offset: 0,
        }
    }
}

impl Read for SharedBufRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let bytes = self.fill_buf()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        let length = bytes.len().min(output.len());
        output[..length].copy_from_slice(&bytes[..length]);
        self.consume(length);
        Ok(length)
    }
}

impl BufRead for SharedBufRead<'_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.offset == self.buffer.len() {
            self.buffer.clear();
            self.offset = 0;
            let mut chunk = [0_u8; 8192];
            let length = self.stream.borrow_mut().read(&mut chunk)?;
            self.buffer.extend_from_slice(&chunk[..length]);
        }
        Ok(&self.buffer[self.offset..])
    }

    fn consume(&mut self, amount: usize) {
        self.offset = self.offset.saturating_add(amount).min(self.buffer.len());
    }
}

struct FileInputs<'a> {
    stream: std::rc::Rc<RefCell<InputStream<'a>>>,
    parser: Option<Box<dyn Iterator<Item = io::Result<Val>> + 'a>>,
    seq: bool,
    raw: bool,
    slurp: bool,
    finished: bool,
}

impl<'a> FileInputs<'a> {
    fn new(
        files: Vec<PathBuf>,
        raw: bool,
        slurp: bool,
        seq: bool,
        filename: &'a RefCell<Val>,
    ) -> Self {
        let stream = std::rc::Rc::new(RefCell::new(InputStream::new(files, filename, !raw)));
        let parser = (!raw).then(|| {
            if seq {
                Box::new(json::read::read_many_jq_seq(SharedBufRead::new(
                    stream.clone(),
                ))) as Box<dyn Iterator<Item = io::Result<Val>> + 'a>
            } else {
                Box::new(json::read::read_many_jq(SharedBufRead::new(stream.clone())))
                    as Box<dyn Iterator<Item = io::Result<Val>> + 'a>
            }
        });
        Self {
            stream,
            parser,
            seq,
            raw,
            slurp,
            finished: false,
        }
    }

    fn read_next(&mut self) -> Result<Option<Val>, RunError> {
        if self.raw {
            let mut bytes = Vec::new();
            let length = self
                .stream
                .borrow_mut()
                .read_until(b'\n', &mut bytes)
                .map_err(RunError::Io)?;
            if length == 0 {
                return Ok(None);
            }
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            return Ok(Some(lossy_value(bytes)));
        }
        let Some(parser) = self.parser.as_mut() else {
            return Ok(None);
        };
        loop {
            match parser.next() {
                None => return Ok(None),
                Some(Ok(value)) => {
                    return normalize_input(value).map(Some).map_err(RunError::Input);
                }
                Some(Err(error)) if self.seq && error.kind() == io::ErrorKind::InvalidData => {
                    continue;
                }
                Some(Err(error)) if error.kind() == io::ErrorKind::InvalidData => {
                    return Err(RunError::Input(error.to_string()));
                }
                Some(Err(error)) => return Err(RunError::Io(error)),
            }
        }
    }

    fn slurp_all(&mut self) -> Result<Val, RunError> {
        if self.raw {
            let mut bytes = Vec::new();
            self.stream
                .borrow_mut()
                .read_to_end(&mut bytes)
                .map_err(RunError::Io)?;
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

fn normalize_input(value: Val) -> Result<Val, String> {
    match value {
        Val::Null | Val::Bool(_) | Val::Num(_) => Ok(value),
        Val::BStr(_) => Err("byte strings are not JSON values".to_string()),
        Val::TStr(text) => Ok(Val::utf8_str(String::from_utf8_lossy(&text).into_owned())),
        Val::Arr(values) => values
            .iter()
            .cloned()
            .map(normalize_input)
            .collect::<Result<Vec<_>, _>>()
            .map(|values| Val::Arr(json::Rc::new(values))),
        Val::Obj(values) => {
            let mut normalized = json::Map::default();
            for (key, value) in values.iter() {
                let key = match key {
                    Val::TStr(text) => Val::utf8_str(String::from_utf8_lossy(text).into_owned()),
                    _ => return Err("object keys must be strings".to_string()),
                };
                normalized.insert(key, normalize_input(value.clone())?);
            }
            Ok(Val::obj(normalized))
        }
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)
}

fn parse_single_json(bytes: &[u8]) -> Result<Val, RunError> {
    let value = json::read::parse_single_jq(strip_bom(bytes))
        .map_err(|error| RunError::Input(error.to_string()))?;
    normalize_input(value).map_err(RunError::Input)
}

#[derive(Default)]
struct OutputOptions {
    compact: bool,
    sort_keys: bool,
    ascii: bool,
    raw: bool,
    join: bool,
    indent: Option<i8>,
    seq: bool,
    raw_output0: bool,
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

fn write_number(output: &mut dyn Write, number: &Num) -> Result<(), RunError> {
    if let Num::Float(number) = number {
        if *number == 0.0 && number.is_sign_negative() {
            output.write_all(b"-0").map_err(RunError::Io)?;
        } else {
            write!(output, "{number}").map_err(RunError::Io)?;
        }
        return Ok(());
    }
    if let Num::Dec(number) = number {
        output
            .write_all(format_decimal(number).as_bytes())
            .map_err(RunError::Io)?;
    } else {
        write!(output, "{number}").map_err(RunError::Io)?;
    }
    Ok(())
}

fn format_decimal(number: &str) -> String {
    let (negative, number) = number
        .strip_prefix('-')
        .map_or((false, number), |number| (true, number));
    let (coefficient, exponent) =
        number
            .split_once(['e', 'E'])
            .map_or((number, 0_i64), |(coefficient, exponent)| {
                let exponent = exponent.parse::<i64>().unwrap_or_else(|_| {
                    if exponent.starts_with('-') {
                        i64::MIN
                    } else {
                        i64::MAX
                    }
                });
                (coefficient, exponent)
            });
    let (integer, fractional) = coefficient
        .split_once('.')
        .map_or((coefficient, ""), |(integer, fractional)| {
            (integer, fractional)
        });
    let mut digits = String::with_capacity(integer.len() + fractional.len());
    digits.push_str(integer);
    digits.push_str(fractional);
    let leading = digits
        .bytes()
        .position(|byte| byte != b'0')
        .unwrap_or(digits.len());
    let sign = if negative { "-" } else { "" };
    if leading == digits.len() {
        let scale = i64::try_from(fractional.len()).unwrap_or(i64::MAX) - exponent;
        return if scale > 0 {
            format!(
                "{sign}0.{}0",
                "0".repeat(usize::try_from(scale - 1).unwrap_or(0))
            )
        } else if scale == 0 {
            format!("{sign}0")
        } else {
            format!("{sign}0E+{}", scale.saturating_neg())
        };
    }
    let digits = &digits[leading..];
    let position = i64::try_from(integer.len()).unwrap_or(i64::MAX)
        - i64::try_from(leading).unwrap_or(i64::MAX)
        + exponent;
    let mut result = String::new();
    result.push_str(sign);
    if position <= 0 {
        result.push_str("0.");
        result.push_str(&"0".repeat(usize::try_from(position.saturating_neg()).unwrap_or(0)));
        result.push_str(digits);
    } else if usize::try_from(position).unwrap_or(usize::MAX) <= digits.len() {
        let position = usize::try_from(position).unwrap_or(digits.len());
        result.push_str(&digits[..position]);
        if position < digits.len() {
            result.push('.');
            result.push_str(&digits[position..]);
        }
    } else {
        result.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            result.push('.');
            result.push_str(&digits[1..]);
        }
        result.push('E');
        let exponent = position - 1;
        if exponent >= 0 {
            result.push('+');
        }
        result.push_str(&exponent.to_string());
    }
    result
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
                Val::Num(number) => write_number(output, number)?,
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
        write_indent(output, depth, options.indent)?;
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
        write_indent(output, depth, options.indent)?;
    }
    output.write_all(end)
}

fn write_indent(output: &mut dyn Write, depth: usize, indent: Option<i8>) -> io::Result<()> {
    match indent {
        Some(-1) => {
            for _ in 0..depth {
                output.write_all(b"\t")?;
            }
        }
        Some(spaces) => {
            for _ in 0..depth {
                for _ in 0..spaces {
                    output.write_all(b" ")?;
                }
            }
        }
        None => {
            for _ in 0..depth {
                output.write_all(b"  ")?;
            }
        }
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
