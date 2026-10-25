use super::*;
use crate::command;

fn run_sed(script: &str, input: &str) -> String {
    run_sed_opts(script, input, false)
}

fn run_sed_opts(script: &str, input: &str, quiet: bool) -> String {
    let parsed = command::parse(script).unwrap();
    let options = Options {
        quiet,
        expressions: vec![],
        script_files: vec![],
        in_place: None,
        follow_symlinks: false,
        extended_regexp: false,
        separate: false,
        null_data: false,
        args: vec![],
    };
    let engine = Engine::new(parsed, &options).unwrap();
    let reader = io::Cursor::new(input.as_bytes());
    let mut output = Vec::new();
    engine
        .process_stream(io::BufReader::new(reader), &mut output)
        .unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn substitute_basic() {
    assert_eq!(run_sed("s/foo/bar/", "foo\n"), "bar\n");
}

#[test]
fn execute_command_arg() {
    // `e command` sends the command's output to the stream, then the
    // pattern space is auto-printed.
    assert_eq!(run_sed("1e echo INJECTED", "hello\n"), "INJECTED\nhello\n");
}

#[test]
fn execute_pattern_space() {
    // `e` with no argument runs the pattern space and replaces it.
    assert_eq!(run_sed("e", "echo hi\n"), "hi\n");
}

#[test]
fn version_is_noop() {
    // `v VERSION` does nothing and `;` terminates its argument.
    assert_eq!(run_sed("v 4.2; s/foo/bar/", "foo\n"), "bar\n");
}

#[test]
fn read_line_interleaves() {
    // `R` queues one line of the file per cycle.
    use std::io::Write as _;
    let mut extra = tempfile::NamedTempFile::new().unwrap();
    write!(extra, "X\nY\nZ\n").unwrap();
    let script = format!("R {}", extra.path().display());
    assert_eq!(run_sed(&script, "a\nb\nc\n"), "a\nX\nb\nY\nc\nZ\n");
}

#[test]
fn write_first_line_only() {
    // `W` writes only the first line of the pattern space.
    let target = tempfile::NamedTempFile::new().unwrap();
    let path = target.path().to_path_buf();
    let script = format!("N; W {}", path.display());
    run_sed(&script, "one\ntwo\n");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\n");
}

#[test]
fn print_file_name() {
    // `F` prints the current input file name.
    let parsed = command::parse("F").unwrap();
    let engine = Engine::new(parsed, &Options::default()).unwrap();
    let mut out = Vec::new();
    engine
        .process(
            ReaderInput::new(
                io::Cursor::new(b"x\n".as_ref()),
                OsStr::new("my.txt"),
                b'\n',
            ),
            &mut Output::new(&mut out, b'\n'),
        )
        .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "my.txt\nx\n");
}

#[test]
fn substitute_global() {
    assert_eq!(run_sed("s/o/0/g", "foo boo\n"), "f00 b00\n");
}

#[test]
fn substitute_first_only() {
    assert_eq!(run_sed("s/o/0/", "foo\n"), "f0o\n");
}

#[test]
fn substitute_nth() {
    assert_eq!(run_sed("s/o/0/2", "foo boo\n"), "fo0 boo\n");
}

#[test]
fn substitute_ampersand() {
    assert_eq!(run_sed("s/foo/[&]/", "foo\n"), "[foo]\n");
}

#[test]
fn substitute_backreference() {
    assert_eq!(run_sed(r"s/\(f\)\(o\+\)/\2\1/", "foo\n"), "oof\n");
}

#[test]
fn delete_command() {
    assert_eq!(run_sed("2d", "a\nb\nc\n"), "a\nc\n");
}

#[test]
fn print_command_quiet() {
    assert_eq!(run_sed_opts("2p", "a\nb\nc\n", true), "b\n");
}

#[test]
fn address_regex() {
    assert_eq!(
        run_sed("/^b/d", "apple\nbanana\ncherry\n"),
        "apple\ncherry\n"
    );
}

#[test]
fn address_range() {
    assert_eq!(run_sed("2,3d", "a\nb\nc\nd\n"), "a\nd\n");
}

#[test]
fn address_negation() {
    assert_eq!(run_sed("2!d", "a\nb\nc\n"), "b\n");
}

#[test]
fn transliterate() {
    assert_eq!(run_sed("y/abc/xyz/", "abc\n"), "xyz\n");
}

#[test]
fn quit_command() {
    assert_eq!(run_sed("2q", "a\nb\nc\n"), "a\nb\n");
}

#[test]
fn hold_and_get() {
    // Store line 1 in hold, append it after line 2
    assert_eq!(
        run_sed("1h;2G", "first\nsecond\n"),
        "first\nsecond\nfirst\n"
    );
}

