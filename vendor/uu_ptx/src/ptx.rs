// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write, stdin, stdout};
use std::ops::Range;
use std::path::Path;

use clap::{Arg, ArgAction, Command, value_parser};
use regex::bytes::{Regex, RegexBuilder};
use rustc_hash::FxHashSet;
use uucore::display::Quotable;
use uucore::error::{FromIo, UResult, USimpleError, UUsageError};
use uucore::format_usage;
use uucore::translate;

// Sentence punctuation belongs to the context. A lone newline, including a
// blank line, does not end a context unless it follows sentence punctuation.
const DEFAULT_SENTENCE_REGEXP: &str = r#"[.?!][\]"')}]*(?:$|\t|  )[ \t\n]*"#;

fn escape_trailing_backslash(pattern: &str) -> String {
    let trailing = pattern.chars().rev().take_while(|&c| c == '\\').count();
    if trailing % 2 == 1 {
        format!("{pattern}\\")
    } else {
        pattern.to_owned()
    }
}

fn compile_regex(pattern: &str, ignore_case: bool) -> UResult<Regex> {
    RegexBuilder::new(pattern)
        .unicode(false)
        .multi_line(true)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|error| {
            USimpleError::new(
                1,
                translate!("ptx-error-invalid-regexp", "error" => error.to_string()),
            )
        })
}

#[derive(Debug, PartialEq)]
enum OutFormat {
    Dumb,
    Roff,
    Tex,
}

#[derive(Debug)]
enum ContextBoundary {
    Default,
    WholeFile,
    Regexp(Regex),
}

#[derive(Debug)]
struct Config {
    format: OutFormat,
    gnu_ext: bool,
    auto_ref: bool,
    input_ref: bool,
    right_ref: bool,
    ignore_case: bool,
    macro_name: String,
    trunc_str: String,
    line_width: usize,
    gap_size: usize,
    sentence_regex: ContextBoundary,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            format: OutFormat::Dumb,
            gnu_ext: true,
            auto_ref: false,
            input_ref: false,
            right_ref: false,
            ignore_case: false,
            macro_name: "xx".to_owned(),
            trunc_str: "/".to_owned(),
            line_width: 72,
            gap_size: 3,
            sentence_regex: ContextBoundary::Default,
        }
    }
}

