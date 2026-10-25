use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    procutils_watch::run(procutils_watch::Args::parse())
}
