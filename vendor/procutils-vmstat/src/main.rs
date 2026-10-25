use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    procutils_vmstat::run(procutils_vmstat::Args::parse())
}
