// Copyright 2017 Google Inc.
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::cell::RefCell;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::{stderr, Write};
use std::path::Path;
use std::process::Command;

use super::{Matcher, MatcherIO, WalkEntry};

enum Arg {
    FileArg(Vec<OsString>),
    LiteralArg(OsString),
}

fn parse_arg(s: &OsStr) -> Arg {
    let source = s.as_encoded_bytes();
    let mut rest = source;
    let mut parts = Vec::new();
    while let Some(index) = rest.windows(2).position(|window| window == b"{}") {
        // SAFETY: ASCII markers cannot split an encoded non-ASCII code point.
        parts.push(unsafe { OsStr::from_encoded_bytes_unchecked(&rest[..index]) }.to_owned());
        rest = &rest[index + 2..];
    }
    if parts.is_empty() {
        Arg::LiteralArg(s.to_owned())
    } else {
        // SAFETY: rest begins immediately after the ASCII marker.
        parts.push(unsafe { OsStr::from_encoded_bytes_unchecked(rest) }.to_owned());
        Arg::FileArg(parts)
    }
}

pub struct SingleExecMatcher {
    executable: Arg,
    args: Vec<Arg>,
    exec_in_parent_dir: bool,
    interactive: bool,
}

impl SingleExecMatcher {
    pub fn new<S: AsRef<OsStr>>(
        executable: impl AsRef<OsStr>,
        args: &[S],
        exec_in_parent_dir: bool,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self::new_impl(
            executable.as_ref(),
            args,
            exec_in_parent_dir,
            false,
        ))
    }

    pub fn new_interactive<S: AsRef<OsStr>>(
        executable: impl AsRef<OsStr>,
        args: &[S],
        exec_in_parent_dir: bool,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self::new_impl(
            executable.as_ref(),
            args,
            exec_in_parent_dir,
            true,
        ))
    }

    fn new_impl<S: AsRef<OsStr>>(
        executable: &OsStr,
        args: &[S],
        exec_in_parent_dir: bool,
        interactive: bool,
    ) -> Self {
        let transformed_args = args.iter().map(|a| parse_arg(a.as_ref())).collect();

        Self {
            executable: parse_arg(executable),
            args: transformed_args,
            exec_in_parent_dir,
            interactive,
        }
    }
}

impl Matcher for SingleExecMatcher {
    fn matches(&self, file_info: &WalkEntry, matcher_io: &mut MatcherIO) -> bool {
        let path_to_file = if self.exec_in_parent_dir {
            if let Some(f) = file_info.path().file_name() {
                Path::new(".").join(f)
            } else {
                Path::new(".").join(file_info.path())
            }
        } else {
            file_info.path().to_path_buf()
        };

        let resolved_executable = match self.executable {
            Arg::LiteralArg(ref a) => a.clone(),
            Arg::FileArg(ref parts) => parts.join(path_to_file.as_os_str()),
        };

        if self.interactive {
            // GNU find prints a fixed, abbreviated prompt of the form
            // "< executable ... pathname > ? ".  It does not render the
            // substituted argument list, and always shows the full path of
            // the entry being processed (even for -okdir, whose command runs
            // with the "./basename" form).
            let prompt = format!(
                "< {} ... {} > ? ",
                resolved_executable.to_string_lossy(),
                file_info.path().to_string_lossy()
            );

            if !matcher_io.confirm(&prompt) {
                return false;
            }
        }

        let mut command = Command::new(&resolved_executable);

        for arg in &self.args {
            match *arg {
                Arg::LiteralArg(ref a) => command.arg(a.as_os_str()),
                Arg::FileArg(ref parts) => command.arg(parts.join(path_to_file.as_os_str())),
            };
        }
        if self.exec_in_parent_dir {
            match file_info.path().parent() {
                None => {
                    // Root paths like "/" have no parent.  Run them from the root to match GNU find.
                    command.current_dir(file_info.path());
                }
                Some(parent) if parent == Path::new("") => {
                    // Paths like "foo" have a parent of "".  Avoid chdir("").
                }
                Some(parent) => {
                    command.current_dir(parent);
                }
            }
        }
        match command.status() {
            Ok(status) => status.success(),
            Err(e) => {
                writeln!(
                    &mut stderr(),
                    "Failed to run {}: {}",
                    resolved_executable.to_string_lossy(),
                    e
                )
                .unwrap();
                false
            }
        }
    }

