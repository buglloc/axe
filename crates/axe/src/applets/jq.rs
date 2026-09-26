use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use jaq_all::data::{self, Runner};
use jaq_all::jaq_core::{Exn, ValT, Vars};
use jaq_all::json::{self, Val};

const HELP: &str = "Usage: jq [OPTIONS] FILTER [FILE ...]\n\
\n\
Options:\n\
  -r, --raw-output       output strings without JSON quotes\n\
  -R, --raw-input        read each input line as a string\n\
  -j, --join-output      output strings without JSON quotes or newlines\n\
  -c, --compact-output   output compact JSON\n\
  -s, --slurp            read all inputs into an array\n\
  -S, --sort-keys        sort object keys in output\n\
  -e, --exit-status      set status from the last output value\n\
  -n, --null-input       run FILTER once with null input\n\
      --arg NAME VALUE   bind VALUE as a string\n\
      --argjson NAME JSON bind VALUE as JSON\n\
      --rawfile NAME FILE bind the complete file as a string\n\
      --slurpfile NAME FILE bind the file's JSON values as an array\n\
  -h, --help             show this help\n\
  -V, --version          show version";

#[derive(Default)]
struct Options {
    raw_output: bool,
    raw_input: bool,
    join_output: bool,
    compact_output: bool,
    slurp: bool,
    sort_keys: bool,
    exit_status: bool,
    null_input: bool,
    bindings: Vec<(String, Val)>,
    filter: Option<String>,
    files: Vec<PathBuf>,
}

enum ParseOutcome {
    Run(Options),
    Exit(i32),
}

#[derive(Debug)]
enum RunError {
    Input(String),
    Filter(String),
    Halt(i32),
    Io(io::Error),
}

