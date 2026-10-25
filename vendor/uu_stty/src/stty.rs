// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore clocal erange tcgetattr tcsetattr tcsanow tiocgwinsz tiocswinsz cfgetospeed cfsetospeed ushort vmin vtime cflag lflag ispeed ospeed
// spell-checker:ignore parenb parodd cmspar hupcl cstopb cread clocal crtscts CSIZE
// spell-checker:ignore ignbrk brkint ignpar parmrk inpck istrip inlcr igncr icrnl ixoff ixon iuclc ixany imaxbel iutf
// spell-checker:ignore opost olcuc ocrnl onlcr onocr onlret ofdel nldly crdly tabdly bsdly vtdly ffdly ofill
// spell-checker:ignore isig icanon iexten echoe crterase echok echonl noflsh xcase tostop echoprt prterase echoctl ctlecho echoke crtkill flusho extproc
// spell-checker:ignore lnext rprnt susp swtch vdiscard veof veol verase vintr vkill vlnext vquit vreprint vstart vstop vsusp vswtc vwerase werase
// spell-checker:ignore sigquit sigtstp
// spell-checker:ignore cbreak decctlq evenp litout oddp tcsadrain exta extb NCCS cfsetispeed
// spell-checker:ignore notaflag notacombo notabaud
// spell-checker:ignore baudrate TCGETS

#![cfg(unix)]

mod flags;

use crate::flags::AllFlags;
use crate::flags::COMBINATION_SETTINGS;
use clap::{Arg, ArgAction, ArgMatches, Command};
use nix::libc::{O_NONBLOCK, TIOCGWINSZ, TIOCSWINSZ, c_ushort};

#[cfg(linux_termios2)]
use nix::libc::{TCGETS2, TCSETS2, TCSETSF2, TCSETSW2, termios2};

use nix::sys::termios::{
    ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg, SpecialCharacterIndices as S,
    Termios,
};
use nix::{ioctl_read_bad, ioctl_write_ptr_bad};
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, Stdin, stdin, stdout};
use std::num::IntErrorKind;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use uucore::error::{FromIo, UError, UResult, USimpleError, UUsageError};
use uucore::format_usage;
use uucore::parser::num_parser::ExtendedParser;
use uucore::translate;

#[cfg(not(bsd))]
use flags::BAUD_RATES;
use flags::{CONTROL_CHARS, CONTROL_FLAGS, INPUT_FLAGS, LOCAL_FLAGS, OUTPUT_FLAGS};

const ASCII_DEL: u8 = 127;

// Sane defaults for control characters.
const SANE_CONTROL_CHARS: [(S, u8); 12] = [
    (S::VINTR, 3),     // ^C
    (S::VQUIT, 28),    // ^\
    (S::VERASE, 127),  // DEL
    (S::VKILL, 21),    // ^U
    (S::VEOF, 4),      // ^D
    (S::VSTART, 17),   // ^Q
    (S::VSTOP, 19),    // ^S
    (S::VSUSP, 26),    // ^Z
    (S::VREPRINT, 18), // ^R
    (S::VWERASE, 23),  // ^W
    (S::VLNEXT, 22),   // ^V
    (S::VDISCARD, 15), // ^O
];

#[derive(Clone, Copy, Debug)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Flag<T> {
    name: &'static str,
    #[expect(clippy::struct_field_names)]
    flag: T,
    show: bool,
    sane: bool,
    group: Option<T>,
}

impl<T> Flag<T> {
    pub const fn new(name: &'static str, flag: T) -> Self {
        Self {
            name,
            flag,
            show: true,
            sane: false,
            group: None,
        }
    }

    pub const fn new_grouped(name: &'static str, flag: T, group: T) -> Self {
        Self {
            name,
            flag,
            show: true,
            sane: false,
            group: Some(group),
        }
    }

    pub const fn hidden(mut self) -> Self {
        self.show = false;
        self
    }

    pub const fn sane(mut self) -> Self {
        self.sane = true;
        self
    }
}

trait TermiosFlag: Copy {
    fn is_in(&self, termios: &Termios, group: Option<Self>) -> bool;
    fn apply(&self, termios: &mut Termios, val: bool);
}

mod options {
    pub const ALL: &str = "all";
    pub const SAVE: &str = "save";
    pub const FILE: &str = "file";
    pub const SETTINGS: &str = "settings";
}

struct Options<'a> {
    all: bool,
    save: bool,
    file: Device,
    device_name: String,
    settings: Option<Vec<&'a str>>,
}

enum Device {
    File(File),
    Stdin(Stdin),
}

struct TerminalSettings {
    termios: Termios,
    #[cfg(linux_termios2)]
    input_speed: u32,
    #[cfg(linux_termios2)]
    output_speed: u32,
}

#[cfg(linux_termios2)]
impl TerminalSettings {
    fn input_speed(&self) -> u32 {
        if self.input_speed == 0 {
            self.output_speed
        } else {
            self.input_speed
        }
    }

    fn update_speed_flags(&mut self) {
        let flags = self.termios.control_flags.bits() & !(nix::libc::CBAUD | nix::libc::CIBAUD);
        self.termios.control_flags = ControlFlags::from_bits_retain(
            flags
                | speed_to_baud(self.output_speed)
                | (speed_to_baud(self.input_speed) << nix::libc::IBSHIFT),
        );
    }
}

#[derive(Debug)]
enum ControlCharMappingError {
    IntOutOfRange(String),
    MultipleChars(String),
}

enum SpecialSetting {
    Rows(u16),
    Cols(u16),
    Line(u8),
}

enum PrintSetting {
    Size,
    Speed,
}

enum ArgOptions<'a> {
    Flags(AllFlags<'a>),
    Mapping((S, u8)),
    Special(SpecialSetting),
    Print(PrintSetting),
    SavedState(Vec<u32>),
}

impl<'a> From<AllFlags<'a>> for ArgOptions<'a> {
    fn from(flag: AllFlags<'a>) -> Self {
        ArgOptions::Flags(flag)
    }
}

impl AsFd for Device {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::File(f) => f.as_fd(),
            Self::Stdin(stdin) => stdin.as_fd(),
        }
    }
}

impl AsRawFd for Device {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Self::File(f) => f.as_raw_fd(),
            Self::Stdin(stdin) => stdin.as_raw_fd(),
        }
    }
}

impl<'a> Options<'a> {
    fn from(matches: &'a ArgMatches) -> io::Result<Self> {
        let (file, device_name) = match matches.get_one::<String>(options::FILE) {
            // Two notes here:
            // 1. O_NONBLOCK is needed because according to GNU docs, a
            //    POSIX tty can block waiting for carrier-detect if the
            //    "clocal" flag is not set. If your TTY is not connected
            //    to a modem, it is probably not relevant though.
            // 2. We never close the FD that we open here, but the OS
            //    will clean up the FD for us on exit, so it doesn't
            //    matter. The alternative would be to have an enum of
            //    BorrowedFd/OwnedFd to handle both cases.
            Some(f) => (
                Device::File(
                    std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(O_NONBLOCK)
                        .open(f)?,
                ),
                f.clone(),
            ),
            // Per POSIX, stdin is used for TTY operations when no device is specified.
            // This matches GNU coreutils behavior: if stdin is not a TTY,
            // tcgetattr will fail with "Inappropriate ioctl for device".
            None => (Device::Stdin(stdin()), "standard input".to_string()),
        };
        Ok(Self {
            all: matches.get_flag(options::ALL),
            save: matches.get_flag(options::SAVE),
            file,
            device_name,
            settings: matches
                .get_many::<String>(options::SETTINGS)
                .map(|v| v.map(AsRef::as_ref).collect()),
        })
    }
}

// Needs to be repr(C) because we pass it to the ioctl calls.
#[repr(C)]
#[derive(Default, Debug)]
pub struct TermSize {
    rows: c_ushort,
    columns: c_ushort,
    x: c_ushort,
    y: c_ushort,
}

ioctl_read_bad!(
    /// Get terminal window size
    tiocgwinsz,
    TIOCGWINSZ,
    TermSize
);

