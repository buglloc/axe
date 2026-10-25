use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::cli::Options;
use crate::command::{Address, AddressRange, Command, SedCommand};
use crate::error::{Error, Result};
use crate::input::{
    FilesInput, Input, LineReader, ReaderInput, Record, read_record, report_path,
};
use crate::output::Output;
use crate::regex::{PosixRegex, utf8_locale};
use crate::replacement::{ReplacePart, apply_substitution, compile_replacement};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

enum CompiledAddress {
    Line(usize),
    Last,
    Regex(PosixRegex),
    Step { first: usize, step: usize },
    Plus(usize),
    Tilde(usize),
}

struct CompiledAddressRange {
    kind: CompiledAddressKind,
    negated: bool,
}

enum CompiledAddressKind {
    None,
    Single(CompiledAddress),
    Range(CompiledAddress, CompiledAddress),
}

impl CompiledAddressRange {
    fn none() -> Self {
        Self {
            kind: CompiledAddressKind::None,
            negated: false,
        }
    }
}

struct CompiledSubstitute {
    pattern: PosixRegex,
    replacement: Vec<ReplacePart>,
    global: bool,
    print: bool,
    nth: Option<usize>,
    write_file: Option<Vec<u8>>,
}

enum CompiledCommand {
    ScopeStart,
    ScopeEnd,
    Substitute(CompiledSubstitute),
    Transliterate {
        from: Vec<u8>,
        to: Vec<u8>,
        utf8: bool,
    },
    Print,
    PrintFirstLine,
    PrintLineNumber,
    List,
    Delete,
    DeleteFirstLine,
    Next,
    NextAppend,
    HoldReplace,
    HoldAppend,
    GetReplace,
    GetAppend,
    Exchange,
    Append(Vec<u8>),
    Insert(Vec<u8>),
    Change(Vec<u8>),
    Label,
    Branch(Option<Vec<u8>>),
    BranchIfSub(Option<Vec<u8>>),
    BranchIfNotSub(Option<Vec<u8>>),
    ReadFile(Vec<u8>),
    ReadLine(Vec<u8>),
    WriteFile(Vec<u8>),
    WriteFirstLine(Vec<u8>),
    PrintFileName,
    Execute(Option<Vec<u8>>),
    Quit(i32),
    QuitNoprint(i32),
    ClearPattern,
    Noop,
}

struct CompiledSedCommand {
    address: CompiledAddressRange,
    command: CompiledCommand,
}

enum Flow {
    Continue,
    Restart,
    RestartScript,
    Branch(Option<Vec<u8>>),
    Quit(i32),
    QuitNoPrint(i32),
    EndInput { print: bool },
}

struct Outcome {
    code: i32,
    quit: bool,
}

pub struct Engine {
    commands: Vec<CompiledSedCommand>,
    labels: HashMap<Vec<u8>, usize>,
    quiet: bool,
    delimiter: u8,
    separate: bool,
    follow_symlinks: bool,
    write_files: Mutex<HashMap<Vec<u8>, fs::File>>,
}

struct State {
    line_number: usize,
    pattern_space: Record,
    hold_space: Record,
    append_queue: Vec<Vec<u8>>,
    last_sub_success: bool,
    range_active: Vec<bool>,
    range_start: Vec<usize>,
    filename: usize,
    r_readers: HashMap<Vec<u8>, Option<io::BufReader<fs::File>>>,
}

impl State {
    fn new(commands: &[CompiledSedCommand]) -> Self {
        Self {
            line_number: 0,
            pattern_space: Record::default(),
            hold_space: Record {
                data: Vec::new(),
                terminated: true,
            },
            append_queue: Vec::new(),
            last_sub_success: false,
            range_active: commands
                .iter()
                .map(|command| {
                    matches!(
                        &command.address.kind,
                        CompiledAddressKind::Range(CompiledAddress::Line(0), _)
                    )
                })
                .collect(),
            range_start: vec![0; commands.len()],
            filename: 0,
            r_readers: HashMap::new(),
        }
    }

