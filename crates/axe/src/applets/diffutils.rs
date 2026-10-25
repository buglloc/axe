use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use diffutilslib::params::{self, Format};
use regex::bytes::Regex;
use similar::{Algorithm, DiffTag, capture_diff_slices};
use unicode_width::UnicodeWidthChar;

#[derive(Default)]
struct DiffOptions {
    params: params::Params,
    recursive: bool,
    new_file: bool,
    text: bool,
    ignore_case: bool,
    ignore_all_space: bool,
    ignore_space_change: bool,
    ignore_blank_lines: bool,
    ignore_matching: Vec<Regex>,
    labels: Vec<OsString>,
    command_options: Vec<OsString>,
}

pub fn diff(args: Vec<OsString>) -> i32 {
    let mut options = match parse_diff(args) {
        Ok(Some(options)) => options,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("diff: {error}");
            return 2;
        }
    };
    let from = Path::new(&options.params.from);
    let to = Path::new(&options.params.to);
    let from_metadata = fs::metadata(from);
    let to_metadata = fs::metadata(to);
    let from_directory = from_metadata.as_ref().is_ok_and(fs::Metadata::is_dir);
    let to_directory = to_metadata.as_ref().is_ok_and(fs::Metadata::is_dir);
    let from_missing = options.new_file
        && from != Path::new("-")
        && from_metadata
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
    let to_missing = options.new_file
        && to != Path::new("-")
        && to_metadata
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
    if (from_directory && (to_directory || to_missing)) || (to_directory && from_missing) {
        return directory_diff(&options, from, to, &mut BTreeSet::new());
    }
    if from_directory && let Some(name) = to.file_name() {
        options.params.from = from.join(name).into_os_string();
    } else if to_directory && let Some(name) = from.file_name() {
        options.params.to = to.join(name).into_os_string();
    }
    match compare_diff_files(&options, &options.params.from, &options.params.to) {
        Ok((output, status, _)) => write_output("diff", &output, status),
        Err(error) => {
            eprintln!("diff: {error}");
            2
        }
    }
}

fn parse_diff(args: Vec<OsString>) -> Result<Option<DiffOptions>, String> {
    let mut options = DiffOptions::default();
    let mut files = Vec::new();
    let mut format = None;
    let mut iter = args.into_iter();
    options.params.executable = iter.next().unwrap_or_else(|| "diff".into());
    let mut operands = false;
    while let Some(argument) = iter.next() {
        if operands || argument == "-" || !argument.as_encoded_bytes().starts_with(b"-") {
            files.push(argument);
            continue;
        }
        if argument == "--" {
            operands = true;
            continue;
        }
        let text = argument.to_str().ok_or("invalid option encoding")?;
        if informational_option(text, "diff", "Usage: diff [OPTION]... FILE1 FILE2") {
            return Ok(None);
        }
        options.command_options.push(argument.clone());
        if let Some(long) = text.strip_prefix("--") {
            let (name, attached) = long
                .split_once('=')
                .map_or((long, None), |(name, value)| (name, Some(value)));
            let takes_value = matches!(
                name,
                "label" | "width" | "tabsize" | "ignore-matching-lines"
            );
            let value = if takes_value {
                Some(match attached {
                    Some(value) => OsString::from(value),
                    None => {
                        let value = iter
                            .next()
                            .ok_or_else(|| format!("option '--{name}' requires an argument"))?;
                        options.command_options.push(value.clone());
                        value
                    }
                })
            } else {
                None
            };
            if attached.is_some() && !takes_value && !matches!(name, "unified" | "context") {
                return Err(format!("option '--{name}' doesn't allow an argument"));
            }
            match name {
                "recursive" => options.recursive = true,
                "new-file" => options.new_file = true,
                "text" => options.text = true,
                "binary" => {}
                "ignore-case" => options.ignore_case = true,
                "ignore-all-space" => options.ignore_all_space = true,
                "ignore-space-change" => options.ignore_space_change = true,
                "ignore-blank-lines" => options.ignore_blank_lines = true,
                "ignore-matching-lines" => options
                    .ignore_matching
                    .push(ignore_regex(value.as_deref().unwrap())?),
                "label" => options.labels.push(value.unwrap()),
                "width" => {
                    options.params.width = positive_number(value.as_deref().unwrap(), "width")?
                }
                "tabsize" => {
                    options.params.tabsize = positive_number(value.as_deref().unwrap(), "tabsize")?
                }
                "report-identical-files" => options.params.report_identical_files = true,
                "brief" => options.params.brief = true,
                "expand-tabs" => options.params.expand_tabs = true,
                "normal" => set_format(&mut format, Format::Normal)?,
                "ed" => set_format(&mut format, Format::Ed)?,
                "side-by-side" => set_format(&mut format, Format::SideBySide)?,
                "unified" | "context" => {
                    set_format(
                        &mut format,
                        if name == "unified" {
                            Format::Unified
                        } else {
                            Format::Context
                        },
                    )?;
                    if let Some(value) = attached {
                        options.params.context_count = number(OsStr::new(value), "context length")?;
                    }
                }
                _ => return Err(format!("unrecognized option '{text}'")),
            }
        } else {
            if text.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                && matches!(text.as_bytes().last(), Some(b'u' | b'c'))
            {
                let style = if text.ends_with('u') {
                    Format::Unified
                } else {
                    Format::Context
                };
                set_format(&mut format, style)?;
                options.params.context_count =
                    number(OsStr::new(&text[1..text.len() - 1]), "context length")?;
                continue;
            }
            for (offset, flag) in text[1..].char_indices() {
                match flag {
                    'r' => options.recursive = true,
                    'N' => options.new_file = true,
                    'a' => options.text = true,
                    'i' => options.ignore_case = true,
                    'w' => options.ignore_all_space = true,
                    'b' => options.ignore_space_change = true,
                    'B' => options.ignore_blank_lines = true,
                    's' => options.params.report_identical_files = true,
                    'q' => options.params.brief = true,
                    't' => options.params.expand_tabs = true,
                    'u' | 'c' => {
                        set_format(
                            &mut format,
                            if flag == 'u' {
                                Format::Unified
                            } else {
                                Format::Context
                            },
                        )?;
                        let rest = &text[offset + 2..];
                        if !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()) {
                            options.params.context_count =
                                number(OsStr::new(rest), "context length")?;
                            break;
                        }
                    }
                    'e' => set_format(&mut format, Format::Ed)?,
                    'y' => set_format(&mut format, Format::SideBySide)?,
                    'U' | 'C' | 'W' | 'I' | 'L' => {
                        let remaining = &text[offset + 2..];
                        let value = if remaining.is_empty() {
                            let value = iter.next().ok_or_else(|| {
                                format!("option requires an argument -- '{flag}'")
                            })?;
                            options.command_options.push(value.clone());
                            value
                        } else {
                            OsString::from(remaining)
                        };
                        match flag {
                            'U' | 'C' => {
                                set_format(
                                    &mut format,
                                    if flag == 'U' {
                                        Format::Unified
                                    } else {
                                        Format::Context
                                    },
                                )?;
                                options.params.context_count = number(&value, "context length")?;
                            }
                            'W' => options.params.width = positive_number(&value, "width")?,
                            'I' => options.ignore_matching.push(ignore_regex(&value)?),
                            'L' => options.labels.push(value),
                            _ => unreachable!(),
                        }
                        break;
                    }
                    _ => return Err(format!("invalid option -- '{flag}'")),
                }
            }
        }
    }
    if files.len() != 2 {
        return Err("exactly two input files are required".into());
    }
    if options.labels.len() > 2 {
        return Err("too many file labels".into());
    }
    options.params.format = format.unwrap_or(Format::Normal);
    options.params.to = files.pop().unwrap();
    options.params.from = files.pop().unwrap();
    Ok(Some(options))
}

