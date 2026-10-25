use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;

use awk_rs::{Interpreter, Lexer, Parser};

#[derive(Debug)]
struct StdoutFailure(io::Error);

impl std::fmt::Display for StdoutFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for StdoutFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

fn stdout_failure(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), StdoutFailure(error))
}

struct ClassifiedStdout<'a>(io::StdoutLock<'a>);

impl Write for ClassifiedStdout<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes).map_err(stdout_failure)
    }

    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.0.write_all(bytes).map_err(stdout_failure)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush().map_err(stdout_failure)
    }
}

pub fn awk(args: Vec<OsString>) -> i32 {
    match run(&args[1..]) {
        Ok(code) => code,
        Err(error) => {
            let output_failed = matches!(
                error.downcast_ref::<awk_rs::Error>(),
                Some(awk_rs::Error::Io(error))
                    if error.get_ref().is_some_and(|error| error.is::<StdoutFailure>())
            );
            eprintln!("awk: {error}");
            if output_failed { 1 } else { 2 }
        }
    }
}

fn run(args: &[OsString]) -> Result<i32, Box<dyn std::error::Error>> {
    let mut program_fragments = Vec::new();
    let mut operands = Vec::new();
    let mut variables = Vec::new();
    let mut posix_mode = false;
    let mut traditional_mode = false;
    let mut options = true;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].as_bytes();
        if options && arg == b"--" {
            options = false;
            index += 1;
            continue;
        }
        if options && (arg == b"--help" || arg == b"-h") {
            print_help();
            return Ok(0);
        }
        if options && arg == b"--version" {
            println!("awk {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }
        if options && (arg == b"--posix" || arg == b"-P") {
            posix_mode = true;
            traditional_mode = false;
        } else if options && matches!(arg, b"--traditional" | b"--compat" | b"-c") {
            traditional_mode = true;
            posix_mode = false;
        } else if options && arg.starts_with(b"-F") {
            let separator = option_value(args, &mut index, b"-F")?;
            let mut assignment = Vec::with_capacity(3 + separator.len());
            assignment.extend_from_slice(b"FS=");
            assignment.extend_from_slice(separator);
            variables.push(assignment);
        } else if options && arg.starts_with(b"-v") {
            variables.push(option_value(args, &mut index, b"-v")?.to_vec());
        } else if options && arg.starts_with(b"-f") {
            let path = option_value(args, &mut index, b"-f")?;
            let source = if path == b"-" {
                let mut source = String::new();
                io::stdin().read_to_string(&mut source)?;
                source
            } else {
                fs::read_to_string(OsStr::from_bytes(path))?
            };
            program_fragments.push(source);
        } else if options && arg.starts_with(b"-") && arg != b"-" {
            return Err(format!("unknown option: {}", args[index].to_string_lossy()).into());
        } else if program_fragments.is_empty() {
            program_fragments.push(
                args[index]
                    .to_str()
                    .ok_or("program is not valid UTF-8")?
                    .to_owned(),
            );
            options = false;
        } else {
            operands.extend(args[index..].iter().map(|arg| arg.as_bytes().to_vec()));
            break;
        }
        index += 1;
    }

    if program_fragments.is_empty() {
        return Err("no program provided".into());
    }
    let program_source = program_fragments.join("\n");
    let mut lexer = Lexer::new(&program_source);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    let mut interpreter = Interpreter::new(&program);
    interpreter.set_posix_mode(posix_mode);
    interpreter.set_traditional_mode(traditional_mode);
    let mut argv = Vec::with_capacity(operands.len() + 1);
    argv.push(b"awk".to_vec());
    argv.extend(operands);
    interpreter.set_args(argv);
    for assignment in variables {
        interpreter.set_assignment(&assignment)?;
    }

    let stdout = io::stdout();
    Ok(interpreter.run(&mut ClassifiedStdout(stdout.lock()))?)
}

fn option_value<'a>(
    args: &'a [OsString],
    index: &mut usize,
    option: &[u8],
) -> Result<&'a [u8], Box<dyn std::error::Error>> {
    let argument = args[*index].as_bytes();
    if argument.len() > option.len() {
        return Ok(&argument[option.len()..]);
    }
    *index += 1;
    args.get(*index).map(|arg| arg.as_bytes()).ok_or_else(|| {
        format!(
            "option {} requires an argument",
            std::str::from_utf8(option).expect("option names are ASCII")
        )
        .into()
    })
}

fn print_help() {
    println!(
        "Usage: awk [OPTIONS] 'program' [file ...]\n\
         \nOptions:\n\
         \x20 -F fs             Set the field separator\n\
         \x20 -v var=val        Assign a variable before execution\n\
         \x20 -f program-file   Read a program fragment (repeatable)\n\
         \x20 -P, --posix       Enable strict POSIX mode\n\
         \x20 -c, --traditional Disable extensions\n\
         \x20 --help            Print help\n\
         \x20 --version         Print version"
    );
}
