use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use diffutilslib::params::{self, Format};
use similar::{Algorithm, DiffTag, capture_diff_slices};

pub fn diff(args: Vec<OsString>) -> i32 {
    if let Some(code) = informational(&args, "diff", "Usage: diff [OPTION]... FILE1 FILE2") {
        return code;
    }

    let mut recursive = false;
    let args = args
        .into_iter()
        .filter(|argument| {
            if argument == OsStr::new("-r") || argument == OsStr::new("--recursive") {
                recursive = true;
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>();
    let params = match params::parse_params(args.into_iter().peekable()) {
        Ok(params) => params,
        Err(error) => {
            eprintln!("diff: {error}");
            return 2;
        }
    };
    let from_path = Path::new(&params.from);
    let to_path = Path::new(&params.to);
    if from_path.is_dir() || to_path.is_dir() {
        if !recursive {
            eprintln!("diff: recursive comparison requires -r");
            return 2;
        }
        if !from_path.is_dir() || !to_path.is_dir() {
            println!(
                "File {} is a directory while file {} is not",
                if from_path.is_dir() {
                    from_path.display()
                } else {
                    to_path.display()
                },
                if from_path.is_dir() {
                    to_path.display()
                } else {
                    from_path.display()
                }
            );
            return 1;
        }
        return recursive_diff(&params);
    }

    compare_diff_files(&params)
}

fn compare_diff_files(params: &params::Params) -> i32 {
    let from = match read_operand(&params.from) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("diff: {}: {error}", params.from.to_string_lossy());
            return 2;
        }
    };
    let to = match read_operand(&params.to) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("diff: {}: {error}", params.to.to_string_lossy());
            return 2;
        }
    };

    if from == to {
        if params.report_identical_files {
            println!(
                "Files {} and {} are identical",
                params.from.to_string_lossy(),
                params.to.to_string_lossy()
            );
        }
        return 0;
    }
    if params.brief {
        println!(
            "Files {} and {} differ",
            params.from.to_string_lossy(),
            params.to.to_string_lossy()
        );
        return 1;
    }

    let mut side_output = Vec::new();
    let rendered = match params.format {
        Format::Normal => diffutilslib::normal_diff(&from, &to, params),
        Format::Unified => diffutilslib::unified_diff(&from, &to, params),
        Format::Context => diffutilslib::context_diff(&from, &to, params),
        Format::Ed => match diffutilslib::ed_diff(&from, &to, params) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("diff: {error:?}");
                return 2;
            }
        },
        Format::SideBySide => {
            diffutilslib::side_by_side_diff(&from, &to, &mut side_output, params);
            side_output
        }
    };
    match io::stdout().lock().write_all(&rendered) {
        Ok(()) => 1,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 1,
        Err(error) => {
            eprintln!("diff: {error}");
            2
        }
    }
}

fn recursive_diff(params: &params::Params) -> i32 {
    let left_root = Path::new(&params.from);
    let right_root = Path::new(&params.to);
    let mut paths = BTreeSet::new();
    if let Err(error) = collect_relative_paths(left_root, Path::new(""), &mut paths)
        .and_then(|()| collect_relative_paths(right_root, Path::new(""), &mut paths))
    {
        eprintln!("diff: {error}");
        return 2;
    }

    let mut status = 0;
    for relative in paths {
        let left = left_root.join(&relative);
        let right = right_root.join(&relative);
        match (left.exists(), right.exists()) {
            (true, false) => {
                println!(
                    "Only in {}: {}",
                    left.parent().unwrap_or(left_root).display(),
                    left.file_name().unwrap_or_default().to_string_lossy()
                );
                status = 1;
            }
            (false, true) => {
                println!(
                    "Only in {}: {}",
                    right.parent().unwrap_or(right_root).display(),
                    right.file_name().unwrap_or_default().to_string_lossy()
                );
                status = 1;
            }
            (true, true) if left.is_dir() != right.is_dir() => {
                println!(
                    "File {} is a directory while file {} is not",
                    if left.is_dir() {
                        left.display()
                    } else {
                        right.display()
                    },
                    if left.is_dir() {
                        right.display()
                    } else {
                        left.display()
                    }
                );
                status = 1;
            }
            (true, true) if !left.is_dir() => {
                let mut file_params = params.clone();
                file_params.from = left.clone().into_os_string();
                file_params.to = right.clone().into_os_string();
                if !params.brief {
                    println!("diff -r {} {}", left.display(), right.display());
                }
                let result = compare_diff_files(&file_params);
                if result == 1 && !params.brief {
                    status = 1;
                } else if result != 0 {
                    status = result;
                }
            }
            _ => {}
        }
        if status == 2 {
            return status;
        }
    }
    status
}

fn collect_relative_paths(
    root: &Path,
    relative: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> io::Result<()> {
    let directory = root.join(relative);
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        paths.insert(child.clone());
        if entry.file_type()?.is_dir() {
            collect_relative_paths(root, &child, paths)?;
        }
    }
    Ok(())
}

