use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    procutils_free::run(procutils_free::Args::parse())
}
