use std::ffi::{OsStr, OsString};
use std::io::{self, Write};

use clap::error::ErrorKind;
use clap::{ArgAction, Parser};

#[derive(Parser)]
#[command(name = "which", version, about = "Locate a command")]
struct Args {
    /// Print every matching executable instead of only the first one.
    #[arg(short = 'a', long)]
    all: bool,

    /// Return status only; do not print matches.
    #[arg(short = 's', long)]
    silent: bool,

    #[arg(required = true, action = ArgAction::Append)]
    commands: Vec<OsString>,
}

pub fn which(args: Vec<OsString>) -> i32 {
    let args = match Args::try_parse_from(args) {
        Ok(args) => args,
        Err(error) => {
            let code = if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                0
            } else {
                2
            };
            let _ = error.print();
            return code;
        }
    };

    let mut missing = false;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    for command in args.commands {
        let mut found = false;
        if let Ok(paths) = which::which_all(&command) {
            for path in paths {
                found = true;
                if !args.silent
                    && let Err(error) = stdout
                        .write_all(path.as_os_str().as_encoded_bytes())
                        .and_then(|()| stdout.write_all(b"\n"))
                {
                    eprintln!("which: write error: {}", uucore::error::strip_errno(&error));
                    return 1;
                }
                if !args.all {
                    break;
                }
            }
        }
        if !found {
            missing = true;
            if !args.silent {
                let bytes = command.as_encoded_bytes();
                let (name, paths) = match bytes.iter().rposition(|byte| *byte == b'/') {
                    Some(index) => (&bytes[index + 1..], &bytes[..index]),
                    None => (bytes, b"".as_slice()),
                };
                let path = std::env::var_os("PATH").unwrap_or_default();
                let paths = if bytes.contains(&b'/') {
                    paths
                } else {
                    OsStr::new(&path).as_encoded_bytes()
                };
                let mut stderr = io::stderr().lock();
                let _ = stderr
                    .write_all(b"which: no ")
                    .and_then(|()| stderr.write_all(name))
                    .and_then(|()| stderr.write_all(b" in ("))
                    .and_then(|()| stderr.write_all(paths))
                    .and_then(|()| stderr.write_all(b")\n"));
            }
        }
    }
    if let Err(error) = stdout.flush() {
        eprintln!("which: write error: {}", uucore::error::strip_errno(&error));
        return 1;
    }

    i32::from(missing)
}