fn read_filter_file(matches: &clap::ArgMatches, option: &str) -> io::Result<Vec<u8>> {
    let filename = matches
        .get_one::<OsString>(option)
        .expect("parsing options failed!");
    let mut reader: Box<dyn Read> = if filename == "-" {
        Box::new(stdin())
    } else {
        Box::new(File::open(Path::new(filename))?)
    };
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_word_filter_file(
    matches: &clap::ArgMatches,
    option: &str,
    ignore_case: bool,
) -> io::Result<FxHashSet<Vec<u8>>> {
    let bytes = read_filter_file(matches, option)?;
    Ok(bytes
        .split(|&byte| byte == b'\n')
        .filter(|word| !word.is_empty())
        .map(|word| {
            if ignore_case {
                word.to_ascii_uppercase()
            } else {
                word.to_vec()
            }
        })
        .collect())
}

enum WordMatcher {
    Characters([bool; 256]),
    Regexp(Regex),
}

impl WordMatcher {
    fn find_words(&self, bytes: &[u8], range: Range<usize>) -> Vec<Range<usize>> {
        let mut words = Vec::new();
        let mut cursor = range.start;
        while cursor < range.end {
            let word = match self {
                Self::Characters(allowed) => {
                    while cursor < range.end && !allowed[usize::from(bytes[cursor])] {
                        cursor += 1;
                    }
                    if cursor == range.end {
                        break;
                    }
                    let start = cursor;
                    while cursor < range.end && allowed[usize::from(bytes[cursor])] {
                        cursor += 1;
                    }
                    start..cursor
                }
                Self::Regexp(regexp) => {
                    let Some(found) = regexp.find(&bytes[cursor..range.end]) else {
                        break;
                    };
                    let start = cursor + found.start();
                    let end = cursor + found.end();
                    if start == end {
                        cursor = end + 1;
                        continue;
                    }
                    cursor = end;
                    start..end
                }
            };
            words.push(word);
        }
        words
    }
}

struct WordFilter {
    only_set: FxHashSet<Vec<u8>>,
    ignore_set: FxHashSet<Vec<u8>>,
    matcher: WordMatcher,
}

impl WordFilter {
    fn new(matches: &clap::ArgMatches, config: &Config) -> UResult<Self> {
        let only_set = if matches.contains_id(options::ONLY_FILE) {
            read_word_filter_file(matches, options::ONLY_FILE, config.ignore_case)
                .map_err_context(String::new)?
        } else {
            FxHashSet::default()
        };
        let ignore_set = if matches.contains_id(options::IGNORE_FILE) {
            read_word_filter_file(matches, options::IGNORE_FILE, config.ignore_case)
                .map_err_context(String::new)?
        } else {
            FxHashSet::default()
        };
        let pattern = matches
            .get_one::<String>(options::WORD_REGEXP)
            .filter(|pattern| !pattern.is_empty());
        let matcher = if let Some(pattern) = pattern {
            WordMatcher::Regexp(compile_regex(
                &escape_trailing_backslash(pattern),
                config.ignore_case,
            )?)
        } else {
            let mut allowed = [false; 256];
            if matches.contains_id(options::BREAK_FILE) {
                allowed.fill(true);
                let breaks =
                    read_filter_file(matches, options::BREAK_FILE).map_err_context(String::new)?;
                for byte in breaks {
                    allowed[usize::from(byte)] = false;
                }
                if !config.gnu_ext {
                    for byte in b" \t\n" {
                        allowed[usize::from(*byte)] = false;
                    }
                }
            } else if config.gnu_ext {
                for byte in b'A'..=b'Z' {
                    allowed[usize::from(byte)] = true;
                    allowed[usize::from(byte.to_ascii_lowercase())] = true;
                }
            } else {
                allowed.fill(true);
                for byte in b" \t\n" {
                    allowed[usize::from(*byte)] = false;
                }
            }
            WordMatcher::Characters(allowed)
        };
        Ok(Self {
            only_set,
            ignore_set,
            matcher,
        })
    }
}

fn get_config(matches: &mut clap::ArgMatches) -> UResult<Config> {
    let mut config = Config::default();
    let err_msg = "parsing options failed";
    if matches.get_flag(options::TRADITIONAL) {
        config.gnu_ext = false;
        config.format = OutFormat::Roff;
    }
    config.auto_ref = matches.get_flag(options::AUTO_REFERENCE);
    config.input_ref = matches.get_flag(options::REFERENCES);
    config.right_ref = matches.get_flag(options::RIGHT_SIDE_REFS);
    config.ignore_case = matches.get_flag(options::IGNORE_CASE);
    if let Some(pattern) = matches.remove_one::<String>(options::SENTENCE_REGEXP) {
        let pattern = escape_trailing_backslash(&pattern);
        config.sentence_regex = if pattern.is_empty() {
            ContextBoundary::WholeFile
        } else {
            let regexp = compile_regex(&pattern, config.ignore_case)?;
            if regexp.is_match(b"") {
                return Err(USimpleError::new(1, translate!("ptx-error-empty-regexp")));
            }
            ContextBoundary::Regexp(regexp)
        };
    }
    if matches.contains_id(options::MACRO_NAME) {
        matches
            .get_one::<String>(options::MACRO_NAME)
            .expect(err_msg)
            .clone_into(&mut config.macro_name);
    }
    if matches.contains_id(options::FLAG_TRUNCATION) {
        matches
            .get_one::<String>(options::FLAG_TRUNCATION)
            .expect(err_msg)
            .clone_into(&mut config.trunc_str);
    }
    if matches.contains_id(options::WIDTH) {
        config.line_width = *matches.get_one::<u64>(options::WIDTH).unwrap() as usize;
    } else if matches.get_flag(options::TYPESET_MODE) {
        config.line_width = 100;
    }
    if matches.contains_id(options::GAP_SIZE) {
        config.gap_size = *matches.get_one::<u64>(options::GAP_SIZE).unwrap() as usize;
    }
    if let Some(format) = matches.get_one::<String>(options::FORMAT) {
        config.format = match format.as_str() {
            "roff" => OutFormat::Roff,
            "tex" => OutFormat::Tex,
            _ => unreachable!("should be caught by clap"),
        };
    }
    if matches.get_flag(options::format::ROFF) {
        config.format = OutFormat::Roff;
    }
    if matches.get_flag(options::format::TEX) {
        config.format = OutFormat::Tex;
    }
    Ok(config)
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn trim_end(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while start < end && is_space(bytes[end - 1]) {
        end -= 1;
    }
    end
}

fn trim_start(bytes: &[u8], mut start: usize, end: usize) -> usize {
    while start < end && is_space(bytes[start]) {
        start += 1;
    }
    start
}

struct Context {
    range: Range<usize>,
    // Keep all word boundaries, including rejected keywords: filters select
    // index entries, not the text available to the four context fields.
    words: Vec<Range<usize>>,
}

impl Context {
    fn align_start(&self, position: usize) -> usize {
        let next = self.words.partition_point(|word| word.end <= position);
        match self.words.get(next) {
            Some(word) if word.start < position => word.end,
            _ => position,
        }
    }

    fn align_end(&self, position: usize) -> usize {
        let next = self.words.partition_point(|word| word.end <= position);
        match self.words.get(next) {
            Some(word) if word.start < position => word.start,
            _ => position,
        }
    }
}

struct FileContent {
    bytes: Vec<u8>,
    contexts: Vec<Context>,
    last_reference_line: usize,
}

type FileMap = Vec<(OsString, FileContent)>;

fn read_input(input_files: &[OsString], config: &Config, filter: &WordFilter) -> UResult<FileMap> {
    let default_splitter;
    let sentence_splitter = match &config.sentence_regex {
        ContextBoundary::Default => {
            let pattern = if config.gnu_ext && !config.input_ref {
                DEFAULT_SENTENCE_REGEXP
            } else {
                "\n"
            };
            default_splitter = compile_regex(pattern, config.ignore_case)?;
            Some(&default_splitter)
        }
        ContextBoundary::WholeFile => None,
        ContextBoundary::Regexp(regexp) => Some(regexp),
    };
    let mut file_map = Vec::with_capacity(input_files.len());
    for filename in input_files {
        let mut reader: BufReader<Box<dyn Read>> = BufReader::new(if filename == "-" {
            Box::new(stdin())
        } else {
            Box::new(
                File::open(Path::new(filename)).map_err_context(|| filename.quote().to_string())?,
            )
        });
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err_context(|| filename.quote().to_string())?;
        let mut contexts = Vec::new();
        let mut start = 0;
        while start < bytes.len() {
            let next = sentence_splitter
                .as_ref()
                .and_then(|regexp| regexp.find_at(&bytes, start))
                .map_or(bytes.len(), |found| found.end());
            if next <= start {
                return Err(USimpleError::new(1, translate!("ptx-error-empty-regexp")));
            }
            let range = start..trim_end(&bytes, start, next);
            let words = filter.matcher.find_words(&bytes, range.clone());
            contexts.push(Context { range, words });
            start = next;
        }
        file_map.push((
            filename.clone(),
            FileContent {
                bytes,
                contexts,
                last_reference_line: 0,
            },
        ));
    }
    Ok(file_map)
}

struct WordRef {
    file_index: usize,
    context_index: usize,
    word_index: usize,
    local_line_nr: usize,
    context_start: usize,
    reference: Range<usize>,
}

fn compare_words(first: &[u8], second: &[u8], ignore_case: bool) -> Ordering {
    if ignore_case {
        first
            .iter()
            .map(u8::to_ascii_uppercase)
            .cmp(second.iter().map(u8::to_ascii_uppercase))
    } else {
        first.cmp(second)
    }
}

fn keyword<'a>(word_ref: &WordRef, file_map: &'a FileMap) -> &'a [u8] {
    let file = &file_map[word_ref.file_index].1;
    &file.bytes[file.contexts[word_ref.context_index].words[word_ref.word_index].clone()]
}