    fn accept(&mut self, record: crate::input::InputRecord) {
        self.line_number += 1;
        self.pattern_space = record.record;
        self.filename = record.source;
        self.last_sub_success = false;
    }
}

impl Engine {
    pub fn new(parsed: Vec<SedCommand>, options: &Options) -> Result<Self> {
        let locale_utf8 = utf8_locale();
        let mut commands = Vec::new();
        let mut labels = HashMap::new();
        flatten_and_compile(
            parsed,
            &mut commands,
            &mut labels,
            options.extended_regexp,
            locale_utf8,
        )?;
        let write_files = open_write_files(&commands)?;
        Ok(Self {
            commands,
            labels,
            quiet: options.quiet,
            delimiter: if options.null_data { 0 } else { b'\n' },
            separate: options.separate,
            follow_symlinks: options.follow_symlinks,
            write_files: Mutex::new(write_files),
        })
    }

    /// Process stdin or the named files, returning the sed exit status after flushing stdout.
    pub fn run(&self, files: &[PathBuf]) -> Result<i32> {
        let stdout = io::stdout();
        let mut output = Output::new(io::BufWriter::new(stdout.lock()), self.delimiter);
        let result = if files.is_empty() {
            self.process(
                ReaderInput::new(io::stdin().lock(), OsStr::new("-"), self.delimiter),
                &mut output,
            )
            .map(|outcome| outcome.code)
        } else if self.separate {
            self.run_separate(files, &mut output)
        } else {
            self.process(FilesInput::new(files, self.delimiter), &mut output)
                .map(|outcome| outcome.code)
        };
        let flushed = output.flush();
        let code = result?;
        flushed?;
        Ok(code)
    }

    fn run_separate<W: Write>(&self, files: &[PathBuf], output: &mut Output<W>) -> Result<i32> {
        let stdin = io::stdin();
        let mut stdin = stdin.lock();
        let mut code = 0;
        for path in files {
            let outcome = if path == Path::new("-") {
                self.process(
                    ReaderInput::new(&mut stdin, path.as_os_str(), self.delimiter),
                    output,
                )?
            } else {
                let file = match fs::File::open(path) {
                    Ok(file) => file,
                    Err(error) => {
                        report_path("can't read", path, &error);
                        code = 2;
                        continue;
                    }
                };
                self.process(
                    ReaderInput::new(io::BufReader::new(file), path.as_os_str(), self.delimiter),
                    output,
                )?
            };
            if outcome.quit {
                return Ok(if code == 0 { outcome.code } else { code });
            }
            if outcome.code != 0 {
                code = outcome.code;
            }
        }
        Ok(code)
    }

    /// Each in-place file has an independent sed state and a same-directory temporary.
    /// Publication and backup renaming happen only after successful reads and writes.
    pub fn run_in_place(&self, files: &[PathBuf], backup_suffix: &OsStr) -> Result<i32> {
        let mut code = 0;
        for path in files {
            let outcome = match self.edit_file(path, backup_suffix) {
                Ok(outcome) => outcome,
                Err(Error::Input(error)) => {
                    report_path("can't read", path, &error);
                    code = 2;
                    continue;
                }
                Err(Error::Io(error)) => {
                    report_path("couldn't edit", path, &error);
                    return Ok(4);
                }
                Err(error) => {
                    eprintln!("sed: {error}");
                    return Ok(4);
                }
            };
            if outcome.quit {
                return Ok(if code == 0 { outcome.code } else { code });
            }
            if outcome.code != 0 {
                code = outcome.code;
            }
        }
        Ok(code)
    }

    fn edit_file(&self, path: &Path, suffix: &OsStr) -> Result<Outcome> {
        if path == Path::new("-") {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file").into());
        }
        let (destination, input, metadata) = crate::in_place::open(path, self.follow_symlinks)?;
        let mut temporary = crate::in_place::temporary(&destination)?;
        let outcome;
        {
            let mut output =
                Output::new(io::BufWriter::new(temporary.as_file_mut()), self.delimiter);
            let result = self.process(
                ReaderInput::new(io::BufReader::new(input), path.as_os_str(), self.delimiter),
                &mut output,
            );
            let flushed = output.flush();
            outcome = result?;
            flushed?;
        }
        crate::in_place::publish(temporary, &destination, &metadata, suffix)?;
        Ok(outcome)
    }

