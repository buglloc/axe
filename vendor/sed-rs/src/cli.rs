use clap::Parser;
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Parser, Debug, Default)]
#[command(
    name = "sed",
    version,
    about = "A GNU-compatible stream editor implemented in Rust",
    max_term_width = 100
)]
pub struct Options {
    /// Suppress automatic printing of pattern space
    #[arg(short = 'n', long = "quiet", alias = "silent")]
    pub quiet: bool,

    /// Add the script to the commands to be executed
    #[arg(
        short = 'e',
        long = "expression",
        value_name = "SCRIPT",
        allow_hyphen_values = true
    )]
    pub expressions: Vec<String>,

    /// Add the contents of script-file to the commands to be executed
    #[arg(
        short = 'f',
        long = "file",
        value_name = "SCRIPT-FILE",
        allow_hyphen_values = true
    )]
    pub script_files: Vec<PathBuf>,

    /// Edit files in place (makes backup if SUFFIX supplied)
    #[arg(
        long = "in-place",
        value_name = "SUFFIX",
        num_args = 0..=1,
        default_missing_value = "",
        require_equals = true
    )]
    pub in_place: Option<OsString>,

    /// Follow symbolic links when editing files in place
    #[arg(long = "follow-symlinks")]
    pub follow_symlinks: bool,

    /// Use extended regular expressions instead of basic regular expressions
    #[arg(short = 'E', short_alias = 'r', long = "regexp-extended")]
    pub extended_regexp: bool,

    /// Consider files as separate rather than as a single continuous stream
    #[arg(short = 's', long = "separate")]
    pub separate: bool,

    /// Separate lines by NUL characters
    #[arg(short = 'z', long = "null-data")]
    pub null_data: bool,

    /// [SCRIPT] [INPUT-FILE...]
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

impl Options {
    pub fn script_and_files(&self) -> crate::Result<(String, Vec<PathBuf>)> {
        let mut scripts = self.expressions.clone();
        for path in &self.script_files {
            let content = std::fs::read_to_string(path).map_err(|error| {
                crate::Error::Parse(format!(
                    "couldn't read script file '{}': {error}",
                    path.display()
                ))
            })?;
            scripts.push(content);
        }

        if scripts.is_empty() {
            let Some((first, rest)) = self.args.split_first() else {
                return Err(crate::Error::Parse(
                    "no script provided. Usage: sed [OPTIONS] SCRIPT [FILE...]".into(),
                ));
            };
            let script = first
                .to_str()
                .ok_or_else(|| crate::Error::Parse("script is not valid UTF-8".into()))?;
            Ok((script.to_owned(), rest.iter().map(PathBuf::from).collect()))
        } else {
            Ok((
                scripts.join("\n"),
                self.args.iter().map(PathBuf::from).collect(),
            ))
        }
    }
}

/// Normalize short options before clap's hyphen-valued positional lookahead.
pub fn preprocess_args(raw: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut result = Vec::new();
    let mut options = true;
    let mut option_value = false;
    'arguments: for (index, arg) in raw.into_iter().enumerate() {
        if index == 0 || !options {
            result.push(arg);
            continue;
        }
        if option_value {
            let option = result
                .last_mut()
                .expect("a pending option has been emitted");
            option.push("=");
            option.push(arg);
            option_value = false;
            continue;
        }
        if arg == "--" {
            options = false;
            result.push(arg);
            continue;
        }
        if arg == "--expression" || arg == "--file" {
            option_value = true;
            result.push(arg);
            continue;
        }
        let bytes = arg.as_encoded_bytes();
        if bytes.starts_with(b"-") && !bytes.starts_with(b"--") {
            for offset in 1..bytes.len() {
                match bytes[offset] {
                    option @ (b'i' | b'e' | b'f') => {
                        if offset != 1 {
                            // SAFETY: the prefix ends at an ASCII option boundary
                            // inside this OsString's unchanged platform encoding.
                            let prefix = unsafe {
                                std::ffi::OsStr::from_encoded_bytes_unchecked(&bytes[..offset])
                            };
                            result.push(prefix.to_os_string());
                        }
                        let mut normalized = OsString::from(match option {
                            b'i' => "--in-place",
                            b'e' => "--expression",
                            b'f' => "--file",
                            _ => unreachable!(),
                        });
                        if option == b'i' || offset + 1 != bytes.len() {
                            normalized.push("=");
                            // SAFETY: cutting after an ASCII option letter
                            // preserves the original platform encoding.
                            let value = unsafe {
                                std::ffi::OsStr::from_encoded_bytes_unchecked(&bytes[offset + 1..])
                            };
                            normalized.push(value);
                        } else {
                            option_value = true;
                        }
                        result.push(normalized);
                        continue 'arguments;
                    }
                    b'n' | b'E' | b'r' | b's' | b'z' => {}
                    _ => break,
                }
            }
        }
        if !bytes.starts_with(b"-") || arg == "-" {
            options = false;
        }
        result.push(arg);
    }
    result
}
