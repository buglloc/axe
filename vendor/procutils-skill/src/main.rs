use clap::Parser as _;

fn main() -> std::process::ExitCode {
    let argv = procutils_skill::preprocess_argv(std::env::args().collect());
    procutils_skill::run(procutils_skill::Args::parse_from(argv))
}
