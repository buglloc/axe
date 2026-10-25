use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::process::{Child, ExitStatus, Stdio};

const DEFAULT_EXECUTION_BUDGET: usize = 128 * 1024;
const MAX_SINGLE_ARGUMENT: usize = 128 * 1024 - 1;

pub fn find(mut args: Vec<OsString>) -> i32 {
    if let Err(error) = rewrite_find_commands(&mut args) {
        eprintln!("find: {error}");
        return 1;
    }

    let deps = findutils::find::StandardDependencies::new();
    findutils::find::find_main(&args, &deps)
}

pub fn xargs(args: Vec<OsString>) -> i32 {
    match Xargs::parse(args).and_then(|config| config.run()) {
        Ok(code) => code,
        Err(error) => {
            if let Some(message) = error.message {
                eprintln!("xargs: {message}");
            }
            error.code
        }
    }
}

struct XargsFailure {
    code: i32,
    message: Option<String>,
}

impl XargsFailure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: Some(message.into()),
        }
    }

    fn done() -> Self {
        Self {
            code: 0,
            message: None,
        }
    }
}

struct Xargs {
    command: OsString,
    initial: Vec<OsString>,
    delimiter: Option<u8>,
    eof: Option<OsString>,
    max_args: Option<usize>,
    max_lines: Option<usize>,
    max_chars: Option<usize>,
    max_procs: usize,
    replace: Option<OsString>,
    no_run_if_empty: bool,
}

impl Xargs {
    fn parse(args: Vec<OsString>) -> Result<Self, XargsFailure> {
        let mut iter = args.into_iter();
        let _name = iter.next();
        let mut delimiter = None;
        let mut eof = None;
        let mut max_args = None;
        let mut max_lines = None;
        let mut max_chars = None;
        let mut max_procs = 1;
        let mut replace = None;
        let mut no_run_if_empty = false;
        let mut command = None;
        let mut initial = Vec::new();

        while let Some(arg) = iter.next() {
            if command.is_some() {
                initial.push(arg);
                continue;
            }
            let bytes = arg.as_encoded_bytes();
            if bytes == b"--" {
                command = iter.next();
                break;
            }
            if !bytes.starts_with(b"-") || bytes == b"-" {
                command = Some(arg);
                break;
            }
            let (option, attached) = if bytes.starts_with(b"--") {
                match bytes.iter().position(|byte| *byte == b'=') {
                    Some(index) => (&bytes[..index], Some(&bytes[index + 1..])),
                    None => (bytes, None),
                }
            } else {
                (&bytes[..2], (bytes.len() > 2).then_some(&bytes[2..]))
            };
            if option == b"-e" {
                eof = attached
                    .map(os_string_from_input)
                    .transpose()
                    .map_err(XargsFailure::usage)?
                    .and_then(nonempty_os_string);
                continue;
            }
            if option == b"-E" {
                let value = match attached {
                    Some(value) => os_string_from_input(value).map_err(XargsFailure::usage)?,
                    None => iter
                        .next()
                        .ok_or_else(|| XargsFailure::usage("option requires an argument: -E"))?,
                };
                eof = nonempty_os_string(value);
                continue;
            }
            if option == b"--eof" {
                eof = attached
                    .map(os_string_from_input)
                    .transpose()
                    .map_err(XargsFailure::usage)?
                    .and_then(nonempty_os_string);
                continue;
            }

            if matches!(
                option,
                b"-0" | b"--null" | b"-r" | b"--no-run-if-empty" | b"--help" | b"--version"
            ) {
                if attached.is_some() {
                    return Err(XargsFailure::usage(format!(
                        "option does not take an argument: {}",
                        String::from_utf8_lossy(option)
                    )));
                }
                match option {
                    b"-0" | b"--null" => delimiter = Some(0),
                    b"-r" | b"--no-run-if-empty" => no_run_if_empty = true,
                    b"--help" => {
                        println!(
                            "Usage: xargs [-0] [-r] [-E EOFSTR] [-e[EOFSTR]] \
                             [--eof[=EOFSTR]] [-d DELIM] [-n MAX-ARGS] [-L MAX-LINES] \
                             [-s MAX-CHARS] [-P MAX-PROCS] [-I REPLACE] [COMMAND [ARG...]]"
                        );
                        return Err(XargsFailure::done());
                    }
                    b"--version" => {
                        println!("xargs (axe) {}", env!("CARGO_PKG_VERSION"));
                        return Err(XargsFailure::done());
                    }
                    _ => unreachable!(),
                }
                continue;
            }
            if !matches!(
                option,
                b"-d"
                    | b"--delimiter"
                    | b"-n"
                    | b"--max-args"
                    | b"-L"
                    | b"--max-lines"
                    | b"-s"
                    | b"--max-chars"
                    | b"-P"
                    | b"--max-procs"
                    | b"-I"
                    | b"--replace"
            ) {
                return Err(XargsFailure::usage(format!(
                    "unrecognized option {}",
                    arg.to_string_lossy()
                )));
            }
            let value = match attached {
                Some(value) => os_string_from_input(value).map_err(XargsFailure::usage)?,
                None if option == b"--replace" => OsString::from("{}"),
                None if option == b"--max-lines" => OsString::from("1"),
                None => iter.next().ok_or_else(|| {
                    XargsFailure::usage(format!(
                        "option requires an argument: {}",
                        String::from_utf8_lossy(option)
                    ))
                })?,
            };
            match option {
                b"-d" | b"--delimiter" => delimiter = Some(parse_delimiter(&value)?),
                b"-n" | b"--max-args" => {
                    max_args = Some(parse_positive(&value, "-n")?);
                    max_lines = None;
                }
                b"-L" | b"--max-lines" => {
                    max_lines = Some(parse_positive(&value, "-L")?);
                    max_args = None;
                }
                b"-s" | b"--max-chars" => max_chars = Some(parse_positive(&value, "-s")?),
                b"-P" | b"--max-procs" => max_procs = parse_nonnegative(&value, "-P")?,
                b"-I" | b"--replace" => {
                    replace = Some(value);
                    max_lines = Some(1);
                    max_args = None;
                }
                _ => unreachable!(),
            }
        }
        initial.extend(iter);

        Ok(Self {
            command: command.unwrap_or_else(|| OsString::from("echo")),
            initial,
            delimiter,
            eof,
            max_args,
            max_lines,
            max_chars,
            max_procs: if max_procs == 0 {
                std::thread::available_parallelism()
                    .map_or(1, usize::from)
                    .min(256)
            } else {
                max_procs
            },
            replace,
            no_run_if_empty,
        })
    }

