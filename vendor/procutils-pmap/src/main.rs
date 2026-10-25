use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    procutils_pmap::run(procutils_pmap::Args::parse())
}
