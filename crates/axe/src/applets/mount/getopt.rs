use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use super::{Failure, usage};

#[derive(Clone, Copy)]
pub(super) enum Applet {
    Mount,
    Umount,
}

impl Applet {
    pub(super) fn bad_usage(self, message: impl std::fmt::Display) -> Failure {
        let name = match self {
            Self::Mount => "mount",
            Self::Umount => "umount",
        };
        usage(format!(
            "{message}\nTry '{name} --help' for more information."
        ))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Argument {
    None,
    Required,
    Optional,
}

pub(super) struct OptionSpec {
    pub(super) name: &'static str,
    short: u8,
    argument: Argument,
}

impl OptionSpec {
    pub(super) const fn flag(name: &'static str, short: u8) -> Self {
        Self {
            name,
            short,
            argument: Argument::None,
        }
    }

    pub(super) const fn value(name: &'static str, short: u8) -> Self {
        Self {
            name,
            short,
            argument: Argument::Required,
        }
    }

    pub(super) const fn optional(name: &'static str, short: u8) -> Self {
        Self {
            name,
            short,
            argument: Argument::Optional,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Spelling {
    Long,
    Short(u8),
}

impl Spelling {
    pub(super) fn display(self, name: &str) -> String {
        match self {
            Self::Long => format!("--{name}"),
            Self::Short(flag) => format!("-{}", char::from(flag)),
        }
    }
}

pub(super) enum Item {
    Operand(OsString),
    Option(&'static str, Option<OsString>, Spelling),
}

pub(super) struct Getopt {
    args: std::vec::IntoIter<OsString>,
    cluster: Option<(OsString, usize)>,
    operands_only: bool,
    applet: Applet,
    options: &'static [OptionSpec],
}

impl Getopt {
    pub(super) fn new(
        args: Vec<OsString>,
        applet: Applet,
        options: &'static [OptionSpec],
    ) -> Self {
        let mut args = args.into_iter();
        args.next();
        Self {
            args,
            cluster: None,
            operands_only: false,
            applet,
            options,
        }
    }

    pub(super) fn next(&mut self) -> Result<Option<Item>, Failure> {
        if let Some((cluster, offset)) = self.cluster.take() {
            return self.short(cluster, offset).map(Some);
        }
        let Some(arg) = self.args.next() else {
            return Ok(None);
        };
        let bytes = arg.as_bytes();
        if self.operands_only || bytes == b"-" || !bytes.starts_with(b"-") {
            return Ok(Some(Item::Operand(arg)));
        }
        if bytes == b"--" {
            self.operands_only = true;
            return Ok(self.args.next().map(Item::Operand));
        }
        let Some(long) = bytes.strip_prefix(b"--") else {
            return self.short(arg, 1).map(Some);
        };
        let split = long.iter().position(|byte| *byte == b'=');
        let name = &long[..split.unwrap_or(long.len())];
        let name = std::str::from_utf8(name).map_err(|_| {
            self.applet
                .bad_usage(format!("unrecognized option '{}'", arg.display()))
        })?;
        let option = long_option(self.options, self.applet, name)?;
        let inline = split.map(|index| OsString::from_vec(long[index + 1..].to_vec()));
        let value = match (option.argument, inline) {
            (Argument::None, Some(_)) => {
                return Err(self.applet.bad_usage(format!(
                    "option '--{}' doesn't allow an argument",
                    option.name
                )));
            }
            (Argument::Required, None) => Some(self.args.next().ok_or_else(|| {
                self.applet.bad_usage(format!(
                    "option '--{}' requires an argument",
                    option.name
                ))
            })?),
            (_, inline) => inline,
        };
        Ok(Some(Item::Option(option.name, value, Spelling::Long)))
    }

    fn short(&mut self, cluster: OsString, offset: usize) -> Result<Item, Failure> {
        let bytes = cluster.as_bytes();
        let flag = bytes[offset];
        let option = self
            .options
            .iter()
            .find(|option| option.short == flag)
            .ok_or_else(|| {
                let flag = OsStr::from_bytes(&bytes[offset..])
                    .to_string_lossy()
                    .chars()
                    .next()
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                self.applet
                    .bad_usage(format!("invalid option -- '{flag}'"))
            })?;
        let rest = &bytes[offset + 1..];
        let value = match option.argument {
            Argument::None => {
                if !rest.is_empty() {
                    self.cluster = Some((cluster, offset + 1));
                }
                None
            }
            Argument::Required if rest.is_empty() => {
                Some(self.args.next().ok_or_else(|| {
                    self.applet.bad_usage(format!(
                        "option requires an argument -- '{}'",
                        char::from(flag)
                    ))
                })?)
            }
            Argument::Optional if rest.is_empty() => None,
            Argument::Required | Argument::Optional => {
                Some(OsString::from_vec(rest.to_vec()))
            }
        };
        Ok(Item::Option(option.name, value, Spelling::Short(flag)))
    }
}

/// The table order also defines the order of ambiguous long-option candidates.
pub(super) fn long_option(
    options: &'static [OptionSpec],
    applet: Applet,
    name: &str,
) -> Result<&'static OptionSpec, Failure> {
    if let Some(exact) = options.iter().find(|option| option.name == name) {
        return Ok(exact);
    }
    let mut candidates = options
        .iter()
        .filter(|option| !name.is_empty() && option.name.starts_with(name));
    let Some(first) = candidates.next() else {
        return Err(applet.bad_usage(format!("unrecognized option '--{name}'")));
    };
    if candidates.next().is_none() {
        return Ok(first);
    }
    let possibilities = options
        .iter()
        .filter(|option| option.name.starts_with(name))
        .map(|option| format!(" '--{}'", option.name))
        .collect::<String>();
    Err(applet.bad_usage(format!(
        "option '--{name}' is ambiguous; possibilities:{possibilities}"
    )))
}
