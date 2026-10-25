use clap::Parser as _;

fn main() -> std::process::ExitCode {
    procutils_w::run(procutils_w::Args::parse())
}
