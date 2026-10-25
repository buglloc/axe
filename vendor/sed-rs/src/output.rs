use std::io::{self, Write};

use crate::input::Record;

pub(crate) struct Output<W> {
    writer: W,
    delimiter: u8,
    unterminated: bool,
}

impl<W: Write> Output<W> {
    pub(crate) fn new(writer: W, delimiter: u8) -> Self {
        Self {
            writer,
            delimiter,
            unterminated: false,
        }
    }

    fn separate(&mut self) -> io::Result<()> {
        if self.unterminated {
            self.writer.write_all(&[self.delimiter])?;
            self.unterminated = false;
        }
        Ok(())
    }

    pub(crate) fn record(&mut self, record: &Record) -> io::Result<()> {
        self.bytes(&record.data, record.terminated)
    }

    pub(crate) fn bytes(&mut self, data: &[u8], terminated: bool) -> io::Result<()> {
        self.separate()?;
        self.writer.write_all(data)?;
        if terminated {
            self.writer.write_all(&[self.delimiter])?;
        }
        self.unterminated = !terminated;
        Ok(())
    }

    pub(crate) fn raw(&mut self, data: &[u8]) -> io::Result<()> {
        if !data.is_empty() {
            self.separate()?;
            self.writer.write_all(data)?;
            self.unterminated = data.last() != Some(&self.delimiter);
        }
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