fn set_format(format: &mut Option<Format>, value: Format) -> Result<(), String> {
    if format.is_some_and(|previous| previous != value) {
        return Err("conflicting output style options".into());
    }
    *format = Some(value);
    Ok(())
}

fn number(value: &OsStr, name: &str) -> Result<usize, String> {
    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("invalid {name} '{}'", value.to_string_lossy()))
}

fn positive_number(value: &OsStr, name: &str) -> Result<usize, String> {
    let number = number(value, name)?;
    if number == 0 || number > isize::MAX as usize - 3 {
        return Err(format!("invalid {name} '{}'", value.to_string_lossy()));
    }
    Ok(number)
}

fn ignore_regex(pattern: &OsStr) -> Result<Regex, String> {
    let pattern = pattern
        .to_str()
        .ok_or("invalid regular expression encoding")?;
    // diff -I uses basic regular expressions; Rust's engine uses extended syntax.
    let mut extended = String::new();
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                let next = characters
                    .next()
                    .ok_or("invalid regular expression: trailing backslash")?;
                if !matches!(next, '(' | ')' | '+' | '?' | '{' | '}' | '|') {
                    extended.push('\\');
                }
                extended.push(next);
            }
            '[' => translate_bre_bracket(&mut characters, &mut extended)?,
            _ => {
                if matches!(character, '(' | ')' | '+' | '?' | '{' | '}' | '|') {
                    extended.push('\\');
                }
                extended.push(character);
            }
        }
    }
    Regex::new(&format!("(?m-u:{extended})"))
        .map_err(|error| format!("invalid regular expression: {error}"))
}

fn translate_bre_bracket(
    characters: &mut std::iter::Peekable<std::str::Chars<'_>>,
    extended: &mut String,
) -> Result<(), String> {
    extended.push('[');
    if characters.peek() == Some(&'^') {
        characters.next();
        extended.push('^');
    }
    let mut first = true;
    while let Some(character) = characters.next() {
        match character {
            ']' if !first => {
                extended.push(']');
                return Ok(());
            }
            ']' => extended.push_str(r"\]"),
            '\\' => extended.push_str(r"\\"),
            '&' | '~' => {
                extended.push('\\');
                extended.push(character);
            }
            '[' if characters.peek() == Some(&':') => {
                characters.next();
                extended.push_str("[:");
                loop {
                    let character = characters
                        .next()
                        .ok_or("invalid regular expression: unterminated character class")?;
                    extended.push(character);
                    if character == ':' && characters.peek() == Some(&']') {
                        characters.next();
                        extended.push(']');
                        break;
                    }
                }
            }
            '[' if matches!(characters.peek(), Some('.' | '=')) => {
                return Err(
                    "unsupported regular expression: collating and equivalence classes".into(),
                );
            }
            '[' => extended.push_str(r"\["),
            '-' if characters.peek() == Some(&'-') => {
                return Err("unsupported regular expression: consecutive range operators".into());
            }
            _ => extended.push(character),
        }
        first = false;
    }
    Err("invalid regular expression: unterminated bracket expression".into())
}

fn read_diff_operand(path: &OsStr, new_file: bool) -> Result<Vec<u8>, String> {
    match read_operand(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if new_file && error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("{}: {error}", path.to_string_lossy())),
    }
}

#[derive(Debug)]
struct Change {
    old: Range<usize>,
    new: Range<usize>,
    ignored: bool,
}

fn comparison_key(line: &[u8], options: &DiffOptions) -> Vec<u8> {
    let newline =
        line.ends_with(b"\n") && !options.ignore_all_space && !options.ignore_space_change;
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let mut key = Vec::with_capacity(line.len() + usize::from(newline));
    let mut space = false;
    for &byte in line {
        if byte.is_ascii_whitespace() && (options.ignore_all_space || options.ignore_space_change) {
            space = true;
            continue;
        }
        if space && options.ignore_space_change && !options.ignore_all_space {
            key.push(b' ');
        }
        space = false;
        key.push(if options.ignore_case {
            byte.to_ascii_lowercase()
        } else {
            byte
        });
    }
    if newline {
        key.push(b'\n');
    }
    key
}