    fn run(self) -> Result<i32, XargsFailure> {
        let budget = execution_budget();
        if !self.arguments_fit(&[], budget) {
            return Err(XargsFailure::usage(
                "command and initial arguments exceed the system argument limit",
            ));
        }
        let stdin = io::stdin();
        let mut records = InputRecords::new(
            BufReader::new(stdin.lock()),
            self.delimiter,
            self.replace.is_some(),
            self.eof.clone(),
            budget,
        );
        let mut active = VecDeque::new();
        let mut pending = Vec::new();
        let mut pending_lines = 0_usize;
        let mut status = 0;
        let mut saw_input = false;

        while let Some(record) = records.next_record()? {
            saw_input = true;
            if self.replace.is_some() {
                if !self.arguments_fit(&record, budget) {
                    return Err(XargsFailure::usage(
                        "expanded replacement exceeds the system argument limit",
                    ));
                }
                if !self.queue_batch(&record, budget, &mut active, &mut status)? {
                    return Ok(status);
                }
                continue;
            }

            if let Some(max_lines) = self.max_lines {
                if !pending.is_empty()
                    && (pending_lines == max_lines
                        || !self.arguments_fit_with(&pending, &record, budget))
                {
                    if !self.queue_batch(&pending, budget, &mut active, &mut status)? {
                        return Ok(status);
                    }
                    pending.clear();
                    pending_lines = 0;
                }
                if !self.arguments_fit_with(&pending, &record, budget) {
                    return Err(XargsFailure::usage(
                        "one input record exceeds the system argument limit",
                    ));
                }
                pending.extend(record);
                pending_lines += 1;
                continue;
            }

            for field in record {
                let max_args_reached = self
                    .max_args
                    .is_some_and(|maximum| pending.len() == maximum);
                if !pending.is_empty()
                    && (max_args_reached
                        || !self.arguments_fit_with(&pending, std::slice::from_ref(&field), budget))
                {
                    if !self.queue_batch(&pending, budget, &mut active, &mut status)? {
                        return Ok(status);
                    }
                    pending.clear();
                }
                if !self.arguments_fit_with(&pending, std::slice::from_ref(&field), budget) {
                    return Err(XargsFailure::usage(
                        "one input argument exceeds the system argument limit",
                    ));
                }
                pending.push(field);
            }
        }

        if !pending.is_empty() {
            if !self.queue_batch(&pending, budget, &mut active, &mut status)? {
                return Ok(status);
            }
        } else if !saw_input && !self.no_run_if_empty && self.replace.is_none() {
            self.queue_batch(&[], budget, &mut active, &mut status)?;
        }
        while let Some(child) = active.pop_front() {
            status = combine_status(status, wait_child(child)?);
        }
        Ok(status)
    }

