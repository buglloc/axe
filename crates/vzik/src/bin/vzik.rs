fn main() {
    std::process::exit(vzik::entry_with_signals(std::env::args_os().collect()));
}