fn diff_changes(from: &[&[u8]], to: &[&[u8]], options: &DiffOptions) -> Vec<Change> {
    let operations =
        if options.ignore_case || options.ignore_all_space || options.ignore_space_change {
            let left = from
                .iter()
                .map(|line| comparison_key(line, options))
                .collect::<Vec<_>>();
            let right = to
                .iter()
                .map(|line| comparison_key(line, options))
                .collect::<Vec<_>>();
            capture_diff_slices(Algorithm::Myers, &left, &right)
        } else {
            capture_diff_slices(Algorithm::Myers, from, to)
        };
    operations
        .into_iter()
        .filter_map(|operation| {
            let (tag, old, new) = operation.as_tag_tuple();
            if tag == DiffTag::Equal {
                return None;
            }
            let ignored = from[old.clone()]
                .iter()
                .chain(&to[new.clone()])
                .all(|line| {
                    let blank = options.ignore_blank_lines
                        && if options.ignore_all_space || options.ignore_space_change {
                            line.iter().all(u8::is_ascii_whitespace)
                        } else {
                            *line == b"\n"
                        };
                    blank
                        || options.ignore_matching.iter().any(|pattern| {
                            pattern.is_match(line.strip_suffix(b"\n").unwrap_or(line))
                        })
                });
            Some(Change { old, new, ignored })
        })
        .collect()
}

fn compare_diff_files(
    options: &DiffOptions,
    from_name: &OsStr,
    to_name: &OsStr,
) -> Result<(Vec<u8>, i32, bool), String> {
    let from = read_diff_operand(from_name, options.new_file)?;
    let to_bytes = if from_name == "-" && to_name == "-" {
        None
    } else {
        Some(read_diff_operand(to_name, options.new_file)?)
    };
    let to = to_bytes.as_deref().unwrap_or(&from);
    let params = &options.params;
    let mut output = Vec::new();
    if from.as_slice() != to && !options.text && (from.contains(&0) || to.contains(&0)) {
        output.extend_from_slice(if params.brief {
            b"Files "
        } else {
            b"Binary files "
        });
        append_names(&mut output, from_name, to_name, b" differ\n");
        return Ok((output, 1, true));
    }
    let from_lines = split_lines(&from);
    let to_lines = split_lines(to);
    let changes = diff_changes(&from_lines, &to_lines, options);
    let different = changes.iter().any(|change| !change.ignored);
    if !different && params.report_identical_files {
        output.extend_from_slice(b"Files ");
        append_names(&mut output, from_name, to_name, b" are identical\n");
    }
    if params.brief {
        if different {
            output.extend_from_slice(b"Files ");
            append_names(&mut output, from_name, to_name, b" differ\n");
        }
    } else if params.format == Format::SideBySide {
        render_side_by_side(&mut output, &from_lines, &to_lines, &changes, params);
    } else if different {
        match params.format {
            Format::Normal => render_normal(&mut output, &from_lines, &to_lines, &changes, params),
            Format::Unified | Format::Context => {
                render_headers(&mut output, options, from_name, to_name);
                render_contextual(&mut output, &from_lines, &to_lines, &changes, params);
            }
            Format::Ed => render_ed(&mut output, &from_lines, &to_lines, &changes)?,
            Format::SideBySide => unreachable!(),
        }
    }
    Ok((output, i32::from(different), false))
}

fn append_names(output: &mut Vec<u8>, from: &OsStr, to: &OsStr, suffix: &[u8]) {
    output.extend_from_slice(from.as_encoded_bytes());
    output.extend_from_slice(b" and ");
    output.extend_from_slice(to.as_encoded_bytes());
    output.extend_from_slice(suffix);
}

fn line_range(range: &Range<usize>) -> String {
    match range.len() {
        0 => range.start.to_string(),
        1 => range.end.to_string(),
        _ => format!("{},{}", range.start + 1, range.end),
    }
}

fn append_diff_line(output: &mut Vec<u8>, prefix: &[u8], line: &[u8], params: &params::Params) {
    output.extend_from_slice(prefix);
    let content = line.strip_suffix(b"\n").unwrap_or(line);
    let mut column = 0;
    for &byte in content {
        if byte == b'\t' && params.expand_tabs {
            let count = params.tabsize - column % params.tabsize;
            output.resize(output.len() + count, b' ');
            column += count;
        } else {
            output.push(byte);
            column += 1;
        }
    }
    output.push(b'\n');
    if !line.ends_with(b"\n") {
        output.extend_from_slice(b"\\ No newline at end of file\n");
    }
}

fn render_normal(
    output: &mut Vec<u8>,
    from: &[&[u8]],
    to: &[&[u8]],
    changes: &[Change],
    params: &params::Params,
) {
    for change in changes.iter().filter(|change| !change.ignored) {
        let command = if change.old.is_empty() {
            'a'
        } else if change.new.is_empty() {
            'd'
        } else {
            'c'
        };
        writeln!(
            output,
            "{}{command}{}",
            line_range(&change.old),
            line_range(&change.new)
        )
        .unwrap();
        for line in &from[change.old.clone()] {
            append_diff_line(output, b"< ", line, params);
        }
        if command == 'c' {
            output.extend_from_slice(b"---\n");
        }
        for line in &to[change.new.clone()] {
            append_diff_line(output, b"> ", line, params);
        }
    }
}

fn render_headers(output: &mut Vec<u8>, options: &DiffOptions, from: &OsStr, to: &OsStr) {
    let context = options.params.format == Format::Context;
    for (index, (name, prefix)) in [
        (from, if context { b"*** " } else { b"--- " }),
        (to, if context { b"--- " } else { b"+++ " }),
    ]
    .into_iter()
    .enumerate()
    {
        output.extend_from_slice(prefix);
        if let Some(label) = options.labels.get(index) {
            output.extend_from_slice(label.as_encoded_bytes());
        } else {
            output.extend_from_slice(name.as_encoded_bytes());
            output.push(b'\t');
            let modified = fs::metadata(name)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            let date: DateTime<Local> = modified.into();
            write!(
                output,
                "{}",
                date.format(if context {
                    "%a %b %e %T %Y"
                } else {
                    "%Y-%m-%d %H:%M:%S%.9f %z"
                })
            )
            .unwrap();
        }
        output.push(b'\n');
    }
}