#[test]
fn exchange() {
    assert_eq!(run_sed("1{h;d};2{x}", "first\nsecond\n"), "first\n");
}

#[test]
fn labels_and_branch() {
    // Simple loop: replace one 'a' at a time with 'b', branch back if substitution made
    assert_eq!(run_sed(":l\ns/a/b/\nt l", "aaa\n"), "bbb\n");
}

#[test]
fn append_text() {
    assert_eq!(
        run_sed("1a after first", "first\nsecond\n"),
        "first\nafter first\nsecond\n"
    );
}

#[test]
fn insert_text() {
    assert_eq!(
        run_sed("2i before second", "first\nsecond\n"),
        "first\nbefore second\nsecond\n"
    );
}

#[test]
fn line_number() {
    assert_eq!(run_sed("=", "a\nb\n"), "1\na\n2\nb\n");
}

#[test]
fn multiple_commands() {
    assert_eq!(run_sed("s/a/x/; s/b/y/", "ab\n"), "xy\n");
}

#[test]
fn last_line_address() {
    assert_eq!(run_sed("$d", "a\nb\nc\n"), "a\nb\n");
}

#[test]
fn clear_pattern() {
    assert_eq!(run_sed("z", "hello\n"), "\n");
}

#[test]
fn custom_delimiter() {
    assert_eq!(run_sed("s|foo|bar|", "foo\n"), "bar\n");
}

#[test]
fn case_insensitive_sub() {
    assert_eq!(run_sed("s/foo/bar/i", "FOO\n"), "bar\n");
}

#[test]
fn block_with_address() {
    assert_eq!(run_sed("/a/ { s/a/x/; s/$/!/ }", "a\nb\n"), "x!\nb\n");
}

#[test]
fn regex_range_address() {
    assert_eq!(run_sed("/start/,/end/d", "a\nstart\nb\nend\nc\n"), "a\nc\n");
}

#[test]
fn empty_input() {
    assert_eq!(run_sed("s/a/b/", ""), "");
}

#[test]
fn passthrough_no_match() {
    assert_eq!(run_sed("s/xyz/abc/", "hello\n"), "hello\n");
}

// ---------------------------------------------------------------
// Comprehensive coverage tests
// ---------------------------------------------------------------

// -- n command --

#[test]
fn n_single_line() {
    // n on the only line: print it, exit (no double-print)
    assert_eq!(run_sed("n", "a\n"), "a\n");
}

#[test]
fn n_two_lines() {
    // n on line 1: print "a", read "b" into PS, auto-print "b"
    assert_eq!(run_sed("n", "a\nb\n"), "a\nb\n");
}

#[test]
fn n_with_command_after() {
    // n reads next line, then subsequent commands operate on it
    assert_eq!(run_sed("n;s/b/X/", "a\nb\nc\n"), "a\nX\nc\n");
}

#[test]
fn n_quiet_mode() {
    // With -n, n does NOT print before reading next
    assert_eq!(run_sed_opts("n;p", "a\nb\nc\n", true), "b\n");
}

// -- N command --

#[test]
fn big_n_appends() {
    // N appends next line to pattern space with \n
    assert_eq!(run_sed("N;s/\\n/ /", "a\nb\n"), "a b\n");
}

#[test]
fn big_n_at_end() {
    // N at last line: print and exit
    assert_eq!(run_sed("N", "a\nb\nc\n"), "a\nb\nc\n");
}

// -- D command --

#[test]
fn big_d_deletes_first_line_of_pattern() {
    // N;P;D is the classic "sliding window" idiom
    assert_eq!(run_sed("N;P;D", "a\nb\nc\n"), "a\nb\nc\n");
}

#[test]
fn big_d_single_line() {
    // D on single-line pattern space acts like d (deletes every line)
    assert_eq!(run_sed("D", "a\nb\n"), "");
}

// -- c command --

#[test]
fn change_single_address() {
    assert_eq!(run_sed("2c REPLACED", "a\nb\nc\n"), "a\nREPLACED\nc\n");
}

#[test]
fn change_regex_address() {
    assert_eq!(run_sed("/b/c GONE", "a\nb\nc\n"), "a\nGONE\nc\n");
}

// -- P command --

#[test]
fn big_p_first_line() {
    // P prints up to first \n in pattern space
    assert_eq!(run_sed("N;P", "first\nsecond\n"), "first\nfirst\nsecond\n");
}

// -- i (insert) with address --

#[test]
fn insert_before_last() {
    assert_eq!(run_sed("$i END", "a\nb\n"), "a\nEND\nb\n");
}

