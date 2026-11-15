// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore tailable stdlib (stdlib)

#[cfg(not(unix))]
use crate::text;
use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io::{Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::PathBuf;
use uucore::translate;

#[derive(Debug, Clone)]
pub enum InputKind {
    File(PathBuf),
    Stdin,
}

#[cfg(unix)]
impl From<&OsStr> for InputKind {
    fn from(value: &OsStr) -> Self {
        if value == OsStr::new("-") {
            Self::Stdin
        } else {
            Self::File(PathBuf::from(value))
        }
    }
}

#[cfg(not(unix))]
impl From<&OsStr> for InputKind {
    fn from(value: &OsStr) -> Self {
        if value == OsStr::new(text::DASH) {
            Self::Stdin
        } else {
            Self::File(PathBuf::from(value))
        }
    }
}

#[derive(Debug, Clone)]
pub struct Input {
    kind: InputKind,
    pub display_name: String,
}

impl Input {
    pub fn from<T: AsRef<OsStr>>(string: T) -> Self {
        let string = string.as_ref();

        let kind = string.into();
        let display_name = match kind {
            InputKind::File(_) => string.to_string_lossy().to_string(),
            InputKind::Stdin => translate!("tail-stdin-header"),
        };

        Self { kind, display_name }
    }

    pub fn kind(&self) -> &InputKind {
        &self.kind
    }

    pub fn is_stdin(&self) -> bool {
        match self.kind {
            InputKind::File(_) => false,
            InputKind::Stdin => true,
        }
    }
}

impl Default for Input {
    fn default() -> Self {
        Self {
            kind: InputKind::Stdin,
            display_name: translate!("tail-stdin-header"),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HeaderPrinter {
    verbose: bool,
    first_header: bool,
}

impl HeaderPrinter {
    pub fn new(verbose: bool, first_header: bool) -> Self {
        Self {
            verbose,
            first_header,
        }
    }

    pub fn print_input(&mut self, input: &Input) {
        self.print(input.display_name.as_str());
    }

    pub fn print(&mut self, string: &str) {
        if self.verbose {
            println!(
                "{}==> {string} <==",
                if self.first_header { "" } else { "\n" },
            );
            self.first_header = false;
        }
    }
}
pub trait FileExtTail {
    #[allow(clippy::wrong_self_convention)]
    fn is_seekable(&mut self, current_offset: u64) -> bool;
}

impl FileExtTail for File {
    /// Test if File is seekable.
    /// Set the current position offset to `current_offset`.
    fn is_seekable(&mut self, current_offset: u64) -> bool {
        self.stream_position().is_ok()
            && self.seek(SeekFrom::End(0)).is_ok()
            && self.seek(SeekFrom::Start(current_offset)).is_ok()
    }
}

pub trait MetadataExtTail {
    fn is_tailable(&self) -> bool;
    #[cfg(not(target_os = "wasi"))]
    fn file_id_eq(&self, other: &Metadata) -> bool;
}

impl MetadataExtTail for Metadata {
    fn is_tailable(&self) -> bool {
        let ft = self.file_type();
        #[cfg(unix)]
        {
            ft.is_file() || ft.is_char_device() || ft.is_fifo()
        }
        #[cfg(not(unix))]
        {
            ft.is_file()
        }
    }

    #[cfg(not(target_os = "wasi"))]
    fn file_id_eq(&self, #[cfg(unix)] other: &Metadata, #[cfg(not(unix))] _: &Metadata) -> bool {
        #[cfg(unix)]
        {
            self.dev() == other.dev() && self.ino() == other.ino()
        }
        #[cfg(windows)]
        {
            // TODO: `file_index` requires unstable library feature `windows_by_handle`
            // use std::os::windows::prelude::*;
            // if let Some(self_id) = self.file_index() {
            //     if let Some(other_id) = other.file_index() {
            //     // TODO: not sure this is the equivalent of comparing inode numbers
            //
            //         return self_id.eq(&other_id);
            //     }
            // }
            false
        }
    }
}

#[inline]
#[cfg(all(unix, not(target_os = "fuchsia")))]
pub fn stdin_is_bad_fd() -> bool {
    uucore::signals::stdin_was_closed()
}

#[inline]
#[cfg(not(all(unix, not(target_os = "fuchsia"))))]
pub fn stdin_is_bad_fd() -> bool {
    false
}