fn unified_range(range: &Range<usize>) -> String {
    if range.len() == 1 {
        range.end.to_string()
    } else {
        format!(
            "{},{}",
            range.start + usize::from(!range.is_empty()),
            range.len()
        )
    }
}

fn render_contextual(
    output: &mut Vec<u8>,
    from: &[&[u8]],
    to: &[&[u8]],
    changes: &[Change],
    params: &params::Params,
) {
    let context = params.context_count;
    let mut index = 0;
    while index < changes.len() {
        let first = index;
        let mut end = index + 1;
        while end < changes.len()
            && changes[end]
                .old
                .start
                .saturating_sub(changes[end - 1].old.end)
                <= context.saturating_mul(2)
        {
            end += 1;
        }
        index = end;
        let changes = &changes[first..end];
        if changes.iter().all(|change| change.ignored) {
            continue;
        }
        let before = context.min(changes[0].old.start).min(changes[0].new.start);
        let last = changes.last().unwrap();
        let after = context
            .min(from.len() - last.old.end)
            .min(to.len() - last.new.end);
        let old = changes[0].old.start - before..last.old.end + after;
        let new = changes[0].new.start - before..last.new.end + after;
        if params.format == Format::Unified {
            writeln!(
                output,
                "@@ -{} +{} @@",
                unified_range(&old),
                unified_range(&new)
            )
            .unwrap();
            let mut cursor = old.start;
            for change in changes {
                for line in &from[cursor..change.old.start] {
                    append_diff_line(output, b" ", line, params);
                }
                for line in &from[change.old.clone()] {
                    append_diff_line(output, b"-", line, params);
                }
                for line in &to[change.new.clone()] {
                    append_diff_line(output, b"+", line, params);
                }
                cursor = change.old.end;
            }
            for line in &from[cursor..old.end] {
                append_diff_line(output, b" ", line, params);
            }
        } else {
            output.extend_from_slice(b"***************\n");
            for (right, lines, range, prefix, suffix) in [
                (false, from, old, "***", "****"),
                (true, to, new, "---", "----"),
            ] {
                writeln!(output, "{prefix} {} {suffix}", line_range(&range)).unwrap();
                if changes.iter().all(|change| {
                    if right {
                        change.new.is_empty()
                    } else {
                        change.old.is_empty()
                    }
                }) {
                    continue;
                }
                let mut cursor = range.start;
                for change in changes {
                    let changed = if right { &change.new } else { &change.old };
                    for line in &lines[cursor..changed.start] {
                        append_diff_line(output, b"  ", line, params);
                    }
                    let marker = if !change.old.is_empty() && !change.new.is_empty() {
                        b"! "
                    } else if right {
                        b"+ "
                    } else {
                        b"- "
                    };
                    for line in &lines[changed.clone()] {
                        append_diff_line(output, marker, line, params);
                    }
                    cursor = changed.end;
                }
                for line in &lines[cursor..range.end] {
                    append_diff_line(output, b"  ", line, params);
                }
            }
        }
    }
}

fn render_ed(
    output: &mut Vec<u8>,
    from: &[&[u8]],
    to: &[&[u8]],
    changes: &[Change],
) -> Result<(), String> {
    if changes.iter().all(|change| change.ignored) {
        return Ok(());
    }
    if from.last().is_some_and(|line| !line.ends_with(b"\n"))
        || to.last().is_some_and(|line| !line.ends_with(b"\n"))
    {
        return Err("No newline at end of file".into());
    }
    for change in changes.iter().rev().filter(|change| !change.ignored) {
        let command = if change.old.is_empty() {
            'a'
        } else if change.new.is_empty() {
            'd'
        } else {
            'c'
        };
        writeln!(output, "{}{command}", line_range(&change.old)).unwrap();
        if command != 'd' {
            for line in &to[change.new.clone()] {
                append_ed_line(output, line);
            }
            output.extend_from_slice(b".\n");
        }
    }
    Ok(())
}

fn append_ed_line(output: &mut Vec<u8>, line: &[u8]) {
    let content = line.strip_suffix(b"\n").unwrap_or(line);
    if content == b"." {
        output.extend_from_slice(b"..\n.\ns/.//\na\n");
    } else {
        output.extend_from_slice(content);
        output.push(b'\n');
    }
}

fn pad_columns(output: &mut Vec<u8>, mut column: usize, end: usize, params: &params::Params) {
    while column < end {
        let next = column + params.tabsize - column % params.tabsize;
        if !params.expand_tabs && next <= end {
            output.push(b'\t');
            column = next;
        } else {
            output.push(b' ');
            column += 1;
        }
    }
}

fn side_half(output: &mut Vec<u8>, line: &[u8], width: usize, params: &params::Params) -> usize {
    let mut column = 0;
    for chunk in line.strip_suffix(b"\n").unwrap_or(line).utf8_chunks() {
        for character in chunk.valid().chars() {
            if character == '\t' {
                let next = column + params.tabsize - column % params.tabsize;
                if next <= width {
                    if params.expand_tabs {
                        output.resize(output.len() + next - column, b' ');
                    } else {
                        output.push(b'\t');
                    }
                    column = next;
                }
            } else {
                let size = character.width().unwrap_or(0);
                if column + size > width {
                    return column;
                }
                let mut buffer = [0; 4];
                output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                column += size;
            }
        }
        for &byte in chunk.invalid() {
            if column == width {
                return column;
            }
            output.push(byte);
            column += 1;
        }
    }
    column
}