// -- a (append) with regex address --

#[test]
fn append_after_match() {
    assert_eq!(run_sed("/b/a AFTER", "a\nb\nc\n"), "a\nb\nAFTER\nc\n");
}

// -- Hold space operations --

#[test]
fn hold_append_and_get() {
    // H appends to hold; G appends hold to pattern
    assert_eq!(
        run_sed("1H;2G", "first\nsecond\n"),
        "first\nsecond\n\nfirst\n"
    );
}

#[test]
fn hold_get_replace() {
    // h copies to hold; g copies from hold to pattern
    assert_eq!(run_sed("1h;2g", "first\nsecond\n"), "first\nfirst\n");
}

#[test]
fn reverse_lines() {
    // Classic sed reverse: 1!G;h;$!d
    assert_eq!(run_sed("1!G;h;$!d", "a\nb\nc\n"), "c\nb\na\n");
}

// -- x (exchange) --

#[test]
fn exchange_basic() {
    // Hold starts empty, so x on line 1 gives empty, hold gets "a"
    assert_eq!(run_sed("x", "a\nb\n"), "\na\n");
}

// -- Address ranges --

#[test]
fn range_regex_to_regex() {
    assert_eq!(run_sed("/start/,/end/d", "a\nstart\nb\nend\nc\n"), "a\nc\n");
}

#[test]
fn range_line_to_last() {
    assert_eq!(run_sed("2,$d", "a\nb\nc\nd\n"), "a\n");
}

#[test]
fn range_negated() {
    // Delete everything NOT in range 2,3
    assert_eq!(run_sed("2,3!d", "a\nb\nc\nd\n"), "b\nc\n");
}

#[test]
fn range_start_equals_end_on_same_line() {
    // 2,2d only deletes line 2
    assert_eq!(run_sed("2,2d", "a\nb\nc\n"), "a\nc\n");
}

#[test]
fn range_regex_no_check_on_start_line() {
    // When start and end regexes could match same line,
    // end is NOT checked on start line (POSIX/GNU behavior)
    assert_eq!(
        run_sed_opts("/\\[start\\]/,/\\[/p", "[start]\nfoo\nbar\n[end]\n", true,),
        "[start]\nfoo\nbar\n[end]\n"
    );
}

// -- Step address --

#[test]
fn step_even_lines() {
    assert_eq!(run_sed("0~2d", "a\nb\nc\nd\ne\n"), "a\nc\ne\n");
}

#[test]
fn step_odd_lines() {
    assert_eq!(run_sed("1~2d", "a\nb\nc\nd\ne\n"), "b\nd\n");
}

// -- Substitute flags --

#[test]
fn sub_nth_3() {
    assert_eq!(run_sed("s/a/X/3", "aaaaa\n"), "aaXaa\n");
}

#[test]
fn sub_nth_global() {
    // `Ng` replaces the Nth match and all matches after it.
    assert_eq!(run_sed("s/a/X/2g", "aaaaa\n"), "aXXXX\n");
}

#[test]
fn sub_global_and_print() {
    assert_eq!(run_sed_opts("s/a/X/gp", "aaa\nbbb\n", true), "XXX\n");
}

#[test]
fn sub_escaped_delimiter() {
    // Use | as delimiter, pattern contains |
    assert_eq!(run_sed("s/a\\/b/X/", "a/b\n"), "X\n");
}

#[test]
fn sub_newline_in_replacement() {
    assert_eq!(run_sed("s/a/X\\nY/", "a\n"), "X\nY\n");
}

#[test]
fn sub_tab_in_replacement() {
    assert_eq!(run_sed("s/a/X\\tY/", "a\n"), "X\tY\n");
}

#[test]
fn sub_literal_ampersand() {
    // \& should be literal &
    assert_eq!(run_sed("s/foo/\\&/", "foo\n"), "&\n");
}

#[test]
fn sub_literal_backslash() {
    // \\\\ in replacement → literal backslash
    assert_eq!(run_sed("s/a/\\\\/", "a\n"), "\\\n");
}

// -- Branching --

#[test]
fn branch_unconditional() {
    // b skip jumps forward past the d command, preserving output
    assert_eq!(run_sed("b skip;d;:skip", "hello\n"), "hello\n");
}

#[test]
fn branch_unconditional_no_label() {
    // b (no label) branches to end of script
    assert_eq!(run_sed("b\nd", "hello\n"), "hello\n");
}

#[test]
fn branch_if_sub_no_match() {
    // t should NOT branch if no sub was made
    assert_eq!(run_sed("s/x/y/;t end;s/a/X/;:end", "abc\n"), "Xbc\n");
}