    fn arguments_fit_with(
        &self,
        current: &[OsString],
        additional: &[OsString],
        budget: usize,
    ) -> bool {
        if self.replace.is_some() {
            return false;
        }
        let count = self.initial.len() + current.len() + additional.len();
        let argument_bytes = encoded_size(&self.command)
            .saturating_add(self.initial.iter().map(encoded_size).sum::<usize>())
            .saturating_add(current.iter().map(encoded_size).sum::<usize>())
            .saturating_add(additional.iter().map(encoded_size).sum::<usize>());
        let bytes =
            argument_bytes.saturating_add((count + 2).saturating_mul(std::mem::size_of::<usize>()));
        bytes <= budget
            && argument_bytes <= self.max_chars.unwrap_or(DEFAULT_EXECUTION_BUDGET)
            && current
                .iter()
                .chain(additional)
                .all(|argument| encoded_size(argument) <= MAX_SINGLE_ARGUMENT)
    }

    fn arguments_fit(&self, fields: &[OsString], budget: usize) -> bool {
        let args = self.build_args(fields);
        let count = args.len();
        let argument_bytes = encoded_size(&self.command)
            .saturating_add(args.iter().map(encoded_size).sum::<usize>());
        let bytes =
            argument_bytes.saturating_add((count + 2).saturating_mul(std::mem::size_of::<usize>()));
        bytes <= budget
            && argument_bytes <= self.max_chars.unwrap_or(DEFAULT_EXECUTION_BUDGET)
            && args
                .iter()
                .all(|argument| encoded_size(argument) <= MAX_SINGLE_ARGUMENT)
    }

    fn build_args(&self, fields: &[OsString]) -> Vec<OsString> {
        let mut args = self.initial.clone();
        if let Some(marker) = &self.replace {
            for arg in &mut args {
                *arg = replace_bytes(arg, marker, fields);
            }
        } else {
            args.extend(fields.iter().cloned());
        }
        args
    }

    fn queue_batch(
        &self,
        fields: &[OsString],
        budget: usize,
        active: &mut VecDeque<Child>,
        status: &mut i32,
    ) -> Result<bool, XargsFailure> {
        while active.len() >= self.max_procs {
            let child = active.pop_front().expect("active queue is non-empty");
            *status = combine_status(*status, wait_child(child)?);
            if *status == 124 {
                return Ok(false);
            }
        }
        active.push_back(self.spawn_once(fields, budget)?);
        Ok(true)
    }

    fn spawn_once(&self, fields: &[OsString], budget: usize) -> Result<Child, XargsFailure> {
        if !self.arguments_fit(fields, budget) {
            return Err(XargsFailure::usage(
                "argument batch exceeds the system argument limit",
            ));
        }
        let args = self.build_args(fields);
        let mut command = crate::child::command(&self.command)
            .map_err(|error| launch_failure(&self.command, error))?;
        command
            .stdin(Stdio::null())
            .args(args)
            .spawn()
            .map_err(|error| launch_failure(&self.command, error))
    }
}

fn rewrite_find_commands(args: &mut Vec<OsString>) -> Result<(), String> {
    let mut index = 1;
    while index < args.len() {
        if matches!(
            args[index].to_str(),
            Some("-exec" | "-execdir" | "-ok" | "-okdir")
        ) {
            let command_index = index + 1;
            crate::child::rewrite(args, command_index).map_err(|error| error.to_string())?;
            index = command_index + 1;
            while index < args.len()
                && args[index] != OsStr::new(";")
                && args[index] != OsStr::new("+")
            {
                index += 1;
            }
        }
        index += 1;
    }
    Ok(())
}