fn side_row(
    output: &mut Vec<u8>,
    left: Option<&[u8]>,
    right: Option<&[u8]>,
    mut marker: u8,
    params: &params::Params,
) {
    let unaligned = (params.width / 2)
        .saturating_add((params.tabsize + 3) / 2)
        .saturating_add(params.width & (params.tabsize + 3) & 1);
    let offset = unaligned - unaligned % params.tabsize;
    let width = offset
        .saturating_sub(3)
        .min(params.width.saturating_sub(offset));
    let offset = if width == 0 { params.width } else { offset };
    let separator = (width + offset).saturating_sub(1) / 2;
    let column = left.map_or(0, |line| side_half(output, line, width, params));
    if marker == b' '
        && left.is_some_and(|line| !line.ends_with(b"\n"))
            != right.is_some_and(|line| !line.ends_with(b"\n"))
    {
        marker = if left.is_some_and(|line| line.ends_with(b"\n")) {
            b')'
        } else {
            b'('
        };
    }
    if marker != b' ' {
        pad_columns(output, column, separator, params);
        output.push(marker);
        if right.is_some() {
            pad_columns(output, separator + 1, offset, params);
        }
    } else if right.is_some_and(|line| line != b"\n") {
        pad_columns(output, column, offset, params);
    }
    if let Some(right) = right {
        side_half(output, right, width, params);
    }
    output.push(b'\n');
}

fn render_side_by_side(
    output: &mut Vec<u8>,
    from: &[&[u8]],
    to: &[&[u8]],
    changes: &[Change],
    params: &params::Params,
) {
    let mut old = 0;
    let mut new = 0;
    for change in changes {
        while old < change.old.start && new < change.new.start {
            side_row(output, Some(from[old]), Some(to[new]), b' ', params);
            old += 1;
            new += 1;
        }
        let count = change.old.len().max(change.new.len());
        for index in 0..count {
            let left = from[change.old.clone()].get(index).copied();
            let right = to[change.new.clone()].get(index).copied();
            let marker = if change.ignored {
                b' '
            } else {
                match (left, right) {
                    (Some(_), Some(_)) => b'|',
                    (Some(_), None) => b'<',
                    _ => b'>',
                }
            };
            side_row(output, left, right, marker, params);
        }
        old = change.old.end;
        new = change.new.end;
    }
    while old < from.len() && new < to.len() {
        side_row(output, Some(from[old]), Some(to[new]), b' ', params);
        old += 1;
        new += 1;
    }
}

fn directory_diff(
    options: &DiffOptions,
    left_root: &Path,
    right_root: &Path,
    ancestors: &mut BTreeSet<(PathBuf, PathBuf)>,
) -> i32 {
    let identity = (
        fs::canonicalize(left_root).unwrap_or_else(|_| left_root.to_path_buf()),
        fs::canonicalize(right_root).unwrap_or_else(|_| right_root.to_path_buf()),
    );
    if !ancestors.insert(identity.clone()) {
        eprintln!("diff: recursive directory loop");
        return 2;
    }
    let result = compare_directory(options, left_root, right_root, ancestors);
    ancestors.remove(&identity);
    result
}

fn compare_directory(
    options: &DiffOptions,
    left_root: &Path,
    right_root: &Path,
    ancestors: &mut BTreeSet<(PathBuf, PathBuf)>,
) -> i32 {
    let mut names = BTreeSet::new();
    for root in [left_root, right_root] {
        match fs::read_dir(root) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            names.insert(entry.file_name());
                        }
                        Err(error) => {
                            eprintln!("diff: {}: {error}", root.display());
                            return 2;
                        }
                    }
                }
            }
            Err(error) if options.new_file && error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("diff: {}: {error}", root.display());
                return 2;
            }
        }
    }
    let mut status = 0;
    for name in names {
        let left = left_root.join(&name);
        let right = right_root.join(&name);
        let left_exists = left.exists();
        let right_exists = right.exists();
        if (!left_exists || !right_exists) && !options.new_file {
            let root = if left_exists { left_root } else { right_root };
            let mut output = b"Only in ".to_vec();
            output.extend_from_slice(root.as_os_str().as_encoded_bytes());
            output.extend_from_slice(b": ");
            output.extend_from_slice(name.as_encoded_bytes());
            output.push(b'\n');
            status = status.max(write_output("diff", &output, 1));
            continue;
        }
        let left_dir = left.is_dir();
        let right_dir = right.is_dir();
        if left_dir || right_dir {
            if (left_exists && !left_dir) || (right_exists && !right_dir) {
                let (directory, file) = if left_dir {
                    (&left, &right)
                } else {
                    (&right, &left)
                };
                println!(
                    "File {} is a directory while file {} is a regular file",
                    directory.display(),
                    file.display()
                );
                status = status.max(1);
            } else if options.recursive {
                status = status.max(directory_diff(options, &left, &right, ancestors));
            } else if left_exists && right_exists {
                println!(
                    "Common subdirectories: {} and {}",
                    left.display(),
                    right.display()
                );
            } else {
                println!(
                    "Only in {}: {}",
                    if left_exists { left_root } else { right_root }.display(),
                    name.to_string_lossy()
                );
                status = status.max(1);
            }
            continue;
        }
        match compare_diff_files(options, left.as_os_str(), right.as_os_str()) {
            Ok((output, result, binary)) => {
                if !output.is_empty()
                    && !options.params.brief
                    && !(result == 0 && options.params.report_identical_files)
                    && !binary
                {
                    let mut header = b"diff".to_vec();
                    for option in &options.command_options {
                        header.push(b' ');
                        header.extend_from_slice(option.as_encoded_bytes());
                    }
                    header.push(b' ');
                    header.extend_from_slice(left.as_os_str().as_encoded_bytes());
                    header.push(b' ');
                    header.extend_from_slice(right.as_os_str().as_encoded_bytes());
                    header.push(b'\n');
                    status = status.max(write_output("diff", &header, 0));
                }
                status = status.max(write_output("diff", &output, result));
            }
            Err(error) => {
                eprintln!("diff: {error}");
                status = 2;
            }
        }
    }
    status
}

fn write_output(program: &str, output: &[u8], status: i32) -> i32 {
    match io::stdout().lock().write_all(output) {
        Ok(()) => status,
        Err(error) => {
            eprintln!("{program}: {error}");
            2
        }
    }
}