#[test]
fn branch_if_not_sub() {
    // T branches if last sub was NOT successful
    assert_eq!(
        run_sed("s/x/y/;T skip;s/a/SHOULD_NOT/;:skip", "abc\n"),
        "abc\n"
    );
}

// -- l command (list) --

#[test]
fn list_command() {
    assert_eq!(run_sed("l", "a\tb\n"), "a\\tb$\na\tb\n");
}

// -- = command (line number) with address --

#[test]
fn line_number_with_address() {
    assert_eq!(run_sed("2=", "a\nb\nc\n"), "a\n2\nb\nc\n");
}

// -- z (clear pattern) --

#[test]
fn clear_pattern_in_block() {
    assert_eq!(
        run_sed("/hello/{z;s/$/EMPTY/}", "hello\nworld\n"),
        "EMPTY\nworld\n"
    );
}

// -- Multiple commands, complex scripts --

#[test]
fn strip_html_tags() {
    assert_eq!(run_sed("s/<[^>]*>//g", "<b>bold</b>\n"), "bold\n");
}

#[test]
fn sed_multiline_join() {
    // Join all lines with space using N and s
    assert_eq!(
        run_sed(":a;N;s/\\n/ /;$!b a", "one\ntwo\nthree\n"),
        "one two three\n"
    );
}

#[test]
fn double_space() {
    // Classic double-spacing: G appends empty hold to pattern
    assert_eq!(run_sed("G", "a\nb\n"), "a\n\nb\n\n");
}

#[test]
fn delete_empty_lines() {
    assert_eq!(run_sed("/^$/d", "a\n\nb\n\nc\n"), "a\nb\nc\n");
}

#[test]
fn multiple_expressions() {
    // Simulates -e 'cmd1' -e 'cmd2' by joining with newline
    assert_eq!(run_sed("s/a/x/\ns/b/y/", "ab\n"), "xy\n");
}

#[test]
fn nested_blocks() {
    assert_eq!(run_sed("1{/a/{s/a/X/}}", "abc\ndef\n"), "Xbc\ndef\n");
}

// -- Edge cases --

#[test]
fn line_with_no_trailing_newline() {
    // Input without trailing newline
    assert_eq!(run_sed("s/a/X/", "abc"), "Xbc");
}

#[test]
fn single_empty_line() {
    assert_eq!(run_sed("s/^$/EMPTY/", "\n"), "EMPTY\n");
}

#[test]
fn substitute_with_empty_replacement() {
    assert_eq!(run_sed("s/foo//", "foobar\n"), "bar\n");
}

#[test]
fn substitute_with_empty_pattern_match() {
    // Regex that matches empty string at beginning
    assert_eq!(run_sed("s/^/PREFIX: /", "hello\n"), "PREFIX: hello\n");
}

#[test]
fn multiple_ranges_interleaved() {
    // Two separate range commands
    let input = "a\nb\nc\nd\ne\nf\n";
    assert_eq!(run_sed("2,3s/./X/;5,6s/./Y/", input), "a\nX\nX\nd\nY\nY\n");
}

#[test]
fn regex_special_chars() {
    assert_eq!(run_sed("s/\\./X/g", "a.b.c\n"), "aXbXc\n");
}

#[test]
fn regex_anchors() {
    assert_eq!(run_sed("s/^/> /", "hello\n"), "> hello\n");
    assert_eq!(run_sed("s/$/ </", "hello\n"), "hello <\n");
}

// -- q and Q --

#[test]
fn quit_prints_current_line() {
    // q should print the current line before quitting
    assert_eq!(run_sed("2q", "a\nb\nc\n"), "a\nb\n");
}

#[test]
fn quit_no_print() {
    // Q quits without printing pattern space
    assert_eq!(run_sed("2Q", "a\nb\nc\n"), "a\n");
}

// -- Complex real-world scripts --

#[test]
fn remove_trailing_whitespace() {
    assert_eq!(
        run_sed("s/[ \t]*$//", "hello   \nworld\t\t\n"),
        "hello\nworld\n"
    );
}

#[test]
fn number_lines() {
    // Print line number then line (like nl)
    assert_eq!(run_sed("=;s/^/  /", "a\nb\n"), "1\n  a\n2\n  b\n");
}

#[test]
fn print_only_matches() {
    // -n with /pattern/p
    assert_eq!(
        run_sed_opts("/foo/p", "foo\nbar\nfoo2\n", true),
        "foo\nfoo2\n"
    );
}

#[test]
fn delete_first_and_last() {
    assert_eq!(run_sed("1d;$d", "a\nb\nc\n"), "b\n");
}