ioctl_write_ptr_bad!(
    /// Set terminal window size
    tiocswinsz,
    TIOCSWINSZ,
    TermSize
);

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let matches = uucore::clap_localization::handle_clap_result(uu_app(), args)?;

    let opts = Options::from(&matches)?;

    stty(&opts)
}

fn stty(opts: &Options) -> UResult<()> {
    if opts.save && opts.all {
        return Err(USimpleError::new(
            1,
            translate!("stty-error-options-mutually-exclusive"),
        ));
    }

    if opts.settings.is_some() && (opts.save || opts.all) {
        return Err(USimpleError::new(
            1,
            translate!("stty-error-output-style-no-modes"),
        ));
    }

    let mut set_arg = SetArg::TCSADRAIN;
    let mut valid_args: Vec<ArgOptions> = Vec::new();

    if let Some(args) = &opts.settings {
        let mut args_iter = args.iter();
        while let Some(&arg) = args_iter.next() {
            match arg {
                "ispeed" => match args_iter.next() {
                    Some(speed) => {
                        if let Some(baud_flag) = string_to_baud(speed, flags::BaudType::Input) {
                            valid_args.push(ArgOptions::Flags(baud_flag));
                        } else {
                            return invalid_speed(arg, speed);
                        }
                    }
                    None => {
                        return missing_arg(arg);
                    }
                },
                "ospeed" => match args_iter.next() {
                    Some(speed) => {
                        if let Some(baud_flag) = string_to_baud(speed, flags::BaudType::Output) {
                            valid_args.push(ArgOptions::Flags(baud_flag));
                        } else {
                            return invalid_speed(arg, speed);
                        }
                    }
                    None => {
                        return missing_arg(arg);
                    }
                },
                "line" => match args_iter.next() {
                    Some(line) => match parse_u8_or_err(line) {
                        Ok(n) => valid_args.push(ArgOptions::Special(SpecialSetting::Line(n))),
                        Err(e) => return Err(USimpleError::new(1, e)),
                    },
                    None => {
                        return missing_arg(arg);
                    }
                },
                "min" => match args_iter.next() {
                    Some(min) => match parse_u8_or_err(min) {
                        Ok(n) => {
                            valid_args.push(ArgOptions::Mapping((S::VMIN, n)));
                        }
                        Err(e) => return Err(USimpleError::new(1, e)),
                    },
                    None => {
                        return missing_arg(arg);
                    }
                },
                "time" => match args_iter.next() {
                    Some(time) => match parse_u8_or_err(time) {
                        Ok(n) => valid_args.push(ArgOptions::Mapping((S::VTIME, n))),
                        Err(e) => return Err(USimpleError::new(1, e)),
                    },
                    None => {
                        return missing_arg(arg);
                    }
                },
                "rows" => {
                    if let Some(rows) = args_iter.next() {
                        if let Some(n) = parse_rows_cols(rows) {
                            valid_args.push(ArgOptions::Special(SpecialSetting::Rows(n)));
                        } else {
                            return invalid_integer_arg(rows);
                        }
                    } else {
                        return missing_arg(arg);
                    }
                }
                "columns" | "cols" => {
                    if let Some(cols) = args_iter.next() {
                        if let Some(n) = parse_rows_cols(cols) {
                            valid_args.push(ArgOptions::Special(SpecialSetting::Cols(n)));
                        } else {
                            return invalid_integer_arg(cols);
                        }
                    } else {
                        return missing_arg(arg);
                    }
                }
                "drain" => {
                    set_arg = SetArg::TCSADRAIN;
                }
                "-drain" => {
                    set_arg = SetArg::TCSANOW;
                }
                "size" => {
                    valid_args.push(ArgOptions::Print(PrintSetting::Size));
                }
                "speed" => {
                    valid_args.push(ArgOptions::Print(PrintSetting::Speed));
                }
                _ => {
                    // Try to parse saved format (hex string like "6d02:5:4bf:8a3b:...")
                    if let Some(state) = parse_saved_state(arg) {
                        valid_args.push(ArgOptions::SavedState(state));
                    }
                    // control char
                    else if let Some(char_index) = cc_to_index(arg) {
                        if let Some(mapping) = args_iter.next() {
                            let cc_mapping = string_to_control_char(mapping).map_err(|e| {
                                let message = match e {
                                    ControlCharMappingError::IntOutOfRange(val) => {
                                        translate!(
                                            "stty-error-invalid-integer-argument-value-too-large",
                                            "value" => format!("'{val}'")
                                        )
                                    }
                                    ControlCharMappingError::MultipleChars(val) => {
                                        translate!(
                                            "stty-error-invalid-integer-argument",
                                            "value" => format!("'{val}'")
                                        )
                                    }
                                };
                                UUsageError::new(1, message)
                            })?;
                            valid_args.push(ArgOptions::Mapping((char_index, cc_mapping)));
                        } else {
                            return missing_arg(arg);
                        }
                    // baud rate
                    } else if let Some(baud_flag) = string_to_baud(arg, flags::BaudType::Both) {
                        valid_args.push(ArgOptions::Flags(baud_flag));
                    // non control char flag
                    } else if let Some(flag) = string_to_flag(arg) {
                        let remove_group = match flag {
                            AllFlags::Baud(_, _) => false,
                            AllFlags::ControlFlags((flag, remove)) => {
                                check_flag_group(flag, remove)
                            }
                            AllFlags::InputFlags((flag, remove)) => check_flag_group(flag, remove),
                            AllFlags::LocalFlags((flag, remove)) => check_flag_group(flag, remove),
                            AllFlags::OutputFlags((flag, remove)) => check_flag_group(flag, remove),
                        };
                        if remove_group {
                            return invalid_arg(arg);
                        }
                        valid_args.push(flag.into());
                    // combination setting
                    } else if let Some(combo) = string_to_combo(arg) {
                        valid_args.append(&mut combo_to_flags(combo));
                    } else {
                        return invalid_arg(arg);
                    }
                }
            }
        }

        let mut settings = get_terminal_settings(opts.file.as_fd())
            .map_err_context(|| opts.device_name.clone())?;

        // iterate over valid_args, match on the arg type, do the matching apply function
        for arg in &valid_args {
            match arg {
                ArgOptions::Mapping(mapping) => apply_char_mapping(&mut settings.termios, mapping),
                ArgOptions::Flags(flag) => apply_setting(&mut settings, flag)?,
                ArgOptions::Special(setting) => {
                    apply_special_setting(&mut settings.termios, setting, opts.file.as_raw_fd())?;
                }
                ArgOptions::Print(setting) => {
                    print_special_setting(setting, &settings, opts.file.as_raw_fd())?;
                }
                ArgOptions::SavedState(state) => {
                    apply_saved_state(&mut settings, state);
                }
            }
        }
        if valid_args
            .iter()
            .any(|arg| !matches!(arg, ArgOptions::Print(_)))
        {
            set_terminal_settings(opts.file.as_fd(), set_arg, &settings)?;
        }
    } else {
        let settings = get_terminal_settings(opts.file.as_fd())
            .map_err_context(|| opts.device_name.clone())?;
        print_settings(&settings, opts)?;
    }
    Ok(())
}

