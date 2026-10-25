use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Default)]
pub(crate) struct Record {
    pub(crate) data: Vec<u8>,
    pub(crate) terminated: bool,
}

pub(crate) struct InputRecord {
    pub(crate) record: Record,
    pub(crate) source: usize,
}

pub(crate) trait Input {
    fn next(&mut self) -> io::Result<Option<InputRecord>>;
    fn filename(&self, source: usize) -> &OsStr;
    fn status(&self) -> i32 {
        0
    }
}

pub(crate) fn read_record<R: BufRead>(reader: &mut R, delimiter: u8) -> io::Result<Option<Record>> {
    let mut data = Vec::new();
    if reader.read_until(delimiter, &mut data)? == 0 {
        return Ok(None);
    }
    let terminated = data.last() == Some(&delimiter);
    if terminated {
        data.pop();
    }
    Ok(Some(Record { data, terminated }))
}

pub(crate) struct ReaderInput<'a, R> {
    reader: R,
    filename: &'a OsStr,
    delimiter: u8,
}

impl<'a, R: BufRead> ReaderInput<'a, R> {
    pub(crate) fn new(reader: R, filename: &'a OsStr, delimiter: u8) -> Self {
        Self {
            reader,
            filename,
            delimiter,
        }
    }
}

impl<R: BufRead> Input for ReaderInput<'_, R> {
    fn next(&mut self) -> io::Result<Option<InputRecord>> {
        Ok(read_record(&mut self.reader, self.delimiter)?
            .map(|record| InputRecord { record, source: 0 }))
    }

    fn filename(&self, _: usize) -> &OsStr {
        self.filename
    }
}

pub(crate) struct FilesInput<'a> {
    paths: &'a [PathBuf],
    next_path: usize,
    active: Option<usize>,
    file: Option<BufReader<File>>,
    stdin: BufReader<io::Stdin>,
    delimiter: u8,
    failed: bool,
}

impl<'a> FilesInput<'a> {
    pub(crate) fn new(paths: &'a [PathBuf], delimiter: u8) -> Self {
        Self {
            paths,
            next_path: 0,
            active: None,
            file: None,
            stdin: BufReader::new(io::stdin()),
            delimiter,
            failed: false,
        }
    }
}

impl Input for FilesInput<'_> {
    fn next(&mut self) -> io::Result<Option<InputRecord>> {
        loop {
            if let Some(source) = self.active {
                let record = match &mut self.file {
                    Some(file) => read_record(file, self.delimiter)?,
                    None => read_record(&mut self.stdin, self.delimiter)?,
                };
                if let Some(record) = record {
                    return Ok(Some(InputRecord { record, source }));
                }
                self.active = None;
                self.file = None;
            }
            let Some(path) = self.paths.get(self.next_path) else {
                return Ok(None);
            };
            let source = self.next_path;
            self.next_path += 1;
            if path == Path::new("-") {
                self.active = Some(source);
                continue;
            }
            match fs::File::open(path) {
                Ok(file) => {
                    self.file = Some(BufReader::new(file));
                    self.active = Some(source);
                }
                Err(error) => {
                    report_path("can't read", path, &error);
                    self.failed = true;
                }
            }
        }
    }

    fn filename(&self, source: usize) -> &OsStr {
        self.paths[source].as_os_str()
    }
    fn status(&self) -> i32 {
        if self.failed { 2 } else { 0 }
    }
}

pub(crate) fn report_path(operation: &str, path: &Path, error: &io::Error) {
    let mut message = error.to_string();
    if let Some(code) = error.raw_os_error() {
        let suffix = format!(" (os error {code})");
        if message.ends_with(&suffix) {
            message.truncate(message.len() - suffix.len());
        }
    }
    let mut stderr = io::stderr().lock();
    let _ = write!(stderr, "sed: {operation} ");
    let _ = stderr.write_all(path.as_os_str().as_encoded_bytes());
    let _ = writeln!(stderr, ": {message}");
}

/// `$` peeks at the next record; `q` does not read input it will never use.
pub(crate) struct LineReader<I> {
    input: I,
    pending: Option<InputRecord>,
    peeked: bool,
}

impl<I: Input> LineReader<I> {
    pub(crate) fn new(input: I) -> Self {
        Self {
            input,
            pending: None,
            peeked: false,
        }
    }

    pub(crate) fn read_line(&mut self) -> io::Result<Option<InputRecord>> {
        if self.peeked {
            self.peeked = false;
            Ok(self.pending.take())
        } else {
            self.input.next()
        }
    }

    pub(crate) fn is_last(&mut self) -> io::Result<bool> {
        if !self.peeked {
            self.pending = self.input.next()?;
            self.peeked = true;
        }
        Ok(self.pending.is_none())
    }

    pub(crate) fn status(&self) -> i32 {
        self.input.status()
    }
    pub(crate) fn filename(&self, source: usize) -> &OsStr {
        self.input.filename(source)
    }
}
