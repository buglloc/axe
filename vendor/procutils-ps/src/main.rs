// AXE modification: normalize BSD option style through the library API.

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = procutils_ps::preprocess_argv(std::env::args_os().collect());
    procutils_ps::run(procutils_ps::Args::parse_from(args))
}