pub fn cmp(args: Vec<OsString>) -> i32 {
    let args = match parse_cmp(args) {
        Ok(Some(args)) => args,
        Ok(None) => return 0,
        Err(error) => {
            eprintln!("cmp: {error}");
            return 2;
        }
    };
    let params = match diffutilslib::cmp::parse_params(args.into_iter().peekable()) {
        Ok(params) => params,
        Err(error) => {
            eprintln!("cmp: {}", error.strip_prefix("cmp: ").unwrap_or(&error));
            return 2;
        }
    };

    match diffutilslib::cmp::cmp(&params) {
        Ok(diffutilslib::cmp::Cmp::Equal) => 0,
        Ok(diffutilslib::cmp::Cmp::Different) => 1,
        Err(error) => {
            eprintln!("cmp: {error}");
            2
        }
    }
}

fn parse_cmp(args: Vec<OsString>) -> Result<Option<Vec<OsString>>, String> {
    let mut iter = args.into_iter();
    let mut parsed = vec![iter.next().unwrap_or_else(|| "cmp".into())];
    let mut files = Vec::new();
    let mut operands = false;
    let mut ignore_initial = false;
    while let Some(argument) = iter.next() {
        if operands || argument == "-" || !argument.as_encoded_bytes().starts_with(b"-") {
            files.push(argument);
            continue;
        }
        if argument == "--" {
            operands = true;
            continue;
        }
        let text = argument.to_str().ok_or("invalid option encoding")?;
        if informational_option(
            text,
            "cmp",
            "Usage: cmp [OPTION]... FILE1 [FILE2 [SKIP1 [SKIP2]]]",
        ) {
            return Ok(None);
        }
        if text.starts_with("--") {
            let (name, attached) = text
                .split_once('=')
                .map_or((text, None), |(name, value)| (name, Some(value)));
            if matches!(name, "--bytes" | "--ignore-initial") {
                let value = match attached {
                    Some(value) => OsString::from(value),
                    None => iter
                        .next()
                        .ok_or_else(|| format!("option '{name}' requires an argument"))?,
                };
                if name == "--ignore-initial" {
                    ignore_initial = true;
                }
                parsed.push(if name == "--bytes" { "-n" } else { "-i" }.into());
                parsed.push(cmp_quantity(&value, name == "--ignore-initial")?);
            } else {
                parsed.push(argument);
            }
        } else {
            for (offset, flag) in text[1..].char_indices() {
                if matches!(flag, 'n' | 'i') {
                    let rest = &text[offset + 2..];
                    let value = if rest.is_empty() {
                        iter.next()
                            .ok_or_else(|| format!("option requires an argument -- '{flag}'"))?
                    } else {
                        OsString::from(rest)
                    };
                    ignore_initial |= flag == 'i';
                    parsed.push(format!("-{flag}").into());
                    parsed.push(cmp_quantity(&value, flag == 'i')?);
                    break;
                }
                if !matches!(flag, 'b' | 'l' | 's') {
                    return Err(format!("invalid option -- '{flag}'"));
                }
                parsed.push(format!("-{flag}").into());
            }
        }
    }
    if files.is_empty() || files.len() > 4 {
        return Err("expected FILE1 [FILE2 [SKIP1 [SKIP2]]]".into());
    }
    if !ignore_initial {
        for skip in files.iter_mut().skip(2) {
            *skip = gnu_quantity(skip)?.to_string().into();
        }
    }
    parsed.push("--".into());
    parsed.extend(files);
    Ok(Some(parsed))
}

fn cmp_quantity(value: &OsStr, pair: bool) -> Result<OsString, String> {
    if pair && let Some((left, right)) = value.to_str().and_then(|value| value.split_once(':')) {
        return Ok(format!(
            "{}:{}",
            gnu_quantity(OsStr::new(left))?,
            gnu_quantity(OsStr::new(right))?
        )
        .into());
    }
    Ok(gnu_quantity(value)?.to_string().into())
}

fn gnu_quantity(value: &OsStr) -> Result<u64, String> {
    let invalid = || format!("invalid byte count '{}'", value.to_string_lossy());
    let text = value.to_str().ok_or_else(invalid)?;
    let text = text.strip_prefix('+').unwrap_or(text);
    let (digits, radix) = if text.starts_with("0x") || text.starts_with("0X") {
        (&text[2..], 16)
    } else {
        (text, if text.starts_with('0') { 8 } else { 10 })
    };
    let length = digits
        .find(|character: char| !character.is_digit(radix))
        .unwrap_or(digits.len());
    let mut number = u64::from_str_radix(&digits[..length], radix).map_err(|_| invalid())?;
    let suffix = &digits[length..];
    if suffix.is_empty() {
        return Ok(number);
    }
    if suffix == "b" {
        return number.checked_mul(512).ok_or_else(invalid);
    }
    let suffix_bytes = suffix.as_bytes();
    let power = match suffix_bytes[0] {
        b'k' | b'K' => 1,
        b'M' => 2,
        b'G' => 3,
        b'T' => 4,
        b'P' => 5,
        b'E' => 6,
        b'Z' => 7,
        b'Y' => 8,
        _ => return Err(invalid()),
    };
    let base = match &suffix[1..] {
        "" | "iB" => 1024,
        "B" => 1000,
        _ => return Err(invalid()),
    };
    for _ in 0..power {
        number = number.checked_mul(base).ok_or_else(invalid)?;
    }
    Ok(number)
}