    pub fn process_stream<R: BufRead, W: Write>(&self, reader: R, writer: &mut W) -> Result<i32> {
        let mut output = Output::new(writer, self.delimiter);
        let result = self.process(
            ReaderInput::new(reader, OsStr::new("-"), self.delimiter),
            &mut output,
        );
        let flushed = output.flush();
        let outcome = result?;
        flushed?;
        Ok(outcome.code)
    }

    fn process<I: Input, W: Write>(&self, input: I, output: &mut Output<W>) -> Result<Outcome> {
        let mut input = LineReader::new(input);
        let mut state = State::new(&self.commands);
        while let Some(record) = input.read_line()? {
            state.accept(record);
            state.append_queue.clear();
            loop {
                match self.execute_all(&mut state, &mut input, output)? {
                    Flow::Restart => {
                        self.flush_appends(&state, output)?;
                        break;
                    }
                    Flow::RestartScript => continue,
                    Flow::Quit(code) => {
                        if !self.quiet {
                            output.record(&state.pattern_space)?;
                        }
                        self.flush_appends(&state, output)?;
                        let status = input.status();
                        return Ok(Outcome {
                            code: if status == 0 { code } else { status },
                            quit: true,
                        });
                    }
                    Flow::QuitNoPrint(code) => {
                        let status = input.status();
                        return Ok(Outcome {
                            code: if status == 0 { code } else { status },
                            quit: true,
                        });
                    }
                    Flow::EndInput { print } => {
                        if print && !self.quiet {
                            output.record(&state.pattern_space)?;
                        }
                        self.flush_appends(&state, output)?;
                        return Ok(Outcome {
                            code: input.status(),
                            quit: false,
                        });
                    }
                    Flow::Continue | Flow::Branch(_) => {
                        if !self.quiet {
                            output.record(&state.pattern_space)?;
                        }
                        self.flush_appends(&state, output)?;
                        break;
                    }
                }
            }
        }
        Ok(Outcome {
            code: input.status(),
            quit: false,
        })
    }