fn get_terminal_settings(fd: BorrowedFd<'_>) -> nix::Result<TerminalSettings> {
    // musl's TCGETS writes only the kernel prefix, not all libc NCCS entries
    // or speed fields. Initialize the complete value before converting to nix.
    // SAFETY: libc::termios contains only integer fields and integer arrays.
    let mut raw = unsafe { std::mem::zeroed::<nix::libc::termios>() };
    #[cfg(linux_termios2)]
    let speeds = {
        ioctl_read_bad!(get_termios2, TCGETS2, termios2);
        // SAFETY: termios2 contains only integer fields and integer arrays.
        let mut kernel = unsafe { std::mem::zeroed::<termios2>() };
        // SAFETY: fd is borrowed for this call and kernel is writable ABI storage.
        unsafe { get_termios2(fd.as_raw_fd(), &raw mut kernel)? };
        raw.c_iflag = kernel.c_iflag;
        raw.c_oflag = kernel.c_oflag;
        raw.c_cflag = kernel.c_cflag;
        raw.c_lflag = kernel.c_lflag;
        raw.c_line = kernel.c_line;
        for (destination, source) in raw.c_cc.iter_mut().zip(kernel.c_cc) {
            *destination = source;
        }
        // Linux's encoded rates take precedence over the numeric fields.
        let output = baud_to_speed(kernel.c_cflag & nix::libc::CBAUD).unwrap_or(kernel.c_ospeed);
        let input = baud_to_speed((kernel.c_cflag & nix::libc::CIBAUD) >> nix::libc::IBSHIFT)
            .unwrap_or(kernel.c_ispeed);
        (input, output)
    };
    #[cfg(not(linux_termios2))]
    {
        // SAFETY: fd is borrowed for this call and raw is initialized ABI storage.
        nix::errno::Errno::result(unsafe { nix::libc::tcgetattr(fd.as_raw_fd(), &raw mut raw) })?;
    }
    let mut settings = TerminalSettings {
        termios: raw.into(),
        #[cfg(linux_termios2)]
        input_speed: if speeds.0 == 0 { speeds.1 } else { speeds.0 },
        #[cfg(linux_termios2)]
        output_speed: speeds.1,
    };
    // nix's libc conversion truncates flags it does not name.
    settings.termios.input_flags = InputFlags::from_bits_retain(raw.c_iflag);
    settings.termios.output_flags = OutputFlags::from_bits_retain(raw.c_oflag);
    settings.termios.control_flags = ControlFlags::from_bits_retain(raw.c_cflag);
    settings.termios.local_flags = LocalFlags::from_bits_retain(raw.c_lflag);
    // Canonical speed flags make saved states independent of libc's conversion.
    #[cfg(linux_termios2)]
    settings.update_speed_flags();
    Ok(settings)
}

fn set_terminal_settings(
    fd: BorrowedFd<'_>,
    action: SetArg,
    settings: &TerminalSettings,
) -> nix::Result<()> {
    #[cfg(linux_termios2)]
    {
        // SAFETY: termios2 contains only integer fields and integer arrays.
        let mut kernel = unsafe { std::mem::zeroed::<termios2>() };
        kernel.c_iflag = settings.termios.input_flags.bits();
        kernel.c_oflag = settings.termios.output_flags.bits();
        kernel.c_cflag = settings.termios.control_flags.bits();
        kernel.c_lflag = settings.termios.local_flags.bits();
        kernel.c_line = settings.termios.line_discipline;
        for (destination, source) in kernel.c_cc.iter_mut().zip(settings.termios.control_chars) {
            *destination = source;
        }
        kernel.c_ispeed = settings.input_speed();
        kernel.c_ospeed = settings.output_speed;
        // Match libc's equal-speed representation without losing split speeds.
        if kernel.c_ispeed == kernel.c_ospeed {
            kernel.c_cflag &= !nix::libc::CIBAUD;
        }
        let request = match action {
            SetArg::TCSANOW => TCSETS2,
            SetArg::TCSADRAIN => TCSETSW2,
            SetArg::TCSAFLUSH => TCSETSF2,
            _ => return Err(nix::errno::Errno::EINVAL),
        };
        // SAFETY: fd is borrowed for this call and kernel has the ioctl's ABI layout.
        nix::errno::Errno::result(unsafe {
            nix::libc::ioctl(fd.as_raw_fd(), request as _, &raw const kernel)
        })
        .map(drop)
    }
    #[cfg(not(linux_termios2))]
    nix::sys::termios::tcsetattr(fd, action, &settings.termios)
}

#[cfg(linux_termios2)]
fn baud_to_speed(code: nix::libc::tcflag_t) -> Option<u32> {
    BAUD_RATES
        .iter()
        .find(|(_, rate)| *rate as nix::libc::tcflag_t == code)
        .and_then(|(text, _)| text.parse().ok())
}

#[cfg(linux_termios2)]
fn speed_to_baud(speed: u32) -> nix::libc::tcflag_t {
    BAUD_RATES
        .iter()
        .find(|(text, _)| text.parse::<u32>().ok() == Some(speed))
        .map_or(nix::libc::BOTHER, |(_, rate)| *rate as _)
}

// The GNU implementation adds the --help message when the args are incorrectly formatted
fn missing_arg<T>(arg: &str) -> Result<T, Box<dyn UError>> {
    Err(UUsageError::new(
        1,
        translate!(
            "stty-error-missing-argument",
            "arg" => *arg
        ),
    ))
}

fn invalid_arg<T>(arg: &str) -> Result<T, Box<dyn UError>> {
    Err(UUsageError::new(
        1,
        translate!(
            "stty-error-invalid-argument",
            "arg" => *arg
        ),
    ))
}

fn invalid_integer_arg<T>(arg: &str) -> Result<T, Box<dyn UError>> {
    Err(UUsageError::new(
        1,
        translate!(
            "stty-error-invalid-integer-argument",
            "value" => format!("'{arg}'")
        ),
    ))
}

fn invalid_speed<T>(arg: &str, speed: &str) -> Result<T, Box<dyn UError>> {
    Err(UUsageError::new(
        1,
        translate!(
            "stty-error-invalid-speed",
            "arg" => arg,
            "speed" => speed,
        ),
    ))
}

/// GNU uses different error messages if values overflow or underflow a u8,
/// this function returns the appropriate error message in the case of overflow or underflow, or u8 on success
fn parse_u8_or_err(arg: &str) -> Result<u8, String> {
    arg.parse::<u8>().map_err(|e| {
        if let IntErrorKind::PosOverflow = e.kind() {
            translate!("stty-error-invalid-integer-argument-value-too-large", "value" => format!("'{arg}'"))
        } else {
            translate!("stty-error-invalid-integer-argument", "value" => format!("'{arg}'"))
        }
    })
}

/// Parse an integer with hex (0x/0X) and octal (0) prefix support, wrapping to u16.
///
/// GNU stty uses an unsigned 32-bit integer for row/col sizes, then wraps to 16 bits.
/// Returns `None` if parsing fails or value exceeds u32::MAX.
fn parse_rows_cols(arg: &str) -> Option<u16> {
    u64::extended_parse(arg)
        .ok()
        .filter(|&n| u32::try_from(n).is_ok())
        .map(|n| (n % (u16::MAX as u64 + 1)) as u16)
}

/// Parse a saved terminal state string in stty format.
///
/// The format is colon-separated hexadecimal values:
/// `input_flags:output_flags:control_flags:local_flags:cc0:cc1:cc2:...`
///
/// - Must have 4 + NCCS parts, with optional Linux input/output speeds
/// - All parts must be non-empty valid hex values
/// - Control characters must fit in u8 (0-255)
/// - Returns `None` if format is invalid
fn parse_saved_state(arg: &str) -> Option<Vec<u32>> {
    let parts: Vec<&str> = arg.split(':').collect();
    let expected_parts = 4 + nix::libc::NCCS;

    #[cfg(linux_termios2)]
    let valid_length = parts.len() == expected_parts || parts.len() == expected_parts + 2;
    #[cfg(not(linux_termios2))]
    let valid_length = parts.len() == expected_parts;
    if !valid_length {
        return None;
    }

    let mut values = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return None; // GNU rejects empty hex values
        }
        let val = u32::from_str_radix(part, 16).ok()?;

        // Only the control character fields, not optional speeds, must fit in u8.
        if (4..expected_parts).contains(&i) && val > 255 {
            return None;
        }

        values.push(val);
    }

    Some(values)
}

fn check_flag_group<T>(flag: &Flag<T>, remove: bool) -> bool {
    remove && flag.group.is_some()
}