    fn has_side_effects(&self) -> bool {
        true
    }
}

pub struct MultiExecMatcher {
    executable: OsString,
    args: Vec<OsString>,
    exec_in_parent_dir: bool,
    /// Command to build while matching.
    command: RefCell<Option<argmax::Command>>,
}

impl MultiExecMatcher {
    pub fn new<S: AsRef<OsStr>>(
        executable: impl AsRef<OsStr>,
        args: &[S],
        exec_in_parent_dir: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let transformed_args = args.iter().map(|arg| arg.as_ref().to_os_string()).collect();

        Ok(Self {
            executable: executable.as_ref().to_os_string(),
            args: transformed_args,
            exec_in_parent_dir,
            command: RefCell::new(None),
        })
    }

    fn new_command(&self) -> argmax::Command {
        let mut command = argmax::Command::new(&self.executable);
        command.try_args(&self.args).unwrap();
        command
    }

    fn run_command(&self, command: &mut argmax::Command, matcher_io: &mut MatcherIO) {
        match command.status() {
            Ok(status) => {
                if !status.success() {
                    matcher_io.set_exit_code(1);
                }
            }
            Err(e) => {
                writeln!(
                    &mut stderr(),
                    "Failed to run {}: {}",
                    self.executable.to_string_lossy(),
                    e
                )
                .unwrap();
                matcher_io.set_exit_code(1);
            }
        }
    }
}

impl Matcher for MultiExecMatcher {
    fn matches(&self, file_info: &WalkEntry, matcher_io: &mut MatcherIO) -> bool {
        let path_to_file = if self.exec_in_parent_dir {
            if let Some(f) = file_info.path().file_name() {
                Path::new(".").join(f)
            } else {
                Path::new(".").join(file_info.path())
            }
        } else {
            file_info.path().to_path_buf()
        };
        let mut command = self.command.borrow_mut();
        let command = command.get_or_insert_with(|| self.new_command());

        // Build command, or dispatch it before when it is long enough.
        if command.try_arg(&path_to_file).is_err() {
            if self.exec_in_parent_dir {
                match file_info.path().parent() {
                    None => {
                        // Root paths like "/" have no parent.  Run them from the root to match GNU find.
                        command.current_dir(file_info.path());
                    }
                    Some(parent) if parent == Path::new("") => {
                        // Paths like "foo" have a parent of "".  Avoid chdir("").
                    }
                    Some(parent) => {
                        command.current_dir(parent);
                    }
                }
            }
            self.run_command(command, matcher_io);

            // Reset command status.
            *command = self.new_command();
            if let Err(e) = command.try_arg(&path_to_file) {
                writeln!(
                    &mut stderr(),
                    "Cannot fit a single argument {}: {}",
                    path_to_file.to_string_lossy(),
                    e
                )
                .unwrap();
                matcher_io.set_exit_code(1);
            }
        }
        true
    }

    fn finished_dir(&self, dir: &Path, matcher_io: &mut MatcherIO) {
        // Dispatch command for -execdir.
        if self.exec_in_parent_dir {
            let mut command = self.command.borrow_mut();
            if let Some(mut command) = command.take() {
                command.current_dir(Path::new(".").join(dir));
                self.run_command(&mut command, matcher_io);
            }
        }
    }

    fn finished(&self, matcher_io: &mut MatcherIO) {
        // Dispatch command for -exec.
        if !self.exec_in_parent_dir {
            let mut command = self.command.borrow_mut();
            if let Some(mut command) = command.take() {
                self.run_command(&mut command, matcher_io);
            }
        }
    }

    fn has_side_effects(&self) -> bool {
        true
    }
}

#[cfg(test)]
/// No tests here, because we need to call out to an external executable. See
/// `tests/exec_unit_tests.rs` instead.
mod tests {}