fn parse_positive(value: &OsStr, option: &str) -> Result<usize, XargsFailure> {
    value
        .to_str()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|number| *number > 0)
        .ok_or_else(|| {
            XargsFailure::usage(format!(
                "invalid positive integer for {option}: {}",
                value.to_string_lossy()
            ))
        })
}

fn parse_nonnegative(value: &OsStr, option: &str) -> Result<usize, XargsFailure> {
    value
        .to_str()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| {
            XargsFailure::usage(format!(
                "invalid nonnegative integer for {option}: {}",
                value.to_string_lossy()
            ))
        })
}

fn nonempty_os_string(value: OsString) -> Option<OsString> {
    (!value.as_encoded_bytes().is_empty()).then_some(value)
}

fn parse_delimiter(value: &OsStr) -> Result<u8, XargsFailure> {
    let bytes = value.as_encoded_bytes();
    match bytes {
        [] => Ok(0),
        [byte] => Ok(*byte),
        b"\\n" => Ok(b'\n'),
        b"\\r" => Ok(b'\r'),
        b"\\t" => Ok(b'\t'),
        b"\\0" => Ok(0),
        b"\\\\" => Ok(b'\\'),
        [b'\\', b'x', digits @ ..] if digits.len() == 2 => std::str::from_utf8(digits)
            .ok()
            .and_then(|digits| u8::from_str_radix(digits, 16).ok())
            .ok_or_else(|| XargsFailure::usage("invalid hexadecimal delimiter")),
        [b'\\', digits @ ..] if (1..=3).contains(&digits.len()) => std::str::from_utf8(digits)
            .ok()
            .and_then(|digits| u8::from_str_radix(digits, 8).ok())
            .ok_or_else(|| XargsFailure::usage("invalid octal delimiter")),
        _ => Err(XargsFailure::usage(
            "delimiter must be one byte or a supported escape",
        )),
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Quote {
    None,
    Single,
    Double,
}

struct InputRecords<R> {
    input: R,
    delimiter: Option<u8>,
    eof: Option<Vec<u8>>,
    replacement: bool,
    trailing_blank: bool,
    field: Vec<u8>,
    record: Vec<OsString>,
    quote: Quote,
    escaped: bool,
    started: bool,
    exhausted: bool,
    max_field_bytes: usize,
}

fn take_input_field(
    field: &mut Vec<u8>,
    eof: Option<&[u8]>,
    replacement: bool,
) -> Result<Option<OsString>, XargsFailure> {
    let field = os_string_from_input(&std::mem::take(field)).map_err(XargsFailure::usage)?;
    if !replacement && eof.is_some_and(|eof| eof == field.as_encoded_bytes()) {
        return Ok(None);
    }
    Ok(Some(field))
}

fn replacement_record_is_eof(record: &[OsString], eof: Option<&[u8]>, replacement: bool) -> bool {
    replacement && record.len() == 1 && eof.is_some_and(|eof| eof == record[0].as_encoded_bytes())
}
impl<R: BufRead> InputRecords<R> {
    fn new(
        input: R,
        delimiter: Option<u8>,
        replacement: bool,
        eof: Option<OsString>,
        budget: usize,
    ) -> Self {
        Self {
            input,
            delimiter,
            eof: eof.map(|value| value.as_encoded_bytes().to_vec()),
            replacement,
            trailing_blank: false,
            field: Vec::new(),
            record: Vec::new(),
            quote: Quote::None,
            escaped: false,
            started: false,
            exhausted: false,
            max_field_bytes: budget.min(MAX_SINGLE_ARGUMENT),
        }
    }

    fn next_record(&mut self) -> Result<Option<Vec<OsString>>, XargsFailure> {
        if self.exhausted {
            return Ok(None);
        }
        if let Some(delimiter) = self.delimiter {
            return self.next_delimited(delimiter);
        }
        self.next_quoted()
    }

    fn next_delimited(&mut self, delimiter: u8) -> Result<Option<Vec<OsString>>, XargsFailure> {
        loop {
            let (consumed, terminated) = {
                let available = self
                    .input
                    .fill_buf()
                    .map_err(|error| XargsFailure::usage(error.to_string()))?;
                if available.is_empty() {
                    self.exhausted = true;
                    if self.field.is_empty() {
                        return Ok(None);
                    }
                    let field = os_string_from_input(&std::mem::take(&mut self.field))
                        .map_err(XargsFailure::usage)?;
                    return Ok(Some(vec![field]));
                }
                let delimiter_at = available.iter().position(|byte| *byte == delimiter);
                let field_bytes = delimiter_at.map_or(available, |index| &available[..index]);
                self.field.extend_from_slice(field_bytes);
                if self.field.len() > self.max_field_bytes {
                    return Err(XargsFailure::usage(
                        "one input argument exceeds the system argument limit",
                    ));
                }
                (
                    delimiter_at.map_or(available.len(), |index| index + 1),
                    delimiter_at.is_some(),
                )
            };
            self.input.consume(consumed);
            if terminated {
                let field = os_string_from_input(&std::mem::take(&mut self.field))
                    .map_err(XargsFailure::usage)?;
                return Ok(Some(vec![field]));
            }
        }
    }

    fn next_quoted(&mut self) -> Result<Option<Vec<OsString>>, XargsFailure> {
        loop {
            let mut completed = None;
            let consumed = {
                let available = self
                    .input
                    .fill_buf()
                    .map_err(|error| XargsFailure::usage(error.to_string()))?;
                if available.is_empty() {
                    self.exhausted = true;
                    if self.escaped {
                        return Err(XargsFailure::usage("unmatched backslash at end of input"));
                    }
                    if self.quote != Quote::None {
                        return Err(XargsFailure::usage("unmatched quote in input"));
                    }
                    if self.started {
                        if let Some(field) = take_input_field(
                            &mut self.field,
                            self.eof.as_deref(),
                            self.replacement,
                        )? {
                            self.record.push(field);
                        }
                        self.started = false;
                    }
                    if replacement_record_is_eof(
                        &self.record,
                        self.eof.as_deref(),
                        self.replacement,
                    ) {
                        self.record.clear();
                        return Ok(None);
                    }
                    return if self.record.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(std::mem::take(&mut self.record)))
                    };
                }

                let mut consumed = 0;
                for &byte in available {
                    consumed += 1;
                    let continued_line = self.trailing_blank;
                    self.trailing_blank = matches!(byte, b' ' | b'\t');
                    if !self.started
                        && self.quote == Quote::None
                        && !self.escaped
                        && matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c')
                    {
                        continue;
                    }
                    if self.escaped {
                        self.field.push(byte);
                        self.started = true;
                        self.escaped = false;
                    } else {
                        match (self.quote, byte) {
                            (Quote::Single | Quote::Double, b'\n') => {
                                let quote = if self.quote == Quote::Single {
                                    "single"
                                } else {
                                    "double"
                                };
                                return Err(XargsFailure::usage(format!(
                                    "unmatched {quote} quote; by default quotes are special to xargs \
                                     unless you use the -0 option"
                                )));
                            }
                            (Quote::Single, b'\'') | (Quote::Double, b'"') => {
                                self.quote = Quote::None;
                            }
                            (Quote::Single, byte) => self.field.push(byte),
                            (Quote::Double, byte) => self.field.push(byte),
                            (Quote::None, b'\\') => self.escaped = true,
                            (Quote::None, b'\'') => {
                                self.quote = Quote::Single;
                                self.started = true;
                            }
                            (Quote::None, b'"') => {
                                self.quote = Quote::Double;
                                self.started = true;
                            }
                            (Quote::None, b' ' | b'\t') if self.replacement => {
                                if self.started {
                                    self.field.push(byte);
                                }
                            }
                            (Quote::None, b' ' | b'\t' | b'\n') => {
                                if self.started {
                                    if let Some(field) = take_input_field(
                                        &mut self.field,
                                        self.eof.as_deref(),
                                        self.replacement,
                                    )? {
                                        self.record.push(field);
                                    } else {
                                        self.exhausted = true;
                                    }
                                    self.started = false;
                                }
                                if self.exhausted {
                                    completed = (!self.record.is_empty())
                                        .then(|| std::mem::take(&mut self.record));
                                    break;
                                }
                                if byte == b'\n'
                                    && replacement_record_is_eof(
                                        &self.record,
                                        self.eof.as_deref(),
                                        self.replacement,
                                    )
                                {
                                    self.record.clear();
                                    self.exhausted = true;
                                    break;
                                }
                                if byte == b'\n'
                                    && (self.replacement || !continued_line)
                                    && !self.record.is_empty()
                                {
                                    completed = Some(std::mem::take(&mut self.record));
                                    break;
                                }
                            }
                            (Quote::None, byte) => {
                                self.field.push(byte);
                                self.started = true;
                            }
                        }
                    }
                    if self.field.len() > self.max_field_bytes {
                        return Err(XargsFailure::usage(
                            "one input argument exceeds the system argument limit",
                        ));
                    }
                }
                consumed
            };
            self.input.consume(consumed);
            if self.exhausted || completed.is_some() {
                return Ok(completed);
            }
        }
    }
}

