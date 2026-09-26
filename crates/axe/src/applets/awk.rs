use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};

use awk_rs::{Interpreter, Lexer, Parser};

pub fn awk(args: Vec<OsString>) -> i32 {
    let args = match args
        .into_iter()
        .skip(1)
        .map(|arg| arg.into_string().map_err(|_| "argument is not valid UTF-8"))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(args) => args,
        Err(error) => {
            eprintln!("awk: {error}");
            return 2;
        }
    };

    match run(&args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("awk: {error}");
            2
        }
    }
}

fn run(args: &[String]) -> Result<i32, Box<dyn std::error::Error>> {
    let mut field_separator = " ".to_string();
    let mut program_source = None;
    let mut input_files = Vec::new();
    let mut variables = Vec::new();
    let mut posix_mode = false;
    let mut traditional_mode = false;

    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--help" || arg == "-h" {
            print_help();
            return Ok(0);
        }

        if arg == "--version" {
            println!("awk {}", env!("CARGO_PKG_VERSION"));
            return Ok(0);
        }

        if arg == "--posix" || arg == "-P" {
            posix_mode = true;
            traditional_mode = false;
            index += 1;
            continue;
        }

        if arg == "--traditional" || arg == "--compat" || arg == "-c" {
            traditional_mode = true;
            posix_mode = false;
            index += 1;
            continue;
        }

        if arg == "-F" {
            index += 1;
            field_separator = args
                .get(index)
                .ok_or("option -F requires an argument")?
                .clone();
        } else if let Some(separator) = arg.strip_prefix("-F") {
            field_separator = separator.to_string();
        } else if arg == "-v" {
            index += 1;
            let assignment = args.get(index).ok_or("option -v requires an argument")?;
            let (name, value) = assignment
                .split_once('=')
                .ok_or_else(|| format!("invalid variable assignment: {assignment}"))?;
            variables.push((name.to_string(), unescape_assignment(value)));
        } else if arg == "-f" {
            index += 1;
            let script = args.get(index).ok_or("option -f requires an argument")?;
            program_source = Some(fs::read_to_string(script)?);
        } else if arg == "--" {
            index += 1;
            if program_source.is_none() {
                program_source = args.get(index).cloned();
                index += usize::from(program_source.is_some());
            }
            for operand in &args[index..] {
                if let Some((name, value)) = variable_assignment(operand) {
                    variables.push((name.to_owned(), unescape_assignment(value)));
                } else {
                    input_files.push(operand.clone());
                }
            }
            break;
        } else if arg.starts_with('-') && arg != "-" {
            return Err(format!("unknown option: {arg}").into());
        } else if program_source.is_none() {
            program_source = Some(arg.clone());
        } else if let Some((name, value)) = variable_assignment(arg) {
            variables.push((name.to_owned(), unescape_assignment(value)));
        } else {
            input_files.push(arg.clone());
        }
        index += 1;
    }

    let program_source = program_source.ok_or("no program provided")?;
    let mut lexer = Lexer::new(&program_source);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse()?;

    let mut interpreter = Interpreter::new(&program);
    interpreter.set_posix_mode(posix_mode);
    interpreter.set_traditional_mode(traditional_mode);
    interpreter.set_fs(&field_separator);

    let mut argv = vec!["awk".to_string()];
    argv.extend(input_files.iter().cloned());
    interpreter.set_args(argv);
    for (name, value) in variables {
        interpreter.set_variable(&name, &value);
    }

    let mut inputs: Vec<Box<dyn BufRead>> = Vec::new();
    let mut filenames = Vec::new();
    if input_files.is_empty() {
        inputs.push(Box::new(BufReader::new(io::stdin())));
        filenames.push(String::new());
    } else {
        for filename in &input_files {
            if filename == "-" {
                inputs.push(Box::new(BufReader::new(io::stdin())));
            } else {
                inputs.push(Box::new(BufReader::new(File::open(filename)?)));
            }
            filenames.push(filename.clone());
        }
    }
    interpreter.set_filenames(filenames);

    let stdout = io::stdout();
    Ok(interpreter.run(inputs, &mut stdout.lock())?)
}

fn variable_assignment(value: &str) -> Option<(&str, &str)> {
    let (name, value) = value.split_once('=')?;
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first == '_' || first.is_ascii_alphabetic())
        || !chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return None;
    }
    Some((name, value))
}

fn unescape_assignment(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }

        match chars.peek().copied() {
            Some('n') => push_escape(&mut chars, &mut output, '\n'),
            Some('t') => push_escape(&mut chars, &mut output, '\t'),
            Some('r') => push_escape(&mut chars, &mut output, '\r'),
            Some('b') => push_escape(&mut chars, &mut output, '\x08'),
            Some('f') => push_escape(&mut chars, &mut output, '\x0c'),
            Some('a') => push_escape(&mut chars, &mut output, '\x07'),
            Some('v') => push_escape(&mut chars, &mut output, '\x0b'),
            Some('\\') => push_escape(&mut chars, &mut output, '\\'),
            Some('"') => push_escape(&mut chars, &mut output, '"'),
            Some('/') => push_escape(&mut chars, &mut output, '/'),
            Some(digit) if digit.is_digit(8) => {
                let mut digits = String::new();
                while digits.len() < 3 && chars.peek().is_some_and(|c| c.is_digit(8)) {
                    digits.push(chars.next().expect("peeked digit"));
                }
                match u8::from_str_radix(&digits, 8) {
                    Ok(byte) => output.push(byte as char),
                    Err(_) => {
                        output.push('\\');
                        output.push_str(&digits);
                    }
                }
            }
            Some('x') => {
                chars.next();
                let mut digits = String::new();
                while digits.len() < 2 && chars.peek().is_some_and(char::is_ascii_hexdigit) {
                    digits.push(chars.next().expect("peeked digit"));
                }
                match u8::from_str_radix(&digits, 16) {
                    Ok(byte) => output.push(byte as char),
                    Err(_) => {
                        output.push('x');
                        output.push_str(&digits);
                    }
                }
            }
            Some(other) => {
                chars.next();
                output.push(other);
            }
            None => output.push('\\'),
        }
    }

    output
}

fn push_escape<I: Iterator<Item = char>>(
    chars: &mut std::iter::Peekable<I>,
    output: &mut String,
    character: char,
) {
    chars.next();
    output.push(character);
}

fn print_help() {
    println!(
        "Usage: awk [OPTIONS] 'program' [file ...]\n\
         \nOptions:\n\
         \x20 -F fs             Set the field separator\n\
         \x20 -v var=val        Assign a variable before execution\n\
         \x20 -f program-file   Read the program from a file\n\
         \x20 -P, --posix       Enable strict POSIX mode\n\
         \x20 -c, --traditional Disable extensions\n\
         \x20 --help            Print help\n\
         \x20 --version         Print version"
    );
}