pub fn diff3(args: Vec<OsString>) -> i32 {
    let mut files = Vec::new();
    let mut labels = Vec::new();
    let mut merge = false;
    let mut ed = false;
    let mut show_all = false;
    let mut text = false;
    let mut operands = false;
    let mut iter = args.into_iter().skip(1);
    while let Some(arg) = iter.next() {
        if operands || arg == "-" || !arg.as_encoded_bytes().starts_with(b"-") {
            files.push(arg);
            continue;
        }
        match arg.to_str() {
            Some("--") => operands = true,
            Some(option)
                if informational_option(
                    option,
                    "diff3",
                    "Usage: diff3 [OPTION]... MYFILE OLDFILE YOURFILE",
                ) =>
            {
                return 0;
            }
            Some("-m" | "--merge") => merge = true,
            Some("-e" | "--ed") => ed = true,
            Some("-A" | "--show-all") => show_all = true,
            Some("-a" | "--text") => text = true,
            Some("-L" | "--label") => {
                let Some(label) = iter.next() else {
                    eprintln!("diff3: option requires an argument -- L");
                    return 2;
                };
                labels.push(label);
            }
            Some(option) if option.starts_with("--label=") => {
                labels.push(OsString::from(&option["--label=".len()..]))
            }
            Some(option) if option.starts_with("-L") && option.len() > 2 => {
                labels.push(OsString::from(&option[2..]))
            }
            _ => {
                eprintln!("diff3: unsupported option {}", arg.to_string_lossy());
                return 2;
            }
        }
    }
    if files.len() != 3 {
        eprintln!("diff3: three input files are required");
        return 2;
    }
    if labels.len() > 3 {
        eprintln!("diff3: too many file labels");
        return 2;
    }
    if files.iter().filter(|file| *file == "-").count() > 1 {
        eprintln!("diff3: '-' may be specified only once");
        return 2;
    }
    if ed && (merge || show_all) {
        eprintln!("diff3: conflicting output style options");
        return 2;
    }
    let mine = match read_operand(&files[0]) {
        Ok(data) => data,
        Err(error) => return file_error("diff3", &files[0], error),
    };
    let older = match read_operand(&files[1]) {
        Ok(data) => data,
        Err(error) => return file_error("diff3", &files[1], error),
    };
    let yours = match read_operand(&files[2]) {
        Ok(data) => data,
        Err(error) => return file_error("diff3", &files[2], error),
    };
    if !text {
        for (left, right) in if merge {
            [(2, 1), (0, 1)]
        } else {
            [(1, 2), (0, 2)]
        } {
            let inputs = [&mine, &older, &yours];
            if inputs[left] != inputs[right]
                && (inputs[left].contains(&0) || inputs[right].contains(&0))
            {
                eprintln!(
                    "diff3: diff failed: Binary files {} and {} differ",
                    files[left].to_string_lossy(),
                    files[right].to_string_lossy()
                );
                return 2;
            }
        }
    }
    let mut names = files;
    for (name, label) in names.iter_mut().zip(labels) {
        *name = label;
    }
    let mine_lines = split_lines(&mine);
    let older_lines = split_lines(&older);
    let yours_lines = split_lines(&yours);
    let mine_edits = line_edits(&older_lines, &mine_lines);
    let yours_edits = line_edits(&older_lines, &yours_lines);

    let (output, conflict) = if merge {
        render_merge(
            &older_lines,
            &mine_edits,
            &yours_edits,
            [&names[0], &names[1], &names[2]],
        )
    } else if show_all {
        let (merged, conflict) = render_merge(
            &older_lines,
            &mine_edits,
            &yours_edits,
            [&names[0], &names[1], &names[2]],
        );
        let merged_lines = split_lines(&merged);
        let options = DiffOptions::default();
        let changes = diff_changes(&mine_lines, &merged_lines, &options);
        let mut output = Vec::new();
        if let Err(error) = render_ed(&mut output, &mine_lines, &merged_lines, &changes) {
            eprintln!("diff3: {error}");
            return 2;
        }
        (output, conflict)
    } else if ed {
        (
            render_diff3_ed(&older_lines, &mine_edits, &yours_edits),
            false,
        )
    } else {
        (
            render_diff3_report(&older_lines, &mine_edits, &yours_edits),
            false,
        )
    };

    write_output("diff3", &output, i32::from(conflict))
}

#[derive(Debug)]
struct LineEdit<'a> {
    old: Range<usize>,
    replacement: Vec<&'a [u8]>,
}

fn split_lines(input: &[u8]) -> Vec<&[u8]> {
    input.split_inclusive(|byte| *byte == b'\n').collect()
}