    fn execute_all<I: Input, W: Write>(
        &self,
        state: &mut State,
        input: &mut LineReader<I>,
        output: &mut Output<W>,
    ) -> Result<Flow> {
        let mut pc = 0;
        let mut scope_depth = 0usize;
        let mut skip_depth = None;
        while pc < self.commands.len() {
            let command = &self.commands[pc];
            if let Some(depth) = skip_depth {
                match &command.command {
                    CompiledCommand::ScopeStart => scope_depth += 1,
                    CompiledCommand::ScopeEnd => {
                        if scope_depth == depth {
                            skip_depth = None;
                        }
                        scope_depth = scope_depth.saturating_sub(1);
                    }
                    _ => {}
                }
                pc += 1;
                continue;
            }
            match &command.command {
                CompiledCommand::ScopeStart => {
                    scope_depth += 1;
                    if !self.address_matches(&command.address, state, pc, input)? {
                        skip_depth = Some(scope_depth);
                    }
                    pc += 1;
                    continue;
                }
                CompiledCommand::ScopeEnd => {
                    scope_depth = scope_depth.saturating_sub(1);
                    pc += 1;
                    continue;
                }
                _ => {}
            }
            if !self.address_matches(&command.address, state, pc, input)? {
                pc += 1;
                continue;
            }
            match self.execute_one(&command.command, state, input, output)? {
                Flow::Continue => pc += 1,
                Flow::Branch(label) => {
                    let Some(target) = label.and_then(|label| self.labels.get(&label)) else {
                        return Ok(Flow::Continue);
                    };
                    pc = *target;
                }
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Continue)
    }

    fn execute_one<I: Input, W: Write>(
        &self,
        command: &CompiledCommand,
        state: &mut State,
        input: &mut LineReader<I>,
        output: &mut Output<W>,
    ) -> Result<Flow> {
        match command {
            CompiledCommand::Substitute(sub) => {
                if let Some(data) = apply_substitution(
                    &sub.pattern,
                    &state.pattern_space.data,
                    &sub.replacement,
                    sub.global,
                    sub.nth,
                )? {
                    state.pattern_space.data = data;
                    state.last_sub_success = true;
                    if sub.print {
                        output.record(&state.pattern_space)?;
                    }
                    if let Some(path) = &sub.write_file {
                        self.write_file(path, &state.pattern_space, false)?;
                    }
                }
            }
            CompiledCommand::Transliterate { from, to, utf8 } => {
                state.pattern_space.data = transliterate(&state.pattern_space.data, from, to, *utf8)?;
            }
            CompiledCommand::Delete => return Ok(Flow::Restart),
            CompiledCommand::DeleteFirstLine => {
                if let Some(index) = state
                    .pattern_space
                    .data
                    .iter()
                    .position(|&byte| byte == self.delimiter)
                {
                    state.pattern_space.data.drain(..=index);
                    return Ok(Flow::RestartScript);
                }
                return Ok(Flow::Restart);
            }
            CompiledCommand::Print => output.record(&state.pattern_space)?,
            CompiledCommand::PrintFirstLine => {
                let (data, terminated) = self.first_line(&state.pattern_space);
                output.bytes(data, terminated)?;
            }
            CompiledCommand::PrintLineNumber => {
                output.bytes(state.line_number.to_string().as_bytes(), true)?;
            }
            CompiledCommand::List => {
                output.bytes(&list_escape(&state.pattern_space.data, self.delimiter), true)?;
            }
            CompiledCommand::Next => {
                if !self.quiet {
                    output.record(&state.pattern_space)?;
                }
                self.flush_appends(state, output)?;
                state.append_queue.clear();
                let Some(record) = input.read_line()? else {
                    return Ok(Flow::EndInput { print: false });
                };
                state.accept(record);
            }
            CompiledCommand::NextAppend => {
                self.flush_appends(state, output)?;
                state.append_queue.clear();
                let Some(record) = input.read_line()? else {
                    return Ok(Flow::EndInput { print: true });
                };
                state.line_number += 1;
                state.filename = record.source;
                state.last_sub_success = false;
                state.pattern_space.data.push(self.delimiter);
                state.pattern_space.data.extend_from_slice(&record.record.data);
                state.pattern_space.terminated = record.record.terminated;
            }
            CompiledCommand::HoldReplace => state.hold_space.clone_from(&state.pattern_space),
            CompiledCommand::HoldAppend => {
                state.hold_space.data.push(self.delimiter);
                state
                    .hold_space
                    .data
                    .extend_from_slice(&state.pattern_space.data);
                state.hold_space.terminated = state.pattern_space.terminated;
            }
            CompiledCommand::GetReplace => state.pattern_space.clone_from(&state.hold_space),
            CompiledCommand::GetAppend => {
                state.pattern_space.data.push(self.delimiter);
                state
                    .pattern_space
                    .data
                    .extend_from_slice(&state.hold_space.data);
                state.pattern_space.terminated = state.hold_space.terminated;
            }
            CompiledCommand::Exchange => {
                std::mem::swap(&mut state.pattern_space, &mut state.hold_space);
            }
            CompiledCommand::Append(text) => {
                let mut text = text.clone();
                text.push(self.delimiter);
                state.append_queue.push(text);
            }
            CompiledCommand::Insert(text) => output.bytes(text, true)?,
            CompiledCommand::Change(text) => {
                output.bytes(text, true)?;
                return Ok(Flow::Restart);
            }
            CompiledCommand::Branch(label) => return Ok(Flow::Branch(label.clone())),
            CompiledCommand::BranchIfSub(label) => {
                if state.last_sub_success {
                    state.last_sub_success = false;
                    return Ok(Flow::Branch(label.clone()));
                }
            }
            CompiledCommand::BranchIfNotSub(label) => {
                if !state.last_sub_success {
                    return Ok(Flow::Branch(label.clone()));
                }
            }
            CompiledCommand::ReadFile(path) => {
                if let Ok(data) = fs::read(path_from_bytes(path)?) {
                    state.append_queue.push(data);
                }
            }
            CompiledCommand::ReadLine(path) => {
                let reader = state
                    .r_readers
                    .entry(path.clone())
                    .or_insert_with(|| {
                        fs::File::open(path_from_bytes(path).ok()?).ok().map(io::BufReader::new)
                    });
                if let Some(reader) = reader {
                    if let Ok(Some(record)) = read_record(reader, self.delimiter) {
                        let mut data = record.data;
                        if record.terminated {
                            data.push(self.delimiter);
                        }
                        state.append_queue.push(data);
                    }
                }
            }
            CompiledCommand::WriteFile(path) => self.write_file(path, &state.pattern_space, false)?,
            CompiledCommand::WriteFirstLine(path) => {
                self.write_file(path, &state.pattern_space, true)?
            }
            CompiledCommand::PrintFileName => {
                output.bytes(input.filename(state.filename).as_encoded_bytes(), true)?;
            }
            CompiledCommand::Execute(command) => {
                let to_run = match command {
                    Some(text) => os_string_from_bytes(text)?,
                    None => {
                        #[cfg(unix)]
                        {
                            OsString::from_vec(state.pattern_space.data.clone())
                        }
                        #[cfg(not(unix))]
                        {
                            String::from_utf8(state.pattern_space.data.clone())
                                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
                                .into()
                        }
                    }
                };
                let result = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(to_run)
                    .output()?;
                if command.is_some() {
                    output.raw(&result.stdout)?;
                } else {
                    let mut data = result.stdout;
                    if data.last() == Some(&b'\n') {
                        data.pop();
                    }
                    state.pattern_space.data = data;
                }
            }
            CompiledCommand::Quit(code) => return Ok(Flow::Quit(*code)),
            CompiledCommand::QuitNoprint(code) => return Ok(Flow::QuitNoPrint(*code)),
            CompiledCommand::ClearPattern => state.pattern_space.data.clear(),
            CompiledCommand::Label
            | CompiledCommand::Noop
            | CompiledCommand::ScopeStart
            | CompiledCommand::ScopeEnd => {}
        }
        Ok(Flow::Continue)
    }

    fn first_line<'a>(&self, record: &'a Record) -> (&'a [u8], bool) {
        match record.data.iter().position(|&byte| byte == self.delimiter) {
            Some(index) => (&record.data[..index], true),
            None => (&record.data, record.terminated),
        }
    }