fn create_word_set(config: &Config, filter: &WordFilter, file_map: &mut FileMap) -> Vec<WordRef> {
    let mut words = Vec::new();
    let mut folded = Vec::new();
    for (file_index, (_, file)) in file_map.iter_mut().enumerate() {
        let mut line_scan = 0;
        let mut line_start = 0;
        let mut reference_end = 0;
        let mut local_line_nr = 0;
        for (context_index, context) in file.contexts.iter().enumerate() {
            for (word_index, word) in context.words.iter().enumerate() {
                while line_scan < word.start {
                    if file.bytes[line_scan] == b'\n' {
                        local_line_nr += 1;
                        line_start = line_scan + 1;
                        reference_end = line_start;
                    }
                    line_scan += 1;
                }
                if config.input_ref {
                    file.last_reference_line = local_line_nr;
                    while reference_end < file.bytes.len() && !is_space(file.bytes[reference_end]) {
                        reference_end += 1;
                    }
                    if word.start < reference_end {
                        continue;
                    }
                }
                let text = &file.bytes[word.clone()];
                let filtered_text = if config.ignore_case
                    && (!filter.only_set.is_empty() || !filter.ignore_set.is_empty())
                {
                    folded.clear();
                    folded.extend(text.iter().map(u8::to_ascii_uppercase));
                    folded.as_slice()
                } else {
                    text
                };
                if (!filter.only_set.is_empty() && !filter.only_set.contains(filtered_text))
                    || filter.ignore_set.contains(filtered_text)
                {
                    continue;
                }
                let context_start = if config.input_ref && line_start == context.range.start {
                    trim_start(&file.bytes, reference_end, context.range.end)
                } else {
                    context.range.start
                };
                words.push(WordRef {
                    file_index,
                    context_index,
                    word_index,
                    local_line_nr,
                    context_start,
                    reference: line_start..reference_end,
                });
                file.last_reference_line = local_line_nr;
            }
        }
    }
    words.sort_unstable_by(|first, second| {
        compare_words(
            keyword(first, file_map),
            keyword(second, file_map),
            config.ignore_case,
        )
        .then_with(|| first.file_index.cmp(&second.file_index))
        .then_with(|| first.context_index.cmp(&second.context_index))
        .then_with(|| first.word_index.cmp(&second.word_index))
    });
    words
}

