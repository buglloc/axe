use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    procutils_top::run(procutils_top::Args::parse())
}