    fn write_file(&self, path: &[u8], record: &Record, first: bool) -> Result<()> {
        let mut files = self
            .write_files
            .lock()
            .map_err(|_| io::Error::other("w file handle lock poisoned"))?;
        let file = files
            .get_mut(path)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "w file was not opened"))?;
        let (data, terminated) = if first {
            self.first_line(record)
        } else {
            (&record.data[..], record.terminated)
        };
        file.write_all(data)?;
        if terminated {
            file.write_all(&[self.delimiter])?;
        }
        Ok(())
    }

    fn flush_appends<W: Write>(&self, state: &State, output: &mut Output<W>) -> Result<()> {
        for text in &state.append_queue {
            output.raw(text)?;
        }
        Ok(())
    }

    fn address_matches<I: Input>(
        &self,
        address: &CompiledAddressRange,
        state: &mut State,
        index: usize,
        input: &mut LineReader<I>,
    ) -> Result<bool> {
        let matches = match &address.kind {
            CompiledAddressKind::None => true,
            CompiledAddressKind::Single(address) => address_matches(address, state, input)?,
            CompiledAddressKind::Range(start, end) => {
                if state.range_active[index] {
                    let start_line = state.range_start[index];
                    let last = match end {
                        CompiledAddress::Line(line) => *line <= state.line_number,
                        CompiledAddress::Plus(lines) => {
                            state.line_number >= start_line.saturating_add(*lines)
                        }
                        CompiledAddress::Tilde(step) => {
                            *step == 0
                                || (state.line_number > start_line
                                    && state.line_number % step == 0)
                        }
                        _ => address_matches(end, state, input)?,
                    };
                    if last {
                        state.range_active[index] = false;
                    }
                    true
                } else if address_matches(start, state, input)? {
                    state.range_start[index] = state.line_number;
                    let immediate = match end {
                        CompiledAddress::Line(line) => *line <= state.line_number,
                        CompiledAddress::Plus(lines) => *lines == 0,
                        CompiledAddress::Tilde(step) => *step == 0,
                        _ => false,
                    };
                    state.range_active[index] = !immediate;
                    true
                } else {
                    false
                }
            }
        };
        Ok(if address.negated { !matches } else { matches })
    }
}