struct OutputLayout {
    half_width: usize,
    before_width: usize,
    keyafter_width: usize,
    reference_width: usize,
}

impl OutputLayout {
    fn new(config: &Config, file_map: &FileMap, words: &[WordRef]) -> Self {
        let reference_width = if config.auto_ref {
            file_map
                .iter()
                .map(|(filename, file)| {
                    let name_length = if filename == "-" {
                        0
                    } else {
                        filename.as_encoded_bytes().len()
                    };
                    name_length + (file.last_reference_line + 2).ilog10() as usize + 2
                })
                .max()
                .unwrap_or(0)
        } else if config.input_ref {
            words
                .iter()
                .map(|word| word.reference.len())
                .max()
                .unwrap_or(0)
        } else {
            0
        };
        let width = if (config.auto_ref || config.input_ref) && !config.right_ref {
            config
                .line_width
                .saturating_sub(reference_width.saturating_add(config.gap_size))
        } else {
            config.line_width
        };
        let half_width = width / 2;
        let marker_width = config.trunc_str.len().saturating_mul(2);
        let before_width = half_width
            .saturating_sub(config.gap_size)
            .saturating_sub(if config.gnu_ext { marker_width } else { 0 });
        let keyafter_width = half_width
            .saturating_sub(marker_width)
            .saturating_sub(usize::from(!config.gnu_ext));
        Self {
            half_width,
            before_width,
            keyafter_width,
            reference_width,
        }
    }
}

struct Chunks {
    tail: Range<usize>,
    before: Range<usize>,
    keyafter: Range<usize>,
    head: Range<usize>,
    tail_truncated: bool,
    before_truncated: bool,
    keyafter_truncated: bool,
    head_truncated: bool,
}