fn line_edits<'a>(older: &[&[u8]], newer: &'a [&'a [u8]]) -> Vec<LineEdit<'a>> {
    capture_diff_slices(Algorithm::Myers, older, newer)
        .into_iter()
        .filter_map(|operation| {
            let (tag, old, new) = operation.as_tag_tuple();
            (tag != DiffTag::Equal).then(|| LineEdit {
                old,
                replacement: newer[new].to_vec(),
            })
        })
        .collect()
}

fn change_regions(
    mine: &[LineEdit<'_>],
    yours: &[LineEdit<'_>],
    join_adjacent: bool,
) -> Vec<Range<usize>> {
    let mut edits = mine
        .iter()
        .chain(yours)
        .map(|edit| edit.old.clone())
        .collect::<Vec<_>>();
    edits.sort_unstable_by_key(|range| (range.start, range.end));

    let mut regions: Vec<Range<usize>> = Vec::new();
    for edit in edits {
        let Some(region) = regions.last_mut() else {
            regions.push(edit);
            continue;
        };
        let same_insertion =
            edit.start == region.start && (edit.is_empty() || region.start == region.end);
        let touches = edit.start < region.end
            || same_insertion
            || (join_adjacent && edit.start == region.end);
        if touches {
            region.end = region.end.max(edit.end);
        } else {
            regions.push(edit);
        }
    }
    regions
}

fn apply_region<'a>(
    older: &'a [&'a [u8]],
    edits: &[LineEdit<'a>],
    region: &Range<usize>,
) -> Vec<&'a [u8]> {
    let mut output = Vec::new();
    let mut cursor = region.start;
    for edit in edits
        .iter()
        .filter(|edit| edit.old.start >= region.start && edit.old.end <= region.end)
    {
        output.extend_from_slice(&older[cursor..edit.old.start]);
        output.extend_from_slice(&edit.replacement);
        cursor = edit.old.end;
    }
    output.extend_from_slice(&older[cursor..region.end]);
    output
}

fn side_line_index(edits: &[LineEdit<'_>], old_index: usize) -> usize {
    let mut index = old_index;
    for edit in edits {
        let is_before =
            edit.old.end < old_index || (edit.old.end == old_index && !edit.old.is_empty());
        if !is_before {
            break;
        }
        index = index - edit.old.len() + edit.replacement.len();
    }
    index
}

fn append_lines(output: &mut Vec<u8>, lines: &[&[u8]]) {
    for line in lines {
        output.extend_from_slice(line);
    }
}

fn render_merge(
    older: &[&[u8]],
    mine_edits: &[LineEdit<'_>],
    yours_edits: &[LineEdit<'_>],
    names: [&OsString; 3],
) -> (Vec<u8>, bool) {
    let mut output = Vec::new();
    let mut cursor = 0;
    let mut conflict = false;

    for region in change_regions(mine_edits, yours_edits, true) {
        append_lines(&mut output, &older[cursor..region.start]);
        let base = &older[region.clone()];
        let mine = apply_region(older, mine_edits, &region);
        let yours = apply_region(older, yours_edits, &region);
        if mine == yours && mine.as_slice() != base {
            conflict = true;
            output.extend_from_slice(b"<<<<<<< ");
            output.extend_from_slice(names[1].as_encoded_bytes());
            output.push(b'\n');
            append_lines(&mut output, base);
            output.extend_from_slice(b"=======\n");
            append_lines(&mut output, &yours);
            output.extend_from_slice(b">>>>>>> ");
            output.extend_from_slice(names[2].as_encoded_bytes());
            output.push(b'\n');
        } else if mine.as_slice() == base {
            append_lines(&mut output, &yours);
        } else if yours.as_slice() == base {
            append_lines(&mut output, &mine);
        } else {
            conflict = true;
            output.extend_from_slice(b"<<<<<<< ");
            output.extend_from_slice(names[0].as_encoded_bytes());
            output.push(b'\n');
            append_lines(&mut output, &mine);
            output.extend_from_slice(b"||||||| ");
            output.extend_from_slice(names[1].as_encoded_bytes());
            output.push(b'\n');
            append_lines(&mut output, base);
            output.extend_from_slice(b"=======\n");
            append_lines(&mut output, &yours);
            output.extend_from_slice(b">>>>>>> ");
            output.extend_from_slice(names[2].as_encoded_bytes());
            output.push(b'\n');
        }
        cursor = region.end;
    }
    append_lines(&mut output, &older[cursor..]);
    (output, conflict)
}

fn render_diff3_report(
    older: &[&[u8]],
    mine_edits: &[LineEdit<'_>],
    yours_edits: &[LineEdit<'_>],
) -> Vec<u8> {
    let mut output = Vec::new();
    for region in change_regions(mine_edits, yours_edits, true) {
        let mine = apply_region(older, mine_edits, &region);
        let base = &older[region.clone()];
        let yours = apply_region(older, yours_edits, &region);
        let suffix = if base == yours.as_slice() {
            b"1".as_slice()
        } else if mine == yours {
            b"2".as_slice()
        } else if mine.as_slice() == base {
            b"3".as_slice()
        } else {
            b"".as_slice()
        };
        output.extend_from_slice(b"====");
        output.extend_from_slice(suffix);
        output.push(b'\n');

        append_report_section(
            &mut output,
            1,
            side_line_index(mine_edits, region.start),
            &mine,
            suffix != b"2" && suffix != b"3",
        );
        if suffix == b"2" {
            append_report_section(
                &mut output,
                3,
                side_line_index(yours_edits, region.start),
                &yours,
                true,
            );
            append_report_section(&mut output, 2, region.start, base, true);
        } else {
            append_report_section(&mut output, 2, region.start, base, suffix != b"1");
            append_report_section(
                &mut output,
                3,
                side_line_index(yours_edits, region.start),
                &yours,
                true,
            );
        }
    }
    output
}

fn append_report_section(
    output: &mut Vec<u8>,
    file: usize,
    start: usize,
    lines: &[&[u8]],
    content: bool,
) {
    if lines.is_empty() {
        output.extend_from_slice(format!("{file}:{start}a\n").as_bytes());
        return;
    }

    let first = start + 1;
    let last = start + lines.len();
    if first == last {
        output.extend_from_slice(format!("{file}:{first}c\n").as_bytes());
    } else {
        output.extend_from_slice(format!("{file}:{first},{last}c\n").as_bytes());
    }
    if !content {
        return;
    }
    for line in lines {
        output.extend_from_slice(b"  ");
        output.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            output.extend_from_slice(b"\n\\ No newline at end of file\n");
        }
    }
}

fn render_diff3_ed(
    older: &[&[u8]],
    mine_edits: &[LineEdit<'_>],
    yours_edits: &[LineEdit<'_>],
) -> Vec<u8> {
    let mut output = Vec::new();
    for region in change_regions(mine_edits, yours_edits, true)
        .into_iter()
        .rev()
    {
        let mine = apply_region(older, mine_edits, &region);
        let yours = apply_region(older, yours_edits, &region);
        if yours == older[region.clone()] || mine == yours {
            continue;
        }
        let start = side_line_index(mine_edits, region.start);
        let range = start..start + mine.len();
        let command = if mine.is_empty() {
            'a'
        } else if yours.is_empty() {
            'd'
        } else {
            'c'
        };
        writeln!(output, "{}{command}", line_range(&range)).unwrap();
        if command != 'd' {
            for line in yours {
                append_ed_line(&mut output, line);
            }
            output.extend_from_slice(b".\n");
        }
    }
    output
}

fn informational_option(option: &str, name: &str, usage: &str) -> bool {
    match option {
        "--help" => println!("{usage}"),
        "--version" => println!("{name} (axe) {}", env!("CARGO_PKG_VERSION")),
        _ => return false,
    }
    true
}

fn read_operand(path: &OsStr) -> io::Result<Vec<u8>> {
    if path == OsStr::new("-") {
        let mut data = Vec::new();
        io::stdin().read_to_end(&mut data)?;
        Ok(data)
    } else {
        fs::read(path)
    }
}

fn file_error(program: &str, path: &OsStr, error: io::Error) -> i32 {
    eprintln!("{program}: {}: {error}", path.to_string_lossy());
    2
}