fn print_special_setting(
    setting: &PrintSetting,
    settings: &TerminalSettings,
    fd: i32,
) -> nix::Result<()> {
    #[cfg(not(linux_termios2))]
    let termios = &settings.termios;
    match setting {
        PrintSetting::Size => {
            let mut size = TermSize::default();
            unsafe { tiocgwinsz(fd, &raw mut size)? };
            println!("{} {}", size.rows, size.columns);
        }
        PrintSetting::Speed => {
            #[cfg(linux_termios2)]
            let (input, output) = (
                settings.input_speed().to_string(),
                settings.output_speed.to_string(),
            );
            #[cfg(bsd)]
            let (input, output) = (
                nix::sys::termios::cfgetispeed(termios).to_string(),
                nix::sys::termios::cfgetospeed(termios).to_string(),
            );
            #[cfg(all(not(bsd), not(linux_termios2)))]
            let (input, output) = {
                let display = |speed| {
                    BAUD_RATES
                        .iter()
                        .find(|(_, rate)| *rate == speed)
                        .map(|(text, _)| (*text).to_owned())
                        .unwrap_or_else(|| "unknown".to_owned())
                };
                (
                    display(nix::sys::termios::cfgetispeed(termios)),
                    display(nix::sys::termios::cfgetospeed(termios)),
                )
            };
            if input == output || input == "0" {
                println!("{output}");
            } else {
                println!("{input} {output}");
            }
        }
    }
    Ok(())
}

/// Handles line wrapping for stty output to fit within terminal width
struct WrappedPrinter {
    width: usize,
    current: usize,
    first_in_line: bool,
}

impl WrappedPrinter {
    /// Creates a new printer with the specified terminal width.
    /// If term_size is None (typically when output is piped), falls back to
    /// the COLUMNS environment variable or a default width of 80 columns.
    fn new(term_size: Option<&TermSize>) -> Self {
        let columns = if let Some(term_size) = term_size {
            term_size.columns
        } else {
            const DEFAULT_TERM_WIDTH: u16 = 80;

            std::env::var_os("COLUMNS")
                .and_then(|s| s.to_str()?.parse().ok())
                .filter(|&c| c > 0)
                .unwrap_or(DEFAULT_TERM_WIDTH)
        };

        Self {
            width: columns.max(1) as usize,
            current: 0,
            first_in_line: true,
        }
    }

    fn print(&mut self, token: &str) {
        let token_len = self.prefix().chars().count() + token.chars().count();
        if self.current > 0 && self.current + token_len > self.width {
            println!();
            self.current = 0;
            self.first_in_line = true;
        }

        print!("{}{token}", self.prefix());
        self.current += token_len;
        self.first_in_line = false;
    }

    fn prefix(&self) -> &str {
        if self.first_in_line { "" } else { " " }
    }

    fn flush(&mut self) {
        if self.current > 0 {
            println!();
            self.current = 0;
            self.first_in_line = false;
        }
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "needed for some platform-specific code"
)]
fn print_terminal_size(
    settings: &TerminalSettings,
    opts: &Options,
    window_size: Option<&TermSize>,
    term_size: Option<&TermSize>,
) -> nix::Result<()> {
    let termios = &settings.termios;
    #[cfg(linux_termios2)]
    let speed = settings.output_speed;
    #[cfg(not(linux_termios2))]
    let speed = nix::sys::termios::cfgetospeed(termios);

    let mut printer = WrappedPrinter::new(window_size);

    // Linux termios2 and BSD use numeric baud rates directly.
    #[cfg(any(
        bsd,
        all(
            any(target_os = "linux", target_os = "android"),
            linux_termios2
        )
    ))]
    printer.print(&translate!("stty-output-speed", "speed" => speed));

    // Linux PowerPC targets currently use nix's BaudRate representation.
    #[cfg(all(
        any(target_os = "linux", target_os = "android"),
        not(linux_termios2)
    ))]
    {
        let speed_str = BAUD_RATES
            .iter()
            .find(|(_, rate)| *rate == speed)
            .map(|(text, _)| *text)
            .unwrap_or("unknown");
        printer.print(&translate!("stty-output-speed", "speed" => speed_str));
    }

    // Other platforms need to use the baud rate enum, so printing the right value
    // becomes slightly more complicated.
    #[cfg(all(
        not(any(target_os = "linux", target_os = "android")),
        not(bsd)
    ))]
    for (text, baud_rate) in BAUD_RATES {
        if *baud_rate == speed {
            printer.print(&translate!("stty-output-speed", "speed" => (*text)));
            break;
        }
    }

    if opts.all {
        let term_size = term_size.as_ref().expect("terminal size should be set");
        printer.print(
            &translate!("stty-output-rows-columns", "rows" => term_size.rows, "columns" => term_size.columns),
        );
    }

    #[cfg(any(target_os = "linux", target_os = "redox"))]
    {
        // For some reason the normal nix Termios struct does not expose the line,
        // so we get the underlying libc::termios struct to get that information.
        let libc_termios: nix::libc::termios = termios.clone().into();
        let line = libc_termios.c_line;
        printer.print(&translate!("stty-output-line", "line" => line));
    }
    printer.flush();
    Ok(())
}

fn cc_to_index(option: &str) -> Option<S> {
    for cc in CONTROL_CHARS {
        if option == cc.0 {
            return Some(cc.1);
        }
    }
    None
}

fn string_to_combo(arg: &str) -> Option<&str> {
    let is_negated = arg.starts_with('-');
    let name = arg.trim_start_matches('-');
    COMBINATION_SETTINGS
        .iter()
        .find(|&&(combo_name, is_negatable)| name == combo_name && (!is_negated || is_negatable))
        .map(|_| arg)
}

/// Parse and round a baud rate value using GNU stty's custom rounding algorithm.
///
/// Accepts decimal values with the following rounding rules:
/// - If first digit after decimal > 5: round up
/// - If first digit after decimal < 5: round down
/// - If first digit after decimal == 5:
///   - If followed by any non-zero digit: round up
///   - If followed only by zeros (or nothing): banker's rounding (round to nearest even)
///
/// Examples: "9600.49" -> 9600, "9600.51" -> 9600, "9600.5" -> 9600 (even), "9601.5" -> 9602 (even)
fn parse_baud_with_rounding(normalized: &str) -> Option<u32> {
    let (int_part, frac_part) = match normalized.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (normalized, None),
    };

    let mut value = int_part.parse::<u32>().ok()?;

    if let Some(frac) = frac_part {
        let mut chars = frac.chars();
        let first_digit = chars.next()?.to_digit(10)?;
        let rest: Vec<_> = chars.collect();
        if !rest.iter().all(char::is_ascii_digit) {
            return None;
        }

        match first_digit.cmp(&5) {
            Ordering::Greater => value = value.checked_add(1)?,
            Ordering::Equal => {
                if rest.iter().any(|&c| c != '0') {
                    value = value.checked_add(1)?;
                } else {
                    value = value.checked_add(value & 1)?;
                }
            }
            Ordering::Less => {}
        }
    }

    Some(value)
}

fn string_to_baud(arg: &str, baud_type: flags::BaudType) -> Option<AllFlags<'_>> {
    // GNU accepts the historical aliases even though their spelling contains
    // a character otherwise reserved for scientific notation.
    let alias = matches!(arg.trim(), "exta" | "extb");
    if !alias
        && (arg != arg.trim_end()
            || arg.trim().starts_with('-')
            || arg.trim().starts_with("++")
            || arg.contains('E')
            || arg.contains('e')
            || arg.matches('.').count() > 1)
    {
        return None;
    }

    let normalized = arg.trim().trim_start_matches('+');
    let normalized = normalized.strip_suffix('.').unwrap_or(normalized);

    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // Linux termios2 exposes arbitrary rates through BOTHER.  Keep the
        // historical aliases used by GNU stty in addition to numeric rates.
        let value = match normalized {
            "exta" => 19_200,
            "extb" => 38_400,
            _ => parse_baud_with_rounding(normalized)?,
        };
        return Some(AllFlags::Baud(value, baud_type));
    }

    #[cfg(bsd)]
    return match normalized {
        "exta" => Some(AllFlags::Baud(19_200, baud_type)),
        "extb" => Some(AllFlags::Baud(38_400, baud_type)),
        _ => parse_baud_with_rounding(normalized).map(|value| AllFlags::Baud(value, baud_type)),
    };

    #[cfg(all(not(bsd), not(any(target_os = "linux", target_os = "android"))))]
    {
        let value = parse_baud_with_rounding(normalized)?;
        BAUD_RATES
            .iter()
            .find(|(text, _)| text.parse::<u32>().ok() == Some(value))
            .map(|(_, baud_rate)| AllFlags::Baud(*baud_rate, baud_type))
    }
}