pub fn jq(args: Vec<OsString>) -> i32 {
    let options = match parse_args(args) {
        Ok(ParseOutcome::Run(options)) => options,
        Ok(ParseOutcome::Exit(code)) => return code,
        Err(error) => {
            eprintln!("jq: {error}");
            return 2;
        }
    };

    match run(options) {
        Ok(code) => code,
        Err(RunError::Halt(code)) => code,
        Err(RunError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(RunError::Io(error)) => {
            eprintln!("jq: {error}");
            2
        }
        Err(RunError::Input(error)) => {
            eprintln!("jq: parse error: {error}");
            5
        }
        Err(RunError::Filter(error)) => {
            eprintln!("jq: error: {error}");
            5
        }
    }
}

fn parse_args(args: Vec<OsString>) -> Result<ParseOutcome, String> {
    let mut options = Options::default();
    let mut args = args.into_iter().skip(1).peekable();
    let mut positional_only = false;

    while let Some(arg) = args.next() {
        let text = arg
            .into_string()
            .map_err(|arg| format!("argument is not valid UTF-8: {arg:?}"))?;
        if !positional_only && text == "--" {
            positional_only = true;
            continue;
        }
        if !positional_only && text.starts_with("--") {
            match text.as_str() {
                "--raw-output" => options.raw_output = true,
                "--raw-input" => options.raw_input = true,
                "--join-output" => {
                    options.raw_output = true;
                    options.join_output = true;
                }
                "--compact-output" => options.compact_output = true,
                "--slurp" => options.slurp = true,
                "--sort-keys" => options.sort_keys = true,
                "--exit-status" => options.exit_status = true,
                "--null-input" => options.null_input = true,
                "--arg" | "--argjson" | "--rawfile" | "--slurpfile" => {
                    let name = next_utf8(&mut args, &text)?;
                    let value = next_utf8(&mut args, &text)?;
                    let value = match text.as_str() {
                        "--arg" => Val::utf8_str(value),
                        "--argjson" => {
                            json::read::parse_single(value.as_bytes()).map_err(|error| {
                                format!("invalid JSON value for --argjson {name}: {error}")
                            })?
                        }
                        "--rawfile" => Val::utf8_str(
                            fs::read_to_string(&value)
                                .map_err(|error| format!("{value}: {error}"))?,
                        ),
                        "--slurpfile" => {
                            let file =
                                File::open(&value).map_err(|error| format!("{value}: {error}"))?;
                            let values = json::read::read_many(BufReader::new(file))
                                .collect::<Result<Vec<_>, _>>()
                                .map_err(|error| format!("{value}: {error}"))?;
                            Val::Arr(json::Rc::new(values))
                        }
                        _ => unreachable!(),
                    };
                    if let Some(binding) = options
                        .bindings
                        .iter_mut()
                        .find(|(existing, _)| existing == &name)
                    {
                        binding.1 = value;
                    } else {
                        options.bindings.push((name, value));
                    }
                }
                "--help" => {
                    println!("{HELP}");
                    return Ok(ParseOutcome::Exit(0));
                }
                "--version" => {
                    println!("jq (axe {}, jaq)", env!("CARGO_PKG_VERSION"));
                    return Ok(ParseOutcome::Exit(0));
                }
                _ => return Err(format!("unknown option: {text}")),
            }
            continue;
        }
        if !positional_only && text.starts_with('-') && text != "-" {
            for flag in text[1..].chars() {
                match flag {
                    'r' => options.raw_output = true,
                    'R' => options.raw_input = true,
                    'j' => {
                        options.raw_output = true;
                        options.join_output = true;
                    }
                    'c' => options.compact_output = true,
                    's' => options.slurp = true,
                    'S' => options.sort_keys = true,
                    'e' => options.exit_status = true,
                    'n' => options.null_input = true,
                    'h' => {
                        println!("{HELP}");
                        return Ok(ParseOutcome::Exit(0));
                    }
                    'V' => {
                        println!("jq (axe {}, jaq)", env!("CARGO_PKG_VERSION"));
                        return Ok(ParseOutcome::Exit(0));
                    }
                    _ => return Err(format!("unknown option: -{flag}")),
                }
            }
            continue;
        }

        if options.filter.is_none() {
            options.filter = Some(text);
        } else {
            options.files.push(PathBuf::from(text));
        }
    }

    options.filter.get_or_insert_with(|| ".".to_owned());
    Ok(ParseOutcome::Run(options))
}

fn next_utf8(args: &mut impl Iterator<Item = OsString>, option: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{option} expects a name and a value"))?
        .into_string()
        .map_err(|arg| format!("argument is not valid UTF-8: {arg:?}"))
}

fn run(options: Options) -> Result<i32, RunError> {
    let output_options = OutputOptions {
        raw_output: options.raw_output,
        join_output: options.join_output,
    };
    let filter_source = options.filter.as_deref().expect("filter checked by parser");
    let variable_names = options
        .bindings
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let variable_values = options
        .bindings
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    let filter = jaq_all::compile_with(
        filter_source,
        jaq_all::defs(),
        data::base_funs(),
        &variable_names,
    )
    .map_err(|reports| {
        let mut message = String::new();
        for report in reports {
            message.push_str(&jaq_all::load::FileReportsDisp::new(&report).to_string());
        }
        RunError::Filter(message.trim_end().to_string())
    })?;

    let runner = Runner {
        null_input: options.null_input,
        ..Runner::default()
    };
    let pp = json::write::Pp {
        indent: (!options.compact_output).then(|| "  ".to_string()),
        sort_keys: options.sort_keys,
        sep_space: !options.compact_output,
        ..json::write::Pp::default()
    };
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut last = None;

    if options.slurp {
        let input = if options.raw_input {
            let mut text = String::new();
            if !options.null_input {
                if options.files.is_empty() {
                    io::stdin()
                        .read_to_string(&mut text)
                        .map_err(RunError::Io)?;
                } else {
                    for path in &options.files {
                        if path.as_os_str() == "-" {
                            io::stdin()
                                .read_to_string(&mut text)
                                .map_err(RunError::Io)?;
                        } else {
                            File::open(path)
                                .and_then(|mut file| file.read_to_string(&mut text))
                                .map_err(|error| {
                                    RunError::Io(io::Error::new(
                                        error.kind(),
                                        format!("{}: {error}", path.display()),
                                    ))
                                })?;
                        }
                    }
                }
            }
            Val::utf8_str(text)
        } else {
            let mut values = Vec::new();
            if !options.null_input {
                if options.files.is_empty() {
                    let stdin = io::stdin();
                    collect_inputs(&mut values, json::read::read_many(stdin.lock()))?;
                } else {
                    for path in &options.files {
                        if path.as_os_str() == "-" {
                            let stdin = io::stdin();
                            collect_inputs(&mut values, json::read::read_many(stdin.lock()))?;
                        } else {
                            let file = File::open(path).map_err(|error| {
                                RunError::Io(io::Error::new(
                                    error.kind(),
                                    format!("{}: {error}", path.display()),
                                ))
                            })?;
                            collect_inputs(
                                &mut values,
                                json::read::read_many(BufReader::new(file)),
                            )?;
                        }
                    }
                }
            }
            Val::Arr(json::Rc::new(values))
        };
        last = execute(
            &runner,
            &filter,
            Vars::new(variable_values),
            std::iter::once(Ok::<_, String>(input)),
            &mut stdout,
            &pp,
            &output_options,
        )?;
    } else if options.files.is_empty() {
        let stdin = io::stdin();
        if options.raw_input {
            last = execute(
                &runner,
                &filter,
                Vars::new(variable_values),
                raw_lines(stdin.lock()),
                &mut stdout,
                &pp,
                &output_options,
            )?;
        } else {
            last = execute(
                &runner,
                &filter,
                Vars::new(variable_values),
                json::read::read_many(stdin.lock()),
                &mut stdout,
                &pp,
                &output_options,
            )?;
        }
    } else {
        for path in options.files {
            let run_file = |reader, vars, output: &mut dyn Write| {
                if options.raw_input {
                    execute(
                        &runner,
                        &filter,
                        vars,
                        raw_lines(reader),
                        output,
                        &pp,
                        &output_options,
                    )
                } else {
                    execute(
                        &runner,
                        &filter,
                        vars,
                        json::read::read_many(reader),
                        output,
                        &pp,
                        &output_options,
                    )
                }
            };
            if path.as_os_str() == "-" {
                last = run_file(
                    Box::new(BufReader::new(io::stdin())) as Box<dyn BufRead>,
                    Vars::new(variable_values.clone()),
                    &mut stdout,
                )?;
            } else {
                let file = File::open(&path).map_err(|error| {
                    RunError::Io(io::Error::new(
                        error.kind(),
                        format!("{}: {error}", path.display()),
                    ))
                })?;
                last = run_file(
                    Box::new(BufReader::new(file)) as Box<dyn BufRead>,
                    Vars::new(variable_values.clone()),
                    &mut stdout,
                )?;
            }
        }
    }

    if options.exit_status {
        Ok(match last {
            Some(true) => 0,
            Some(false) => 1,
            None => 4,
        })
    } else {
        Ok(0)
    }
}

fn collect_inputs<E: ToString>(
    values: &mut Vec<Val>,
    inputs: impl Iterator<Item = Result<Val, E>>,
) -> Result<(), RunError> {
    for value in inputs {
        values.push(value.map_err(|error| RunError::Input(error.to_string()))?);
    }
    Ok(())
}

fn raw_lines(reader: impl BufRead) -> impl Iterator<Item = Result<Val, io::Error>> {
    reader.lines().map(|line| line.map(Val::utf8_str))
}

struct OutputOptions {
    raw_output: bool,
    join_output: bool,
}

fn execute<E: ToString>(
    runner: &Runner,
    filter: &data::Filter,
    vars: Vars<Val>,
    inputs: impl Iterator<Item = Result<Val, E>>,
    output: &mut dyn Write,
    pp: &json::write::Pp,
    output_options: &OutputOptions,
) -> Result<Option<bool>, RunError> {
    let mut last = None;
    data::run(runner, filter, vars, inputs, RunError::Input, |value| {
        let value = value.map_err(map_exception)?;
        last = Some(value.as_bool());
        if output_options.raw_output {
            if let Val::TStr(text) = &value {
                output.write_all(text).map_err(RunError::Io)?;
            } else {
                json::write::write(output, pp, 0, &value).map_err(RunError::Io)?;
            }
        } else {
            json::write::write(output, pp, 0, &value).map_err(RunError::Io)?;
        }
        if output_options.join_output {
            Ok(())
        } else {
            output.write_all(b"\n").map_err(RunError::Io)
        }
    })?;
    Ok(last)
}

fn map_exception(exception: Exn<'_, Val>) -> RunError {
    match exception.get_err() {
        Ok(error) => RunError::Filter(error.to_string()),
        Err(exception) => match exception.get_halt() {
            Ok(code) => RunError::Halt(code),
            Err(_) => RunError::Filter("unexpected filter control flow".to_string()),
        },
    }
}