#[test]
fn change_every_matching_line() {
    assert_eq!(
        run_sed("/old/c new", "old\nkeep\nold\n"),
        "new\nkeep\nnew\n"
    );
}

fn run_sed_ere(script: &str, input: &str) -> String {
    let parsed = command::parse(script).unwrap();
    let options = Options {
        extended_regexp: true,
        ..Options::default()
    };
    let engine = Engine::new(parsed, &options).unwrap();
    let reader = io::Cursor::new(input.as_bytes());
    let mut output = Vec::new();
    engine.process_stream(reader, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

// -- BRE mode (default, GNU-compatible) --

#[test]
fn bre_group_and_backreference() {
    assert_eq!(run_sed(r"s/\(a\)/[\1]/", "abc\n"), "[a]bc\n");
}

#[test]
fn bre_plus_is_literal() {
    assert_eq!(run_sed(r"s/ab+/X/", "abbb\n"), "abbb\n");
    assert_eq!(run_sed(r"s/ab+/X/", "ab+c\n"), "Xc\n");
}

#[test]
fn bre_escaped_plus_is_repetition() {
    assert_eq!(run_sed(r"s/ab\+/X/", "abbb\n"), "X\n");
}

#[test]
fn bre_question_is_literal() {
    assert_eq!(run_sed(r"s/ab?/X/", "ab?c\n"), "Xc\n");
    assert_eq!(run_sed(r"s/ab\?c/X/", "ac\n"), "X\n");
}

#[test]
fn bre_alternation_needs_backslash() {
    assert_eq!(run_sed(r"s/a\|b/X/g", "ab\n"), "XX\n");
    assert_eq!(run_sed(r"s/a|b/X/", "a|b\n"), "X\n");
}

#[test]
fn bre_interval_needs_backslash() {
    assert_eq!(run_sed(r"s/ab\{2\}/X/", "abb\n"), "X\n");
    assert_eq!(run_sed(r"s/a{2}/X/", "a{2}\n"), "X\n");
}

#[test]
fn bre_parens_are_literal() {
    assert_eq!(run_sed(r"s/(a)/X/", "(a)\n"), "X\n");
}

#[test]
fn bre_caret_literal_midpattern() {
    assert_eq!(run_sed(r"s/a^b/X/", "a^b\n"), "X\n");
}

#[test]
fn bre_dollar_literal_midpattern() {
    assert_eq!(run_sed(r"s/a$b/X/", "a$b\n"), "X\n");
}

#[test]
fn bracket_class_with_backslash() {
    // GNU: `[\]` matches a literal backslash
    assert_eq!(run_sed(r"s/[\]/X/", "a\\b\n"), "aXb\n");
}

#[test]
fn bre_address_pattern() {
    assert_eq!(run_sed(r"/ab\+/d", "abbb\nxyz\n"), "xyz\n");
    assert_eq!(run_sed(r"/\(ab\)c/d", "abc\nxyz\n"), "xyz\n");
}

// -- ERE mode (-E) --

#[test]
fn ere_group_and_plus() {
    assert_eq!(run_sed_ere("s/(f)(o+)/\\2\\1/", "foo\n"), "oof\n");
}

#[test]
fn ere_alternation() {
    assert_eq!(run_sed_ere("s/a|b/X/g", "ab\n"), "XX\n");
}

// -- \xHH, \oNNN, \dNNN escapes (GNU) --

#[test]
fn replacement_hex_escape() {
    assert_eq!(run_sed("N; s/\\n/\\x02/g", "a\nb\n"), "a\x02b\n");
}

#[test]
fn replacement_octal_escape() {
    assert_eq!(run_sed("N; s/\\n/\\o002/g", "a\nb\n"), "a\x02b\n");
}

#[test]
fn replacement_decimal_escape() {
    assert_eq!(run_sed("N; s/\\n/\\d002/g", "a\nb\n"), "a\x02b\n");
}

#[test]
fn replacement_hex_ampersand_is_literal() {
    // \x26 is '&' but must not expand to the whole match
    assert_eq!(run_sed(r"s/ab/\x26/", "ab\n"), "&\n");
}

#[test]
fn pattern_hex_escape() {
    assert_eq!(run_sed(r"s/\x41/Z/", "A\n"), "Z\n");
}

#[test]
fn pattern_octal_escape() {
    assert_eq!(run_sed(r"s/\o101/Z/", "A\n"), "Z\n");
}

#[test]
fn pattern_decimal_escape() {
    assert_eq!(run_sed(r"s/\d065/Z/", "A\n"), "Z\n");
}
