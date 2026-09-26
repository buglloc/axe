use std::ffi::OsString;

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
    for command in args.commands {
        match which::which_all(&command) {
            Ok(paths) => {
                let mut found = false;
                for path in paths {
                    found = true;
                    if !args.silent {
                        println!("{}", path.display());
                    }
                    if !args.all {
                        break;
                    }
                }
                missing |= !found;
            }
            Err(_) => missing = true,
        }
    }

    i32::from(missing)
}