fn address_matches<I: Input>(
    address: &CompiledAddress,
    state: &State,
    input: &mut LineReader<I>,
) -> Result<bool> {
    Ok(match address {
        CompiledAddress::Line(line) => state.line_number == *line,
        CompiledAddress::Last => input.is_last()?,
        CompiledAddress::Regex(regex) => regex.is_match(&state.pattern_space.data)?,
        CompiledAddress::Step { first, step } => {
            if *step == 0 {
                state.line_number == *first
            } else if *first == 0 {
                state.line_number % step == 0
            } else {
                state.line_number >= *first && (state.line_number - first) % step == 0
            }
        }
        CompiledAddress::Plus(_) | CompiledAddress::Tilde(_) => false,
    })
}

fn transliterate(input: &[u8], from: &[u8], to: &[u8], utf8: bool) -> Result<Vec<u8>> {
    if !utf8 {
        let mut output = Vec::with_capacity(input.len());
        for &byte in input {
            output.push(
                from
                    .iter()
                    .position(|&candidate| candidate == byte)
                    .map_or(byte, |index| to[index]),
            );
        }
        return Ok(output);
    }

    let from = std::str::from_utf8(from)
        .map_err(|error| Error::Parse(format!("invalid UTF-8 y source: {error}")))?;
    let to = std::str::from_utf8(to)
        .map_err(|error| Error::Parse(format!("invalid UTF-8 y destination: {error}")))?;
    let from: Vec<char> = from.chars().collect();
    let to: Vec<char> = to.chars().collect();
    if from.len() != to.len() {
        return Err(Error::Parse(format!(
            "y command: 'from' and 'to' must be same length ({} vs {})",
            from.len(),
            to.len()
        )));
    }
    let mut output = Vec::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        let (valid, invalid) = match std::str::from_utf8(rest) {
            Ok(text) => (text, 0),
            Err(error) => {
                let valid = std::str::from_utf8(&rest[..error.valid_up_to()])
                    .map_err(|error| Error::Parse(format!("invalid UTF-8 input: {error}")))?;
                (
                    valid,
                    error
                        .error_len()
                        .unwrap_or(rest.len() - error.valid_up_to()),
                )
            }
        };
        for ch in valid.chars() {
            let replacement = from
                .iter()
                .position(|&candidate| candidate == ch)
                .map(|index| to[index])
                .unwrap_or(ch);
            let mut bytes = [0; 4];
            output.extend_from_slice(replacement.encode_utf8(&mut bytes).as_bytes());
        }
        output.extend_from_slice(&rest[valid.len()..valid.len() + invalid]);
        rest = &rest[valid.len() + invalid..];
    }
    Ok(output)
}