impl Chunks {
    fn new(config: &Config, layout: &OutputLayout, word_ref: &WordRef, file: &FileContent) -> Self {
        let context = &file.contexts[word_ref.context_index];
        let word = &context.words[word_ref.word_index];
        let bytes = &file.bytes;
        let left = word_ref.context_start;
        let right = context.range.end;
        let mark = !config.trunc_str.is_empty();

        let after_limit = word.start.saturating_add(layout.keyafter_width).min(right);
        let after_end = context.align_end(after_limit).max(word.end);
        let mut keyafter_truncated = mark && after_end < right;
        let keyafter = word.start..trim_end(bytes, word.start, after_end);

        let before_end = trim_end(bytes, left, word.start);
        let before_start = context
            .align_start(before_end.saturating_sub(layout.before_width).max(left))
            .min(before_end);
        let mut before_truncated = mark && trim_end(bytes, left, before_start) > left;
        let before = trim_start(bytes, before_start, before_end)..before_end;

        let tail_width = layout
            .before_width
            .saturating_sub(before.len())
            .saturating_sub(config.gap_size);
        let mut tail = keyafter.end..keyafter.end;
        let mut tail_truncated = false;
        if tail_width > 0 {
            let start = trim_start(bytes, keyafter.end, right);
            // The wrap-around tail has a strict upper bound; a final token
            // exactly filling its allowance belongs to the truncated suffix.
            let limit = start.saturating_add(tail_width - 1).min(right);
            let end = context.align_end(limit).max(start);
            if end > start {
                keyafter_truncated = false;
                tail_truncated = mark && end < right;
            }
            tail = start..trim_end(bytes, start, end);
        }

        let head_width = layout
            .keyafter_width
            .saturating_sub(keyafter.len())
            .saturating_sub(config.gap_size);
        let mut head = left..left;
        let mut head_truncated = false;
        if head_width > 0 {
            let end = trim_end(bytes, left, before.start);
            let start = context
                .align_start(end.saturating_sub(head_width).max(left))
                .min(end);
            if end > start {
                before_truncated = false;
                head_truncated = mark && start > left;
            }
            head = trim_start(bytes, start, end)..end;
        }
        Self {
            tail,
            before,
            keyafter,
            head,
            tail_truncated,
            before_truncated,
            keyafter_truncated,
            head_truncated,
        }
    }
}

fn write_spaces(writer: &mut dyn Write, mut count: usize) -> io::Result<()> {
    const SPACES: [u8; 64] = [b' '; 64];
    while count > 0 {
        let length = count.min(SPACES.len());
        writer.write_all(&SPACES[..length])?;
        count -= length;
    }
    Ok(())
}

fn write_field(writer: &mut dyn Write, bytes: &[u8], format: &OutFormat) -> io::Result<()> {
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let replacement: &[u8] = if is_space(byte) {
            b" "
        } else {
            match (format, byte) {
                (OutFormat::Roff, b'"') => b"\"\"",
                (OutFormat::Tex, b'$') => b"\\$",
                (OutFormat::Tex, b'%') => b"\\%",
                (OutFormat::Tex, b'&') => b"\\&",
                (OutFormat::Tex, b'#') => b"\\#",
                (OutFormat::Tex, b'_') => b"\\_",
                (OutFormat::Tex, b'{') => b"$\\{$",
                (OutFormat::Tex, b'}') => b"$\\}$",
                (OutFormat::Tex, b'\\') => b"\\backslash{}",
                _ => continue,
            }
        };
        writer.write_all(&bytes[start..index])?;
        writer.write_all(replacement)?;
        start = index + 1;
    }
    writer.write_all(&bytes[start..])
}

fn write_mark(writer: &mut dyn Write, config: &Config, truncated: bool) -> io::Result<()> {
    if truncated {
        writer.write_all(config.trunc_str.as_bytes())?;
    }
    Ok(())
}

fn get_reference(
    reference: &mut Vec<u8>,
    config: &Config,
    word_ref: &WordRef,
    filename: &OsStr,
    file: &FileContent,
) {
    reference.clear();
    if config.auto_ref {
        if filename != "-" {
            reference.extend_from_slice(filename.as_encoded_bytes());
        }
        write!(reference, ":{}", word_ref.local_line_nr + 1)
            .expect("writing to a byte vector cannot fail");
    } else if config.input_ref {
        reference.extend_from_slice(&file.bytes[word_ref.reference.clone()]);
    }
}