pub fn cmp(args: Vec<OsString>) -> i32 {
    if let Some(code) = informational(
        &args,
        "cmp",
        "Usage: cmp [OPTION]... FILE1 [FILE2 [SKIP1 [SKIP2]]]",
    ) {
        return code;
    }

    let params = match diffutilslib::cmp::parse_params(args.into_iter().peekable()) {
        Ok(params) => params,
        Err(error) => {
            eprintln!("cmp: {error}");
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

pub fn diff3(args: Vec<OsString>) -> i32 {
    if let Some(code) = informational(
        &args,
        "diff3",
        "Usage: diff3 [OPTION]... MYFILE OLDFILE YOURFILE",
    ) {
        return code;
    }

    let mut files = Vec::new();
    let mut labels = Vec::new();
    let mut merge = false;
    let mut iter = args.into_iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--") => files.extend(&mut iter),
            Some("-m" | "--merge") => merge = true,
            Some("-L" | "--label") => {
                let Some(label) = iter.next() else {
                    eprintln!("diff3: option requires an argument -- L");
                    return 2;
                };
                labels.push(label);
            }
            Some(option) if option.starts_with("--label=") => {
                labels.push(OsString::from(&option["--label=".len()..]));
            }
            Some(option) if option.starts_with('-') => {
                eprintln!("diff3: unsupported option {option}");
                return 2;
            }
            _ => files.push(arg),
        }
    }

    if files.len() != 3 {
        eprintln!("diff3: three input files are required");
        return 2;
    }
    if !labels.is_empty() && labels.len() != 3 {
        eprintln!("diff3: three labels are required");
        return 2;
    }

    let mine = match fs::read(&files[0]) {
        Ok(data) => data,
        Err(error) => return file_error("diff3", &files[0], error),
    };
    let older = match fs::read(&files[1]) {
        Ok(data) => data,
        Err(error) => return file_error("diff3", &files[1], error),
    };
    let yours = match fs::read(&files[2]) {
        Ok(data) => data,
        Err(error) => return file_error("diff3", &files[2], error),
    };

    let names = if labels.is_empty() {
        files.clone()
    } else {
        labels
    };
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
    } else {
        (
            render_diff3_report(&older_lines, &mine_edits, &yours_edits),
            false,
        )
    };

    match io::stdout().lock().write_all(&output) {
        Ok(()) => i32::from(conflict),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => i32::from(conflict),
        Err(error) => {
            eprintln!("diff3: {error}");
            2
        }
    }
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

fn append_conflict_side(output: &mut Vec<u8>, lines: &[&[u8]]) {
    append_lines(output, lines);
    if lines.last().is_some_and(|line| !line.ends_with(b"\n")) {
        output.push(b'\n');
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

    for region in change_regions(mine_edits, yours_edits, false) {
        append_lines(&mut output, &older[cursor..region.start]);
        let base = older[region.clone()].to_vec();
        let mine = apply_region(older, mine_edits, &region);
        let yours = apply_region(older, yours_edits, &region);
        if mine == yours {
            append_lines(&mut output, &mine);
        } else if mine == base {
            append_lines(&mut output, &yours);
        } else if yours == base {
            append_lines(&mut output, &mine);
        } else {
            conflict = true;
            output.extend_from_slice(b"<<<<<<< ");
            output.extend_from_slice(names[0].as_encoded_bytes());
            output.push(b'\n');
            append_conflict_side(&mut output, &mine);
            output.extend_from_slice(b"||||||| ");
            output.extend_from_slice(names[1].as_encoded_bytes());
            output.push(b'\n');
            append_conflict_side(&mut output, &base);
            output.extend_from_slice(b"=======\n");
            append_conflict_side(&mut output, &yours);
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
        let base = older[region.clone()].to_vec();
        let yours = apply_region(older, yours_edits, &region);
        let suffix = if base == yours {
            b"1".as_slice()
        } else if mine == yours {
            b"2".as_slice()
        } else if mine == base {
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
        );
        append_report_section(&mut output, 2, region.start, &base);
        append_report_section(
            &mut output,
            3,
            side_line_index(yours_edits, region.start),
            &yours,
        );
    }
    output
}

fn append_report_section(output: &mut Vec<u8>, file: usize, start: usize, lines: &[&[u8]]) {
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
    for line in lines {
        output.extend_from_slice(b"  ");
        output.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            output.push(b'\n');
        }
    }
}

fn informational(args: &[OsString], name: &str, usage: &str) -> Option<i32> {
    if args.iter().skip(1).any(|arg| arg == OsStr::new("--help")) {
        println!("{usage}");
        return Some(0);
    }

    if args
        .iter()
        .skip(1)
        .any(|arg| arg == OsStr::new("--version"))
    {
        println!("{name} (axe) {}", env!("CARGO_PKG_VERSION"));
        return Some(0);
    }

    None
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