/// return `Some(flag)` if the input is a valid flag, `None` if not
fn string_to_flag(option: &str) -> Option<AllFlags<'_>> {
    let remove = option.starts_with('-');
    let name = option.trim_start_matches('-');

    for cflag in CONTROL_FLAGS {
        if name == cflag.name {
            return Some(AllFlags::ControlFlags((cflag, remove)));
        }
    }
    for iflag in INPUT_FLAGS {
        if name == iflag.name {
            return Some(AllFlags::InputFlags((iflag, remove)));
        }
    }
    for lflag in LOCAL_FLAGS {
        if name == lflag.name {
            return Some(AllFlags::LocalFlags((lflag, remove)));
        }
    }
    for oflag in OUTPUT_FLAGS {
        if name == oflag.name {
            return Some(AllFlags::OutputFlags((oflag, remove)));
        }
    }
    None
}

fn control_char_to_string(cc: nix::libc::cc_t) -> nix::Result<String> {
    if cc == 0 {
        return Ok(translate!("stty-output-undef"));
    }

    let (meta_prefix, code) = if cc >= 0x80 {
        ("M-", cc - 0x80)
    } else {
        ("", cc)
    };

    // Determine the '^'-prefix if applicable and character based on the code
    let (ctrl_prefix, character) = match code {
        // Control characters in ASCII range
        0..=0x1f => Ok(("^", (b'@' + code) as char)),
        // Printable ASCII characters
        0x20..=0x7e => Ok(("", code as char)),
        // DEL character
        0x7f => Ok(("^", '?')),
        // Out of range (above 8 bits)
        _ => Err(nix::errno::Errno::ERANGE),
    }?;

    Ok(format!("{meta_prefix}{ctrl_prefix}{character}"))
}

fn print_control_chars(
    termios: &Termios,
    opts: &Options,
    term_size: Option<&TermSize>,
) -> nix::Result<()> {
    if !opts.all {
        // Print only control chars that differ from sane defaults
        let mut printer = WrappedPrinter::new(term_size);
        for (text, cc_index) in CONTROL_CHARS {
            let current_val = termios.control_chars[*cc_index as usize];
            let sane_val = get_sane_control_char(*cc_index);

            if current_val != sane_val {
                printer.print(&format!(
                    "{text} = {};",
                    control_char_to_string(current_val)?
                ));
            }
        }
        printer.flush();
        return Ok(());
    }

    let mut printer = WrappedPrinter::new(term_size);
    for (text, cc_index) in CONTROL_CHARS {
        printer.print(&format!(
            "{text} = {};",
            control_char_to_string(termios.control_chars[*cc_index as usize])?
        ));
    }
    printer.print(&translate!("stty-output-min-time",
        "min" => termios.control_chars[S::VMIN as usize],
        "time" => termios.control_chars[S::VTIME as usize]
    ));
    printer.flush();
    Ok(())
}

fn print_in_save_format(settings: &TerminalSettings) {
    let termios = &settings.termios;
    print!(
        "{:x}:{:x}:{:x}:{:x}",
        termios.input_flags.bits(),
        termios.output_flags.bits(),
        termios.control_flags.bits(),
        termios.local_flags.bits()
    );
    for cc in termios.control_chars {
        print!(":{cc:x}");
    }
    #[cfg(linux_termios2)]
    {
        let flags = termios.control_flags.bits();
        if flags & nix::libc::CBAUD == nix::libc::BOTHER
            || (flags & nix::libc::CIBAUD) >> nix::libc::IBSHIFT == nix::libc::BOTHER
        {
            print!(":{:x}:{:x}", settings.input_speed(), settings.output_speed);
        }
    }
    println!();
}

/// Gets terminal size using the tiocgwinsz ioctl system call.
/// This queries the kernel for the current terminal window dimensions.
fn get_terminal_size(fd: RawFd) -> nix::Result<TermSize> {
    let mut term_size = TermSize::default();
    unsafe { tiocgwinsz(fd, &raw mut term_size) }.map(|_| term_size)
}

fn print_settings(settings: &TerminalSettings, opts: &Options) -> nix::Result<()> {
    let termios = &settings.termios;
    if opts.save {
        print_in_save_format(settings);
    } else {
        let device_fd = opts.file.as_raw_fd();
        let term_size = if opts.all {
            Some(get_terminal_size(device_fd)?)
        } else {
            get_terminal_size(device_fd).ok()
        };

        let stdout_fd = stdout().as_raw_fd();
        let window_size = if device_fd == stdout_fd {
            &term_size
        } else {
            &get_terminal_size(stdout_fd).ok()
        };

        print_terminal_size(settings, opts, window_size.as_ref(), term_size.as_ref())?;
        print_control_chars(termios, opts, window_size.as_ref())?;
        print_flags(termios, opts, CONTROL_FLAGS, window_size.as_ref());
        print_flags(termios, opts, INPUT_FLAGS, window_size.as_ref());
        print_flags(termios, opts, OUTPUT_FLAGS, window_size.as_ref());
        print_flags(termios, opts, LOCAL_FLAGS, window_size.as_ref());
    }
    Ok(())
}

fn print_flags<T: TermiosFlag>(
    termios: &Termios,
    opts: &Options,
    flags: &[Flag<T>],
    term_size: Option<&TermSize>,
) {
    let mut printer = WrappedPrinter::new(term_size);
    for &Flag {
        name,
        flag,
        show,
        sane,
        group,
    } in flags
    {
        if !show {
            continue;
        }
        let val = flag.is_in(termios, group);
        if group.is_some() {
            if val && (!sane || opts.all) {
                printer.print(name);
            }
        } else if opts.all || val != sane {
            if !val {
                printer.print(&format!("-{name}"));
                continue;
            }
            printer.print(name);
        }
    }
    printer.flush();
}

/// Apply a single setting
fn apply_setting(settings: &mut TerminalSettings, setting: &AllFlags) -> nix::Result<()> {
    match setting {
        AllFlags::Baud(_, _) => apply_baud_rate_flag(settings, setting)?,
        AllFlags::ControlFlags((setting, disable)) => {
            setting.flag.apply(&mut settings.termios, !disable);
        }
        AllFlags::InputFlags((setting, disable)) => {
            setting.flag.apply(&mut settings.termios, !disable);
        }
        AllFlags::LocalFlags((setting, disable)) => {
            setting.flag.apply(&mut settings.termios, !disable);
        }
        AllFlags::OutputFlags((setting, disable)) => {
            setting.flag.apply(&mut settings.termios, !disable);
        }
    }
    Ok(())
}

fn apply_baud_rate_flag(settings: &mut TerminalSettings, input: &AllFlags) -> nix::Result<()> {
    if let AllFlags::Baud(rate, baud_type) = input {
        #[cfg(linux_termios2)]
        {
            match baud_type {
                flags::BaudType::Input => settings.input_speed = *rate,
                flags::BaudType::Output => settings.output_speed = *rate,
                flags::BaudType::Both => {
                    settings.input_speed = *rate;
                    settings.output_speed = *rate;
                }
            }
            settings.update_speed_flags();
        }
        #[cfg(not(linux_termios2))]
        {
            let termios = &mut settings.termios;
            match baud_type {
                flags::BaudType::Input => nix::sys::termios::cfsetispeed(termios, *rate)?,
                flags::BaudType::Output => nix::sys::termios::cfsetospeed(termios, *rate)?,
                flags::BaudType::Both => {
                    nix::sys::termios::cfsetispeed(termios, *rate)?;
                    nix::sys::termios::cfsetospeed(termios, *rate)?;
                }
            }
        }
    }
    Ok(())
}

fn apply_char_mapping(termios: &mut Termios, mapping: &(S, u8)) {
    termios.control_chars[mapping.0 as usize] = mapping.1;
}