fn format_dumb_line(
    writer: &mut dyn Write,
    config: &Config,
    layout: &OutputLayout,
    chunks: &Chunks,
    bytes: &[u8],
    reference: &[u8],
) -> io::Result<()> {
    let marker_width = config.trunc_str.len();
    if !config.right_ref {
        write_field(writer, reference, &config.format)?;
        let colon = usize::from(config.auto_ref);
        if config.auto_ref {
            writer.write_all(b":")?;
        }
        write_spaces(
            writer,
            layout
                .reference_width
                .saturating_add(config.gap_size)
                .saturating_sub(reference.len().saturating_add(colon)),
        )?;
    }
    if !chunks.tail.is_empty() {
        write_field(writer, &bytes[chunks.tail.clone()], &config.format)?;
        write_mark(writer, config, chunks.tail_truncated)?;
    }
    let left_used = chunks
        .before
        .len()
        .saturating_add(chunks.tail.len())
        .saturating_add(marker_width.saturating_mul(usize::from(chunks.before_truncated)))
        .saturating_add(marker_width.saturating_mul(usize::from(chunks.tail_truncated)));
    write_spaces(
        writer,
        layout
            .half_width
            .saturating_sub(config.gap_size)
            .saturating_sub(left_used),
    )?;
    write_mark(writer, config, chunks.before_truncated)?;
    write_field(writer, &bytes[chunks.before.clone()], &config.format)?;
    write_spaces(writer, config.gap_size)?;
    write_field(writer, &bytes[chunks.keyafter.clone()], &config.format)?;
    write_mark(writer, config, chunks.keyafter_truncated)?;

    let has_reference = config.auto_ref || config.input_ref;
    if !chunks.head.is_empty() || (has_reference && config.right_ref) {
        let right_used = chunks
            .keyafter
            .len()
            .saturating_add(chunks.head.len())
            .saturating_add(marker_width.saturating_mul(usize::from(chunks.keyafter_truncated)))
            .saturating_add(marker_width.saturating_mul(usize::from(chunks.head_truncated)));
        write_spaces(writer, layout.half_width.saturating_sub(right_used))?;
        write_mark(writer, config, chunks.head_truncated)?;
        write_field(writer, &bytes[chunks.head.clone()], &config.format)?;
    }
    if has_reference && config.right_ref {
        write_spaces(writer, config.gap_size)?;
        write_field(writer, reference, &config.format)?;
    }
    writer.write_all(b"\n")
}

fn format_roff_line(
    writer: &mut dyn Write,
    config: &Config,
    chunks: &Chunks,
    bytes: &[u8],
    reference: &[u8],
) -> io::Result<()> {
    write!(writer, ".{}", config.macro_name)?;
    for (range, before_mark, after_mark) in [
        (&chunks.tail, false, chunks.tail_truncated),
        (&chunks.before, chunks.before_truncated, false),
        (&chunks.keyafter, false, chunks.keyafter_truncated),
        (&chunks.head, chunks.head_truncated, false),
    ] {
        writer.write_all(b" \"")?;
        write_mark(writer, config, before_mark)?;
        write_field(writer, &bytes[range.clone()], &config.format)?;
        write_mark(writer, config, after_mark)?;
        writer.write_all(b"\"")?;
    }
    if config.auto_ref || config.input_ref {
        writer.write_all(b" \"")?;
        write_field(writer, reference, &config.format)?;
        writer.write_all(b"\"")?;
    }
    writer.write_all(b"\n")
}

fn format_tex_line(
    writer: &mut dyn Write,
    config: &Config,
    chunks: &Chunks,
    word: &Range<usize>,
    bytes: &[u8],
    reference: &[u8],
) -> io::Result<()> {
    write!(writer, "\\{} ", config.macro_name)?;
    let keyword_end = word.end.min(chunks.keyafter.end);
    for range in [
        chunks.tail.clone(),
        chunks.before.clone(),
        word.start..keyword_end,
        keyword_end..chunks.keyafter.end,
        chunks.head.clone(),
    ] {
        writer.write_all(b"{")?;
        write_field(writer, &bytes[range], &config.format)?;
        writer.write_all(b"}")?;
    }
    if config.auto_ref || config.input_ref {
        writer.write_all(b"{")?;
        write_field(writer, reference, &config.format)?;
        writer.write_all(b"}")?;
    }
    writer.write_all(b"\n")
}