fn list_escape(input: &[u8], delimiter: u8) -> Vec<u8> {
    let mut output = Vec::new();
    for &byte in input {
        match byte {
            b'\\' => output.extend_from_slice(b"\\\\"),
            7 => output.extend_from_slice(b"\\a"),
            8 => output.extend_from_slice(b"\\b"),
            9 => output.extend_from_slice(b"\\t"),
            10 => output.extend_from_slice(b"\\n"),
            11 => output.extend_from_slice(b"\\v"),
            12 => output.extend_from_slice(b"\\f"),
            13 => output.extend_from_slice(b"\\r"),
            32..=126 if byte != delimiter => output.push(byte),
            _ => output.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    output.push(b'$');
    output
}

fn flatten_and_compile(
    parsed: Vec<SedCommand>,
    commands: &mut Vec<CompiledSedCommand>,
    labels: &mut HashMap<Vec<u8>, usize>,
    extended: bool,
    locale_utf8: bool,
) -> Result<()> {
    for command in parsed {
        let address = compile_address_range(command.address, extended, locale_utf8)?;
        match command.command {
            Command::Block(block) => {
                commands.push(CompiledSedCommand {
                    address,
                    command: CompiledCommand::ScopeStart,
                });
                flatten_and_compile(block, commands, labels, extended, locale_utf8)?;
                commands.push(CompiledSedCommand {
                    address: CompiledAddressRange::none(),
                    command: CompiledCommand::ScopeEnd,
                });
            }
            Command::Label(name) => {
                labels.insert(name, commands.len());
                commands.push(CompiledSedCommand {
                    address,
                    command: CompiledCommand::Label,
                });
            }
            command => commands.push(CompiledSedCommand {
                address,
                command: compile_command(command, extended, locale_utf8)?,
            }),
        }
    }
    Ok(())
}

fn compile_address_range(
    range: AddressRange,
    extended: bool,
    locale_utf8: bool,
) -> Result<CompiledAddressRange> {
    Ok(match range {
        AddressRange::None => CompiledAddressRange::none(),
        AddressRange::Single { addr, negated } => {
            if matches!(addr, Address::Line(0) | Address::Plus(_) | Address::Tilde(_)) {
                return Err(Error::Parse("invalid usage of address".into()));
            }
            CompiledAddressRange {
                kind: CompiledAddressKind::Single(compile_address(addr, extended, locale_utf8)?),
                negated,
            }
        }
        AddressRange::Range {
            start,
            end,
            negated,
        } => {
            if matches!(start, Address::Plus(_) | Address::Tilde(_))
                || matches!(end, Address::Line(0))
                || (matches!(start, Address::Line(0)) && !matches!(end, Address::Regex(_)))
            {
                return Err(Error::Parse("invalid usage of line address 0 or relative address".into()));
            }
            CompiledAddressRange {
                kind: CompiledAddressKind::Range(
                    compile_address(start, extended, locale_utf8)?,
                    compile_address(end, extended, locale_utf8)?,
                ),
                negated,
            }
        }
    })
}

fn compile_address(address: Address, extended: bool, locale_utf8: bool) -> Result<CompiledAddress> {
    Ok(match address {
        Address::Line(line) => CompiledAddress::Line(line),
        Address::Last => CompiledAddress::Last,
        Address::Regex(pattern) => {
            CompiledAddress::Regex(PosixRegex::new(&pattern, extended, false, locale_utf8)?)
        }
        Address::Step { first, step } => CompiledAddress::Step { first, step },
        Address::Plus(lines) => CompiledAddress::Plus(lines),
        Address::Tilde(step) => CompiledAddress::Tilde(step),
    })
}

fn compile_command(command: Command, extended: bool, locale_utf8: bool) -> Result<CompiledCommand> {
    Ok(match command {
        Command::Substitute(sub) => CompiledCommand::Substitute(CompiledSubstitute {
            pattern: PosixRegex::new(&sub.pattern, extended, sub.case_insensitive, locale_utf8)?,
            replacement: compile_replacement(&sub.replacement),
            global: sub.global,
            print: sub.print,
            nth: sub.nth,
            write_file: sub.write_file,
        }),
        Command::Transliterate { from, to } => {
            let use_utf8 = locale_utf8
                && std::str::from_utf8(&from).is_ok()
                && std::str::from_utf8(&to).is_ok();
            if use_utf8 {
                let from_count = std::str::from_utf8(&from)
                    .map_err(|error| Error::Parse(format!("invalid UTF-8 y source: {error}")))?
                    .chars()
                    .count();
                let to_count = std::str::from_utf8(&to)
                    .map_err(|error| Error::Parse(format!("invalid UTF-8 y destination: {error}")))?
                    .chars()
                    .count();
                if from_count != to_count {
                    return Err(Error::Parse(format!(
                        "y command: 'from' and 'to' must be same length ({from_count} vs {to_count})"
                    )));
                }
            } else if from.len() != to.len() {
                return Err(Error::Parse(format!(
                    "y command: 'from' and 'to' must be same length ({} vs {})",
                    from.len(),
                    to.len()
                )));
            }
            CompiledCommand::Transliterate {
                from,
                to,
                utf8: use_utf8,
            }
        }
        Command::Print => CompiledCommand::Print,
        Command::PrintFirstLine => CompiledCommand::PrintFirstLine,
        Command::PrintLineNumber => CompiledCommand::PrintLineNumber,
        Command::List => CompiledCommand::List,
        Command::Delete => CompiledCommand::Delete,
        Command::DeleteFirstLine => CompiledCommand::DeleteFirstLine,
        Command::Next => CompiledCommand::Next,
        Command::NextAppend => CompiledCommand::NextAppend,
        Command::HoldReplace => CompiledCommand::HoldReplace,
        Command::HoldAppend => CompiledCommand::HoldAppend,
        Command::GetReplace => CompiledCommand::GetReplace,
        Command::GetAppend => CompiledCommand::GetAppend,
        Command::Exchange => CompiledCommand::Exchange,
        Command::Append(text) => CompiledCommand::Append(text),
        Command::Insert(text) => CompiledCommand::Insert(text),
        Command::Change(text) => CompiledCommand::Change(text),
        Command::Label(_) => CompiledCommand::Label,
        Command::Branch(label) => CompiledCommand::Branch(label),
        Command::BranchIfSub(label) => CompiledCommand::BranchIfSub(label),
        Command::BranchIfNotSub(label) => CompiledCommand::BranchIfNotSub(label),
        Command::ReadFile(path) => CompiledCommand::ReadFile(path),
        Command::ReadLine(path) => CompiledCommand::ReadLine(path),
        Command::WriteFile(path) => CompiledCommand::WriteFile(path),
        Command::WriteFirstLine(path) => CompiledCommand::WriteFirstLine(path),
        Command::PrintFileName => CompiledCommand::PrintFileName,
        Command::Execute(command) => CompiledCommand::Execute(command),
        Command::Quit(code) => CompiledCommand::Quit(code.unwrap_or(0)),
        Command::QuitNoprint(code) => CompiledCommand::QuitNoprint(code.unwrap_or(0)),
        Command::ClearPattern => CompiledCommand::ClearPattern,
        Command::Noop => CompiledCommand::Noop,
        Command::Block(_) => unreachable!("blocks are compiled recursively"),
    })
}

fn open_write_files(commands: &[CompiledSedCommand]) -> Result<HashMap<Vec<u8>, fs::File>> {
    let mut paths = HashSet::new();
    for command in commands {
        match &command.command {
            CompiledCommand::Substitute(sub) => {
                if let Some(path) = &sub.write_file {
                    paths.insert(path.clone());
                }
            }
            CompiledCommand::WriteFile(path) | CompiledCommand::WriteFirstLine(path) => {
                paths.insert(path.clone());
            }
            _ => {}
        }
    }

    let mut files = HashMap::with_capacity(paths.len());
    for path in paths {
        let special = path == b"/dev/stdout" || path == b"/dev/stderr";
        let mut options = fs::OpenOptions::new();
        options.write(true);
        if !special {
            options.create(true).truncate(true);
        }
        files.insert(path.clone(), options.open(path_from_bytes(&path)?)?);
    }
    Ok(files)
}

fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(bytes.to_vec())
            .map(PathBuf::from)
            .map_err(|error| Error::InvalidPath(PathBuf::from(error.to_string())))
    }
}

fn os_string_from_bytes(bytes: &[u8]) -> Result<OsString> {
    #[cfg(unix)]
    {
        Ok(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(bytes.to_vec())
            .map(OsString::from)
            .map_err(|error| Error::InvalidPath(PathBuf::from(error.to_string())))
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