/// Apply a saved terminal state to the current termios.
///
/// The state array contains:
/// - `state[0]`: input flags
/// - `state[1]`: output flags
/// - `state[2]`: control flags
/// - `state[3]`: local flags
/// - `state[4..4 + NCCS]`: control characters
/// - Optional final Linux fields: numeric input and output speeds
///
/// If state has fewer than 4 elements, no changes are applied. This is a defensive
/// check that should never trigger since `parse_saved_state` rejects such states.
fn apply_saved_state(settings: &mut TerminalSettings, state: &[u32]) {
    if state.len() < 4 {
        return;
    }

    let termios = &mut settings.termios;
    termios.input_flags = InputFlags::from_bits_retain(state[0] as _);
    termios.output_flags = OutputFlags::from_bits_retain(state[1] as _);
    termios.control_flags = ControlFlags::from_bits_retain(state[2] as _);
    termios.local_flags = LocalFlags::from_bits_retain(state[3] as _);

    for (destination, &source) in termios.control_chars.iter_mut().zip(&state[4..]) {
        *destination = source as u8;
    }
    #[cfg(linux_termios2)]
    {
        if let Some(&[input, output]) = state.get(4 + nix::libc::NCCS..) {
            settings.input_speed = input;
            settings.output_speed = output;
        } else {
            let flags = termios.control_flags.bits();
            if let Some(speed) = baud_to_speed(flags & nix::libc::CBAUD) {
                settings.output_speed = speed;
            }
            if let Some(speed) = baud_to_speed((flags & nix::libc::CIBAUD) >> nix::libc::IBSHIFT) {
                settings.input_speed = speed;
            }
        }
        settings.update_speed_flags();
    }
}

fn apply_special_setting(
    _termios: &mut Termios,
    setting: &SpecialSetting,
    fd: i32,
) -> nix::Result<()> {
    let mut size = TermSize::default();
    unsafe { tiocgwinsz(fd, &raw mut size)? };
    match setting {
        SpecialSetting::Rows(n) => size.rows = *n,
        SpecialSetting::Cols(n) => size.columns = *n,
        #[cfg_attr(
            not(any(target_os = "linux", target_os = "android")),
            expect(unused_variables)
        )]
        SpecialSetting::Line(n) => {
            // nix only defines Termios's `line_discipline` field on these platforms
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                _termios.line_discipline = *n;
            }
        }
    }
    unsafe { tiocswinsz(fd, &raw mut size)? };
    Ok(())
}

/// GNU stty defines some valid values for the control character mappings
/// 1. Standard character, can be a a single char (ie 'C') or hat notation (ie '^C')
/// 2. Integer
///    a. hexadecimal, prefixed by '0x'
///    b. octal, prefixed by '0'
///    c. decimal, no prefix
/// 3. Disabling the control character: '^-' or 'undef'
///
/// This function returns the ascii value of valid control chars, or [`ControlCharMappingError`] if invalid
fn string_to_control_char(s: &str) -> Result<u8, ControlCharMappingError> {
    if s == "undef" || s == "^-" || s.is_empty() {
        return Ok(0);
    }

    // try to parse integer (hex, octal, or decimal)
    let ascii_num = if let Some(hex) = s.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).ok()
    } else if let Some(octal) = s.strip_prefix("0") {
        if octal.is_empty() {
            Some(0)
        } else {
            u32::from_str_radix(octal, 8).ok()
        }
    } else {
        s.parse::<u32>().ok()
    };

    if let Some(val) = ascii_num {
        if val > 255 {
            return Err(ControlCharMappingError::IntOutOfRange(s.to_string()));
        }
        return Ok(val as u8);
    }
    // try to parse ^<char> or just <char>
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some('^'), Some(c)) => {
            // special case: ascii value of '^?' is greater than '?'
            if c == '?' {
                return Ok(ASCII_DEL);
            }
            // subtract by '@' to turn the char into the ascii value of '^<char>'
            Ok((c.to_ascii_uppercase() as u8).wrapping_sub(b'@'))
        }
        (Some(c), None) => Ok(c as u8),
        (Some(_), Some(_)) => Err(ControlCharMappingError::MultipleChars(s.to_string())),
        _ => unreachable!("No arguments provided: must have been caught earlier"),
    }
}

// decomposes a combination argument into a vec of corresponding flags
fn combo_to_flags(combo: &str) -> Vec<ArgOptions<'_>> {
    let mut flags = Vec::new();
    let mut ccs = Vec::new();
    #[expect(clippy::match_same_arms)]
    match combo {
        "lcase" | "LCASE" => {
            flags = vec!["xcase", "iuclc", "olcuc"];
        }
        "-lcase" | "-LCASE" => {
            flags = vec!["-xcase", "-iuclc", "-olcuc"];
        }
        "cbreak" => {
            flags = vec!["-icanon"];
        }
        "-cbreak" => {
            flags = vec!["icanon"];
        }
        "cooked" | "-raw" => {
            flags = vec![
                "brkint", "ignpar", "istrip", "icrnl", "ixon", "opost", "isig", "icanon",
            ];
            ccs = vec![(S::VEOF, "^D"), (S::VEOL, "")];
        }
        "crt" => {
            flags = vec!["echoe", "echoctl", "echoke"];
        }
        "dec" => {
            flags = vec!["echoe", "echoctl", "echoke", "-ixany"];
            ccs = vec![(S::VINTR, "^C"), (S::VERASE, "^?"), (S::VKILL, "^U")];
        }
        "decctlq" => {
            flags = vec!["ixany"];
        }
        "-decctlq" => {
            flags = vec!["-ixany"];
        }
        "ek" => {
            ccs = vec![(S::VERASE, "^?"), (S::VKILL, "^U")];
        }
        "evenp" | "parity" => {
            flags = vec!["parenb", "-parodd", "cs7"];
        }
        "-evenp" | "-parity" => {
            flags = vec!["-parenb", "cs8"];
        }
        "litout" => {
            flags = vec!["-parenb", "-istrip", "-opost", "cs8"];
        }
        "-litout" => {
            flags = vec!["parenb", "istrip", "opost", "cs7"];
        }
        "nl" => {
            flags = vec!["-icrnl", "-onlcr"];
        }
        "-nl" => {
            flags = vec!["icrnl", "-inlcr", "-igncr", "onlcr", "-ocrnl", "-onlret"];
        }
        "oddp" => {
            flags = vec!["parenb", "parodd", "cs7"];
        }
        "-oddp" => {
            flags = vec!["-parenb", "cs8"];
        }
        "pass8" => {
            flags = vec!["-parenb", "-istrip", "cs8"];
        }
        "-pass8" => {
            flags = vec!["parenb", "istrip", "cs7"];
        }
        "raw" | "-cooked" => {
            flags = vec![
                "-ignbrk", "-brkint", "-ignpar", "-parmrk", "-inpck", "-istrip", "-inlcr",
                "-igncr", "-icrnl", "-ixon", "-ixoff", "-icanon", "-opost", "-isig", "-iuclc",
                "-xcase", "-ixany", "-imaxbel",
            ];
            ccs = vec![(S::VMIN, "1"), (S::VTIME, "0")];
        }
        "sane" => {
            flags = vec![
                "cread", "-ignbrk", "brkint", "-inlcr", "-igncr", "icrnl", "icanon", "iexten",
                "echo", "echoe", "echok", "-echonl", "-noflsh", "-ixoff", "-iutf8", "-iuclc",
                "-xcase", "-ixany", "imaxbel", "-olcuc", "-ocrnl", "opost", "-ofill", "onlcr",
                "-onocr", "-onlret", "nl0", "cr0", "tab0", "bs0", "vt0", "ff0", "isig", "-tostop",
                "-ofdel", "-echoprt", "echoctl", "echoke", "-extproc", "-flusho",
            ];
            ccs = vec![
                (S::VINTR, "^C"),
                (S::VQUIT, "^\\"),
                (S::VERASE, "^?"),
                (S::VKILL, "^U"),
                (S::VEOF, "^D"),
                (S::VEOL, ""),
                (S::VEOL2, ""),
                #[cfg(target_os = "linux")]
                (S::VSWTC, ""),
                (S::VSTART, "^Q"),
                (S::VSTOP, "^S"),
                (S::VSUSP, "^Z"),
                (S::VREPRINT, "^R"),
                (S::VWERASE, "^W"),
                (S::VLNEXT, "^V"),
                (S::VDISCARD, "^O"),
            ];
        }
        "tabs" => {
            flags = vec!["tab0"];
        }
        "-tabs" => {
            flags = vec!["tab3"];
        }
        _ => unreachable!("invalid combination setting: must have been caught earlier"),
    }
    let mut flags = flags
        .iter()
        .filter_map(|f| string_to_flag(f).map(ArgOptions::Flags))
        .collect::<Vec<ArgOptions>>();
    let mut ccs = ccs
        .iter()
        .map(|cc| ArgOptions::Mapping((cc.0, string_to_control_char(cc.1).unwrap())))
        .collect::<Vec<ArgOptions>>();
    flags.append(&mut ccs);
    flags
}