fn write_traditional_output(
    config: &Config,
    file_map: &FileMap,
    words: &[WordRef],
    output_filename: &OsStr,
) -> UResult<()> {
    let mut writer: BufWriter<Box<dyn Write>> =
        BufWriter::new(if output_filename == OsStr::new("-") {
            Box::new(stdout())
        } else {
            Box::new(
                File::create(output_filename)
                    .map_err_context(|| output_filename.quote().to_string())?,
            )
        });
    let layout = OutputLayout::new(config, file_map, words);
    let mut reference = Vec::with_capacity(layout.reference_width);
    for word_ref in words {
        let (filename, file) = &file_map[word_ref.file_index];
        let chunks = Chunks::new(config, &layout, word_ref, file);
        get_reference(&mut reference, config, word_ref, filename, file);
        let result = match config.format {
            OutFormat::Dumb => format_dumb_line(
                &mut writer,
                config,
                &layout,
                &chunks,
                &file.bytes,
                &reference,
            ),
            OutFormat::Roff => {
                format_roff_line(&mut writer, config, &chunks, &file.bytes, &reference)
            }
            OutFormat::Tex => format_tex_line(
                &mut writer,
                config,
                &chunks,
                &file.contexts[word_ref.context_index].words[word_ref.word_index],
                &file.bytes,
                &reference,
            ),
        };
        result.map_err_context(|| translate!("ptx-error-write-failed"))?;
    }
    writer
        .flush()
        .map_err_context(|| translate!("ptx-error-write-failed"))
}

mod options {
    pub mod format {
        pub static ROFF: &str = "roff";
        pub static TEX: &str = "tex";
    }

    pub static FILE: &str = "file";
    pub static AUTO_REFERENCE: &str = "auto-reference";
    pub static TRADITIONAL: &str = "traditional";
    pub static FLAG_TRUNCATION: &str = "flag-truncation";
    pub static MACRO_NAME: &str = "macro-name";
    pub static FORMAT: &str = "format";
    pub static RIGHT_SIDE_REFS: &str = "right-side-refs";
    pub static SENTENCE_REGEXP: &str = "sentence-regexp";
    pub static WORD_REGEXP: &str = "word-regexp";
    pub static BREAK_FILE: &str = "break-file";
    pub static IGNORE_CASE: &str = "ignore-case";
    pub static GAP_SIZE: &str = "gap-size";
    pub static IGNORE_FILE: &str = "ignore-file";
    pub static ONLY_FILE: &str = "only-file";
    pub static REFERENCES: &str = "references";
    pub static TYPESET_MODE: &str = "typeset-mode";
    pub static WIDTH: &str = "width";
}

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let mut matches = uucore::clap_localization::handle_clap_result(uu_app(), args)?;
    let config = get_config(&mut matches)?;

    let input_files;
    let output_file: OsString;

    let mut files = matches
        .get_many::<OsString>(options::FILE)
        .into_iter()
        .flatten()
        .cloned();

    if config.gnu_ext {
        input_files = {
            let mut files = files.collect::<Vec<_>>();
            if files.is_empty() {
                files.push(OsString::from("-"));
            }
            files
        };
        output_file = OsString::from("-");
    } else {
        input_files = vec![files.next().unwrap_or(OsString::from("-"))];
        output_file = files.next().unwrap_or(OsString::from("-"));
        if let Some(file) = files.next() {
            return Err(UUsageError::new(
                1,
                translate!("ptx-error-extra-operand", "operand" => file.quote()),
            ));
        }
    }

    let word_filter = WordFilter::new(&matches, &config)?;
    let mut file_map = read_input(&input_files, &config, &word_filter)?;
    let word_set = create_word_set(&config, &word_filter, &mut file_map);
    write_traditional_output(&config, &file_map, &word_set, &output_file)
}

