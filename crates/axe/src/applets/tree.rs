use std::ffi::OsString;

use clap::Parser;
use clap::error::ErrorKind;
use rust_tree::rust_tree::cli::{Cli, cli_to_options};
use rust_tree::rust_tree::traversal::list_directory;

pub fn tree(args: Vec<OsString>) -> i32 {
    // GNU tree prints human-readable sizes for -h; rust_tree hides that
    // behind --human-readable and uses -h for help, so remap the flag.
    let args: Vec<OsString> = args
        .into_iter()
        .map(|arg| {
            if arg == "-h" {
                "--human-readable".into()
            } else {
                arg
            }
        })
        .collect();
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
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

    let options = match cli_to_options(&cli) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("tree: {error}");
            return 2;
        }
    };

    if let Err(error) = list_directory(&cli.path, &options) {
        eprintln!("tree: {}: {error}", cli.path);
        return 1;
    }
    0
}