fn get_sane_control_char(cc_index: S) -> u8 {
    for (sane_index, sane_val) in SANE_CONTROL_CHARS {
        if sane_index == cc_index {
            return sane_val;
        }
    }
    // Default values for control chars not in the sane list
    match cc_index {
        S::VEOL => 0,
        S::VEOL2 => 0,
        S::VMIN => 1,
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "sparc64"),
            target_os = "illumos",
            target_os = "solaris"
        )))]
        S::VTIME => 0,
        #[cfg(target_os = "linux")]
        S::VSWTC => 0,
        _ => 0,
    }
}

pub fn uu_app() -> Command {
    Command::new("stty")
        .version(uucore::crate_version!())
        .help_template(uucore::localized_help_template("stty"))
        .override_usage(format_usage(&translate!("stty-usage")))
        .about(translate!("stty-about"))
        .infer_long_args(true)
        .arg(
            Arg::new(options::ALL)
                .short('a')
                .long(options::ALL)
                .help(translate!("stty-option-all"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::SAVE)
                .short('g')
                .long(options::SAVE)
                .help(translate!("stty-option-save"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::FILE)
                .short('F')
                .long(options::FILE)
                .value_hint(clap::ValueHint::FilePath)
                .value_name("DEVICE")
                .help(translate!("stty-option-file")),
        )
        .arg(
            Arg::new(options::SETTINGS)
                .action(ArgAction::Append)
                .allow_hyphen_values(true)
                .help(translate!("stty-option-settings")),
        )
}

impl TermiosFlag for ControlFlags {
    fn is_in(&self, termios: &Termios, group: Option<Self>) -> bool {
        termios.control_flags.contains(*self)
            && group.is_none_or(|g| !termios.control_flags.intersects(g - *self))
    }

    fn apply(&self, termios: &mut Termios, val: bool) {
        termios.control_flags.set(*self, val);
    }
}

impl TermiosFlag for InputFlags {
    fn is_in(&self, termios: &Termios, group: Option<Self>) -> bool {
        termios.input_flags.contains(*self)
            && group.is_none_or(|g| !termios.input_flags.intersects(g - *self))
    }

    fn apply(&self, termios: &mut Termios, val: bool) {
        termios.input_flags.set(*self, val);
    }
}

impl TermiosFlag for OutputFlags {
    fn is_in(&self, termios: &Termios, group: Option<Self>) -> bool {
        termios.output_flags.contains(*self)
            && group.is_none_or(|g| !termios.output_flags.intersects(g - *self))
    }

    fn apply(&self, termios: &mut Termios, val: bool) {
        termios.output_flags.set(*self, val);
    }
}

impl TermiosFlag for LocalFlags {
    fn is_in(&self, termios: &Termios, group: Option<Self>) -> bool {
        termios.local_flags.contains(*self)
            && group.is_none_or(|g| !termios.local_flags.intersects(g - *self))
    }

    fn apply(&self, termios: &mut Termios, val: bool) {
        termios.local_flags.set(*self, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Essential unit tests for complex internal parsing and logic functions.

    // Control character parsing tests
    #[test]
    fn test_string_to_control_char_undef() {
        assert_eq!(string_to_control_char("undef").unwrap(), 0);
        assert_eq!(string_to_control_char("^-").unwrap(), 0);
        assert_eq!(string_to_control_char("").unwrap(), 0);
    }

    #[test]
    fn test_string_to_control_char_hat_notation() {
        assert_eq!(string_to_control_char("^C").unwrap(), 3);
        assert_eq!(string_to_control_char("^A").unwrap(), 1);
        assert_eq!(string_to_control_char("^?").unwrap(), 127);
    }

    #[test]
    fn test_string_to_control_char_formats() {
        assert_eq!(string_to_control_char("A").unwrap(), b'A');
        assert_eq!(string_to_control_char("65").unwrap(), 65);
        assert_eq!(string_to_control_char("0x41").unwrap(), 0x41);
        assert_eq!(string_to_control_char("0101").unwrap(), 0o101);
    }

    #[test]
    fn test_string_to_control_char_overflow() {
        assert!(string_to_control_char("256").is_err());
        assert!(string_to_control_char("0x100").is_err());
        assert!(string_to_control_char("0400").is_err());
    }

    // Control character formatting tests
    #[test]
    fn test_control_char_to_string_formats() {
        assert_eq!(
            control_char_to_string(0).unwrap(),
            translate!("stty-output-undef")
        );
        assert_eq!(control_char_to_string(3).unwrap(), "^C");
        assert_eq!(control_char_to_string(b'A').unwrap(), "A");
        assert_eq!(control_char_to_string(0x7f).unwrap(), "^?");
        assert_eq!(control_char_to_string(0x80).unwrap(), "M-^@");
    }

    // Combination settings tests
    #[test]
    fn test_combo_to_flags_sane() {
        let flags = combo_to_flags("sane");
        assert!(flags.len() > 5); // sane sets multiple flags
    }

    #[test]
    fn test_combo_to_flags_raw_cooked() {
        assert!(!combo_to_flags("raw").is_empty());
        assert!(!combo_to_flags("cooked").is_empty());
        assert!(!combo_to_flags("-raw").is_empty());
    }

    #[test]
    fn test_combo_to_flags_parity() {
        assert!(!combo_to_flags("evenp").is_empty());
        assert!(!combo_to_flags("oddp").is_empty());
        assert!(!combo_to_flags("-evenp").is_empty());
    }

    // Parse rows/cols with overflow handling
    #[test]
    fn test_parse_rows_cols_normal() {
        let result = parse_rows_cols("24");
        assert_eq!(result, Some(24));
    }

    #[test]
    fn test_parse_rows_cols_overflow() {
        assert_eq!(parse_rows_cols("65536"), Some(0)); // wraps to 0
        assert_eq!(parse_rows_cols("65537"), Some(1)); // wraps to 1
    }

    // Sane control character defaults
    #[test]
    fn test_get_sane_control_char_values() {
        assert_eq!(get_sane_control_char(S::VINTR), 3); // ^C
        assert_eq!(get_sane_control_char(S::VQUIT), 28); // ^\
        assert_eq!(get_sane_control_char(S::VERASE), 127); // DEL
        assert_eq!(get_sane_control_char(S::VKILL), 21); // ^U
        assert_eq!(get_sane_control_char(S::VEOF), 4); // ^D
    }

    // Additional tests for parse_rows_cols
    #[test]
    fn test_parse_rows_cols_valid() {
        assert_eq!(parse_rows_cols("80"), Some(80));
        assert_eq!(parse_rows_cols("65535"), Some(65535));
        assert_eq!(parse_rows_cols("0"), Some(0));
        assert_eq!(parse_rows_cols("1"), Some(1));
    }

    #[test]
    fn test_parse_rows_cols_wraparound() {
        // Test u16 wraparound: (u16::MAX + 1) % (u16::MAX + 1) = 0
        assert_eq!(parse_rows_cols("131071"), Some(65535)); // (2*65536 - 1) % 65536 = 65535
        assert_eq!(parse_rows_cols("131072"), Some(0)); // (2*65536) % 65536 = 0
    }

    #[test]
    fn test_parse_rows_cols_invalid() {
        assert_eq!(parse_rows_cols(""), None);
        assert_eq!(parse_rows_cols("abc"), None);
        assert_eq!(parse_rows_cols("-1"), None);
        assert_eq!(parse_rows_cols("12.5"), None);
        assert_eq!(parse_rows_cols("not_a_number"), None);
    }

    // Tests for string_to_baud
    #[test]
    fn test_string_to_baud_valid() {
        #[cfg(not(bsd))]
        {
            assert!(string_to_baud("9600", flags::BaudType::Both).is_some());
            assert!(string_to_baud("115200", flags::BaudType::Both).is_some());
            assert!(string_to_baud("38400", flags::BaudType::Both).is_some());
            assert!(string_to_baud("19200", flags::BaudType::Both).is_some());
        }

        #[cfg(bsd)]
        {
            assert!(string_to_baud("9600", flags::BaudType::Both).is_some());
            assert!(string_to_baud("115200", flags::BaudType::Both).is_some());
            assert!(string_to_baud("1000000", flags::BaudType::Both).is_some());
            assert!(string_to_baud("0", flags::BaudType::Both).is_some());
        }
    }

    #[test]
    fn test_string_to_baud_invalid() {
        #[cfg(not(bsd))]
        {
            assert_eq!(string_to_baud("995", flags::BaudType::Both), None);
            assert_eq!(string_to_baud("invalid", flags::BaudType::Both), None);
            assert_eq!(string_to_baud("", flags::BaudType::Both), None);
            assert_eq!(string_to_baud("abc", flags::BaudType::Both), None);
        }
    }

    #[test]
    fn test_parse_baud_with_rounding_rejects_u32_overflow() {
        assert_eq!(parse_baud_with_rounding("4294967295.4"), Some(u32::MAX));
        assert_eq!(parse_baud_with_rounding("4294967295.6"), None);
        assert_eq!(parse_baud_with_rounding("4294967295.5"), None);
        assert_eq!(parse_baud_with_rounding("4294967295.5001"), None);
    }

    // Tests for string_to_combo
    #[test]
    fn test_string_to_combo_valid() {
        assert_eq!(string_to_combo("sane"), Some("sane"));
        assert_eq!(string_to_combo("raw"), Some("raw"));
        assert_eq!(string_to_combo("cooked"), Some("cooked"));
        assert_eq!(string_to_combo("-raw"), Some("-raw"));
        assert_eq!(string_to_combo("-cooked"), Some("-cooked"));
        assert_eq!(string_to_combo("cbreak"), Some("cbreak"));
        assert_eq!(string_to_combo("-cbreak"), Some("-cbreak"));
        assert_eq!(string_to_combo("nl"), Some("nl"));
        assert_eq!(string_to_combo("-nl"), Some("-nl"));
        assert_eq!(string_to_combo("ek"), Some("ek"));
        assert_eq!(string_to_combo("evenp"), Some("evenp"));
        assert_eq!(string_to_combo("-evenp"), Some("-evenp"));
        assert_eq!(string_to_combo("parity"), Some("parity"));
        assert_eq!(string_to_combo("-parity"), Some("-parity"));
        assert_eq!(string_to_combo("oddp"), Some("oddp"));
        assert_eq!(string_to_combo("-oddp"), Some("-oddp"));
        assert_eq!(string_to_combo("pass8"), Some("pass8"));
        assert_eq!(string_to_combo("-pass8"), Some("-pass8"));
        assert_eq!(string_to_combo("litout"), Some("litout"));
        assert_eq!(string_to_combo("-litout"), Some("-litout"));
        assert_eq!(string_to_combo("crt"), Some("crt"));
        assert_eq!(string_to_combo("dec"), Some("dec"));
        assert_eq!(string_to_combo("decctlq"), Some("decctlq"));
        assert_eq!(string_to_combo("-decctlq"), Some("-decctlq"));
    }

    #[test]
    fn test_string_to_combo_invalid() {
        assert_eq!(string_to_combo("notacombo"), None);
        assert_eq!(string_to_combo(""), None);
        assert_eq!(string_to_combo("invalid"), None);
        // Test non-negatable combos with negation
        assert_eq!(string_to_combo("-sane"), None);
        assert_eq!(string_to_combo("-ek"), None);
        assert_eq!(string_to_combo("-crt"), None);
        assert_eq!(string_to_combo("-dec"), None);
    }

    // Tests for cc_to_index
    #[test]
    fn test_cc_to_index_valid() {
        assert_eq!(cc_to_index("intr"), Some(S::VINTR));
        assert_eq!(cc_to_index("quit"), Some(S::VQUIT));
        assert_eq!(cc_to_index("erase"), Some(S::VERASE));
        assert_eq!(cc_to_index("kill"), Some(S::VKILL));
        assert_eq!(cc_to_index("eof"), Some(S::VEOF));
        assert_eq!(cc_to_index("start"), Some(S::VSTART));
        assert_eq!(cc_to_index("stop"), Some(S::VSTOP));
        assert_eq!(cc_to_index("susp"), Some(S::VSUSP));
        assert_eq!(cc_to_index("rprnt"), Some(S::VREPRINT));
        assert_eq!(cc_to_index("werase"), Some(S::VWERASE));
        assert_eq!(cc_to_index("lnext"), Some(S::VLNEXT));
        assert_eq!(cc_to_index("discard"), Some(S::VDISCARD));
    }

    #[test]
    fn test_cc_to_index_invalid() {
        // spell-checker:ignore notachar
        assert_eq!(cc_to_index("notachar"), None);
        assert_eq!(cc_to_index(""), None);
        assert_eq!(cc_to_index("INTR"), None); // case sensitive
        assert_eq!(cc_to_index("invalid"), None);
    }

    // Tests for check_flag_group
    #[test]
    fn test_check_flag_group() {
        let flag_with_group = Flag::new_grouped("cs5", ControlFlags::CS5, ControlFlags::CSIZE);
        let flag_without_group = Flag::new("parenb", ControlFlags::PARENB);

        assert!(check_flag_group(&flag_with_group, true));
        assert!(!check_flag_group(&flag_with_group, false));
        assert!(!check_flag_group(&flag_without_group, true));
        assert!(!check_flag_group(&flag_without_group, false));
    }

    // Additional tests for get_sane_control_char
    #[test]
    fn test_get_sane_control_char_all_defined() {
        assert_eq!(get_sane_control_char(S::VSTART), 17); // ^Q
        assert_eq!(get_sane_control_char(S::VSTOP), 19); // ^S
        assert_eq!(get_sane_control_char(S::VSUSP), 26); // ^Z
        assert_eq!(get_sane_control_char(S::VREPRINT), 18); // ^R
        assert_eq!(get_sane_control_char(S::VWERASE), 23); // ^W
        assert_eq!(get_sane_control_char(S::VLNEXT), 22); // ^V
        assert_eq!(get_sane_control_char(S::VDISCARD), 15); // ^O
    }

    // Tests for parse_u8_or_err
    #[test]
    fn test_parse_u8_or_err_valid() {
        assert_eq!(parse_u8_or_err("0").unwrap(), 0);
        assert_eq!(parse_u8_or_err("255").unwrap(), 255);
        assert_eq!(parse_u8_or_err("128").unwrap(), 128);
        assert_eq!(parse_u8_or_err("1").unwrap(), 1);
    }

    #[test]
    fn test_parse_u8_or_err_overflow() {
        // Test that overflow values return an error
        // Note: In test environment, translate!() returns the key, not the translated string
        // spell-checker:ignore Valeur
        let err = parse_u8_or_err("256").unwrap_err();
        assert!(
            err.contains("value-too-large")
                || err.contains("Value too large")
                || err.contains("Valeur trop grande"),
            "Expected overflow error, got: {err}"
        );

        assert!(parse_u8_or_err("1000").is_err());
        assert!(parse_u8_or_err("65536").is_err());
    }

    #[test]
    fn test_parse_u8_or_err_invalid() {
        // Test that invalid values return an error
        // Note: In test environment, translate!() returns the key, not the translated string
        // spell-checker:ignore entier invalide
        let err = parse_u8_or_err("-1").unwrap_err();
        assert!(
            err.contains("invalid-integer-argument")
                || err.contains("invalid integer argument")
                || err.contains("argument entier invalide"),
            "Expected invalid argument error, got: {err}"
        );

        assert!(parse_u8_or_err("abc").is_err());
        assert!(parse_u8_or_err("").is_err());
        assert!(parse_u8_or_err("12.5").is_err());
    }
}