pub fn uu_app() -> Command {
    Command::new("ptx")
        .about(translate!("ptx-about"))
        .version(uucore::crate_version!())
        .help_template(uucore::localized_help_template("ptx"))
        .override_usage(format_usage(&translate!("ptx-usage")))
        .infer_long_args(true)
        .arg(
            Arg::new(options::FILE)
                .hide(true)
                .action(ArgAction::Append)
                .value_hint(clap::ValueHint::FilePath)
                .value_parser(clap::value_parser!(OsString)),
        )
        .arg(
            Arg::new(options::AUTO_REFERENCE)
                .short('A')
                .long(options::AUTO_REFERENCE)
                .help(translate!("ptx-help-auto-reference"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::TRADITIONAL)
                .short('G')
                .long(options::TRADITIONAL)
                .help(translate!("ptx-help-traditional"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::FLAG_TRUNCATION)
                .short('F')
                .long(options::FLAG_TRUNCATION)
                .help(translate!("ptx-help-flag-truncation"))
                .value_name("STRING"),
        )
        .arg(
            Arg::new(options::MACRO_NAME)
                .short('M')
                .long(options::MACRO_NAME)
                .help(translate!("ptx-help-macro-name"))
                .value_name("STRING"),
        )
        .arg(
            Arg::new(options::FORMAT)
                .long(options::FORMAT)
                .hide(true)
                .value_parser(["roff", "tex"])
                .overrides_with_all([options::FORMAT, options::format::ROFF, options::format::TEX]),
        )
        .arg(
            Arg::new(options::format::ROFF)
                .short('O')
                .help(translate!("ptx-help-roff"))
                .overrides_with_all([options::FORMAT, options::format::ROFF, options::format::TEX])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::format::TEX)
                .short('T')
                .help(translate!("ptx-help-tex"))
                .overrides_with_all([options::FORMAT, options::format::ROFF, options::format::TEX])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::RIGHT_SIDE_REFS)
                .short('R')
                .long(options::RIGHT_SIDE_REFS)
                .help(translate!("ptx-help-right-side-refs"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::SENTENCE_REGEXP)
                .short('S')
                .long(options::SENTENCE_REGEXP)
                .help(translate!("ptx-help-sentence-regexp"))
                .value_name("REGEXP"),
        )
        .arg(
            Arg::new(options::WORD_REGEXP)
                .short('W')
                .long(options::WORD_REGEXP)
                .help(translate!("ptx-help-word-regexp"))
                .value_name("REGEXP"),
        )
        .arg(
            Arg::new(options::BREAK_FILE)
                .short('b')
                .long(options::BREAK_FILE)
                .help(translate!("ptx-help-break-file"))
                .value_name("FILE")
                .value_hint(clap::ValueHint::FilePath)
                .value_parser(clap::value_parser!(OsString)),
        )
        .arg(
            Arg::new(options::IGNORE_CASE)
                .short('f')
                .long(options::IGNORE_CASE)
                .help(translate!("ptx-help-ignore-case"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::GAP_SIZE)
                .short('g')
                .long(options::GAP_SIZE)
                .value_parser(value_parser!(u64).range(1..))
                .help(translate!("ptx-help-gap-size"))
                .value_name("NUMBER"),
        )
        .arg(
            Arg::new(options::IGNORE_FILE)
                .short('i')
                .long(options::IGNORE_FILE)
                .help(translate!("ptx-help-ignore-file"))
                .value_name("FILE")
                .value_hint(clap::ValueHint::FilePath)
                .value_parser(clap::value_parser!(OsString)),
        )
        .arg(
            Arg::new(options::ONLY_FILE)
                .short('o')
                .long(options::ONLY_FILE)
                .help(translate!("ptx-help-only-file"))
                .value_name("FILE")
                .value_hint(clap::ValueHint::FilePath)
                .value_parser(clap::value_parser!(OsString)),
        )
        .arg(
            Arg::new(options::REFERENCES)
                .short('r')
                .long(options::REFERENCES)
                .help(translate!("ptx-help-references"))
                .value_name("FILE")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::TYPESET_MODE)
                .short('t')
                .long(options::TYPESET_MODE)
                .help(translate!("ptx-help-typeset-mode"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(options::WIDTH)
                .short('w')
                .long(options::WIDTH)
                .value_parser(value_parser!(u64).range(1..))
                .help(translate!("ptx-help-width"))
                .value_name("NUMBER"),
        )
}
