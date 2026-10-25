use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    procutils_pgrep::run(procutils_pgrep::Args::parse())
}