fn encoded_size(value: &OsString) -> usize {
    value.as_encoded_bytes().len().saturating_add(1)
}

fn execution_budget() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: sysconf has no pointer arguments and _SC_ARG_MAX is valid.
        let configured = unsafe { libc::sysconf(libc::_SC_ARG_MAX) };
        let arg_max = usize::try_from(configured)
            .ok()
            .filter(|value| *value != 0)
            .unwrap_or(DEFAULT_EXECUTION_BUDGET);
        let mut environment_bytes = 0_usize;
        let mut environment_count = 0_usize;
        for (key, value) in std::env::vars_os() {
            environment_bytes = environment_bytes
                .saturating_add(encoded_size(&key))
                .saturating_add(encoded_size(&value));
            environment_count += 1;
        }
        arg_max
            .saturating_sub(environment_bytes)
            .saturating_sub((environment_count + 1).saturating_mul(std::mem::size_of::<usize>()))
            .saturating_sub(2048)
    }
    #[cfg(not(unix))]
    DEFAULT_EXECUTION_BUDGET
}

#[cfg(unix)]
fn os_string_from_input(bytes: &[u8]) -> Result<OsString, String> {
    Ok(OsString::from_vec(bytes.to_vec()))
}

fn launch_failure(command: &OsStr, error: io::Error) -> XargsFailure {
    XargsFailure {
        code: if error.kind() == io::ErrorKind::NotFound {
            127
        } else {
            126
        },
        message: Some(format!("{}: {error}", command.to_string_lossy())),
    }
}

