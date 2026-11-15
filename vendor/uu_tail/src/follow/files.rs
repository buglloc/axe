// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use crate::args::{FollowMode, Settings};
use crate::paths::MetadataExtTail;
use std::fs::{File, Metadata};
use std::io::{self, BufRead, BufReader, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uucore::display::Quotable;
use uucore::{show_error, translate};

pub struct FileHandling {
    pub entries: Vec<PathData>,
    last: Option<usize>,
    verbose: bool,
}

impl FileHandling {
    pub fn from(settings: &Settings) -> Self {
        Self {
            entries: Vec::with_capacity(settings.inputs.len()),
            last: None,
            verbose: settings.verbose,
        }
    }

    pub fn insert(&mut self, data: PathData, update_last: bool) {
        let index = if let Some(index) = self.entries.iter().position(|p| p.path == data.path) {
            self.entries[index] = data;
            index
        } else {
            self.entries.push(data);
            self.entries.len() - 1
        };
        if update_last {
            self.last = Some(index);
        }
    }

    pub fn has_files(&self) -> bool {
        self.entries.iter().any(|p| !p.ignored)
    }

    pub fn has_readers(&self) -> bool {
        self.entries.iter().any(|p| p.reader.is_some())
    }

    /// Each pass copies a finite snapshot, so a busy producer cannot starve --pid or other files.
    pub fn tail_file(&mut self, index: usize, writer: &mut impl Write) -> io::Result<bool> {
        let data = &mut self.entries[index];
        let Some(reader) = data.reader.as_mut() else {
            return Ok(false);
        };
        let metadata = reader.get_ref().metadata()?;
        let regular = metadata.is_file();
        if regular && metadata.len() < data.position {
            show_error!(
                "{}",
                translate!("tail-status-file-truncated", "file" => data.display_name)
            );
            reader.seek(SeekFrom::Start(0))?;
            data.position = 0;
        }
        data.metadata = Some(metadata);
        let mut remaining = if regular {
            data.metadata
                .as_ref()
                .map_or(0, |m| m.len().saturating_sub(data.position))
        } else {
            u64::MAX
        }
        .min(1024 * 1024);
        let had_data = remaining != 0;
        let mut printed = false;
        while remaining != 0 {
            let buffer = match reader.fill_buf() {
                Ok(buffer) => buffer,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            };
            if buffer.is_empty() {
                break;
            }
            if !printed && self.verbose && self.last != Some(index) {
                writeln!(writer, "\n==> {} <==", data.display_name)?;
            }
            let count = (buffer.len() as u64).min(remaining) as usize;
            writer.write_all(&buffer[..count])?;
            reader.consume(count);
            data.position = data.position.saturating_add(count as u64);
            remaining -= count as u64;
            printed = true;
        }
        if printed {
            writer.flush()?;
            self.last = Some(index);
        }
        Ok(had_data && remaining == 0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathState {
    Accessible,
    Inaccessible(Option<ErrorKind>),
    Untailable,
}

pub struct PathData {
    pub path: PathBuf,
    pub display_name: String,
    pub reader: Option<BufReader<File>>,
    /// Metadata and position belong to the open descriptor, never to a replacement pathname.
    pub metadata: Option<Metadata>,
    position: u64,
    path_state: PathState,
    pub ignored: bool,
    pub stdin: bool,
}

impl PathData {
    pub fn new(
        path: PathBuf,
        display_name: &str,
        mut reader: Option<BufReader<File>>,
        stdin: bool,
    ) -> io::Result<Self> {
        let metadata = reader
            .as_ref()
            .map(|r| r.get_ref().metadata())
            .transpose()?;
        let position = if metadata.as_ref().is_some_and(Metadata::is_file) {
            reader.as_mut().map_or(Ok(0), Seek::stream_position)?
        } else {
            0
        };
        #[cfg(unix)]
        if let Some(reader) = reader.as_ref()
            && metadata.as_ref().is_some_and(|m| !m.is_file())
        {
            use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
            let flags = fcntl_getfl(reader.get_ref())?;
            fcntl_setfl(reader.get_ref(), flags | OFlags::NONBLOCK)?;
        }
        let path_state = if reader.is_some() {
            PathState::Accessible
        } else {
            PathState::Inaccessible(None)
        };
        Ok(Self {
            path,
            display_name: display_name.to_owned(),
            reader,
            metadata,
            position,
            path_state,
            ignored: false,
            stdin,
        })
    }

    /// Resolve the original spelling on every name check, including symlinks and mount overlays.
    /// Returns true when the file notification watch must be refreshed.
    pub fn reconcile(&mut self, settings: &Settings) -> bool {
        if self.ignored
            || self.stdin
            || (settings.follow == Some(FollowMode::Descriptor) && self.reader.is_some())
        {
            return false;
        }
        match self.path.metadata() {
            Ok(metadata) if metadata.is_tailable() => {
                if self
                    .metadata
                    .as_ref()
                    .is_some_and(|old| old.file_id_eq(&metadata))
                    && self.reader.is_some()
                {
                    self.path_state = PathState::Accessible;
                    return false;
                }
                // O_NONBLOCK also makes a raced-in FIFO safe to open without a writer.
                match open_followed(&self.path).and_then(|file| {
                    let metadata = file.metadata()?;
                    if !metadata.is_tailable() {
                        return Err(io::Error::from(ErrorKind::InvalidInput));
                    }
                    Ok((file, metadata))
                }) {
                    Ok((file, metadata)) => {
                        let message = match self.path_state {
                            PathState::Accessible if self.reader.is_some() => {
                                translate!("tail-status-has-been-replaced-following-new-file", "file" => self.display_name.quote())
                            }
                            PathState::Untailable => {
                                translate!("tail-status-has-become-accessible", "file" => self.display_name.quote())
                            }
                            _ => {
                                translate!("tail-status-has-appeared-following-new-file", "file" => self.display_name.quote())
                            }
                        };
                        show_error!("{message}");
                        self.reader = Some(BufReader::new(file));
                        self.metadata = Some(metadata);
                        self.position = 0;
                        self.path_state = PathState::Accessible;
                        return true;
                    }
                    Err(error) => self.inaccessible(error, settings),
                }
            }
            Ok(_) => {
                if self.path_state != PathState::Untailable {
                    let message = if settings.retry {
                        translate!("tail-status-replaced-with-untailable-file", "file" => self.display_name.quote())
                    } else {
                        translate!("tail-status-replaced-with-untailable-file-giving-up", "file" => self.display_name.quote())
                    };
                    show_error!("{message}");
                }
                self.path_state = PathState::Untailable;
                self.reader = None;
                self.metadata = None;
                self.ignored = !settings.retry;
            }
            Err(error) => self.inaccessible(error, settings),
        }
        false
    }

    fn inaccessible(&mut self, error: io::Error, settings: &Settings) {
        if self.path_state == PathState::Accessible {
            show_error!(
                "{} {}: {}",
                self.display_name.quote(),
                translate!("tail-become-inaccessible"),
                uucore::error::strip_errno(&error)
            );
        } else if let PathState::Inaccessible(Some(previous)) = self.path_state
            && previous != error.kind()
        {
            show_error!(
                "{}",
                translate!("tail-error-cannot-open-no-such-file",
                    "file" => self.display_name,
                    "error" => uucore::error::strip_errno(&error))
            );
        }
        self.path_state = PathState::Inaccessible(Some(error.kind()));
        self.reader = None;
        self.metadata = None;
        self.ignored = !settings.retry;
    }
}

fn open_followed(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}