fn wait_child(mut child: Child) -> Result<i32, XargsFailure> {
    child
        .wait()
        .map(map_status)
        .map_err(|error| XargsFailure::usage(error.to_string()))
}

fn combine_status(current: i32, next: i32) -> i32 {
    match (current, next) {
        (_, 124) => 124,
        (124, _) => 124,
        (_, 125) => 125,
        (125, _) => 125,
        (0, status) => status,
        (status, 0) => status,
        _ => 123,
    }
}

#[cfg(not(unix))]
fn os_string_from_input(bytes: &[u8]) -> Result<OsString, String> {
    String::from_utf8(bytes.to_vec())
        .map(OsString::from)
        .map_err(|_| "non-UTF-8 xargs input is unsupported on this platform".to_owned())
}

#[cfg(unix)]
fn replace_bytes(arg: &OsStr, marker: &OsStr, fields: &[OsString]) -> OsString {
    let replacement = fields
        .first()
        .map_or(b"".as_slice(), |field| field.as_bytes());

    let source = arg.as_bytes();
    let needle = marker.as_bytes();
    if needle.is_empty() {
        return arg.to_owned();
    }

    let mut output = Vec::with_capacity(source.len());
    let mut rest = source;
    while let Some(offset) = rest
        .windows(needle.len())
        .position(|window| window == needle)
    {
        output.extend_from_slice(&rest[..offset]);
        output.extend_from_slice(replacement);
        rest = &rest[offset + needle.len()..];
    }

    output.extend_from_slice(rest);
    OsString::from_vec(output)
}

#[cfg(not(unix))]
fn replace_bytes(arg: &OsStr, marker: &OsStr, fields: &[OsString]) -> OsString {
    let replacement = fields.first().map_or_else(
        || std::borrow::Cow::Borrowed(""),
        |field| field.to_string_lossy(),
    );
    arg.to_string_lossy()
        .replace(&*marker.to_string_lossy(), &replacement)
        .into()
}

fn map_status(status: ExitStatus) -> i32 {
    match status.code() {
        Some(0) => 0,
        Some(255) => 124,
        Some(_) => 123,
        None => 125,
    }
}
