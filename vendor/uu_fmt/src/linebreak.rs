// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::io::{BufWriter, Stdout, Write};

use crate::FmtOptions;
use crate::parasplit::{ParaWords, Paragraph, WordInfo};

struct Word<'a> {
    info: &'a WordInfo<'a>,
    space_before: usize,
    period: bool,
    sentence_final: bool,
    punctuation: bool,
    opening: bool,
}

#[derive(Clone, Copy, Default)]
struct Break {
    next: usize,
    length: usize,
    cost: i64,
}

fn square(value: i64) -> i64 {
    value.saturating_mul(value)
}

fn base_cost(words: &[Word<'_>], index: usize) -> i64 {
    let word = &words[index];
    let mut cost = 4900;
    if let Some(previous) = index.checked_sub(1).map(|i| &words[i]) {
        if previous.period {
            cost += if previous.sentence_final { -2500 } else { 360_000 };
        } else if previous.punctuation {
            cost -= 1600;
        } else if index > 1 && words[index - 2].sentence_final {
            cost += 40_000 / (previous.info.word_nchars as i64 + 2);
        }
    }
    if word.opening {
        cost -= 1600;
    } else if word.sentence_final {
        cost += 22_500 / (word.info.word_nchars as i64 + 2);
    }
    cost
}

fn optimal_breaks(words: &[Word<'_>], opts: &FmtOptions, first: usize, indent: usize) -> Vec<Break> {
    let mut breaks = vec![Break::default(); words.len() + 1];
    for start in (0..words.len()).rev() {
        let mut length = if start == 0 { first } else { indent };
        length = length.saturating_add(words[start].info.word_nchars);
        let mut best = Break {
            next: start + 1,
            length,
            cost: i64::MAX,
        };
        for next in start + 1..=words.len() {
            let mut cost = breaks[next].cost;
            if next < words.len() {
                cost = cost.saturating_add(
                    square(opts.goal as i64 - length as i64).saturating_mul(100),
                );
                if breaks[next].next < words.len() {
                    cost = cost.saturating_add(
                        square(length as i64 - breaks[next].length as i64).saturating_mul(50),
                    );
                }
            }
            if cost < best.cost {
                best = Break { next, length, cost };
            }
            if next == words.len() {
                break;
            }
            length = length
                .saturating_add(words[next].space_before)
                .saturating_add(words[next].info.word_nchars);
            if length > opts.width {
                break;
            }
        }
        best.cost = best.cost.saturating_add(base_cost(words, start));
        breaks[start] = best;
    }
    breaks
}

fn write_spaces(
    ostream: &mut BufWriter<Stdout>,
    column: &mut usize,
    count: usize,
    tabs: bool,
    tabwidth: usize,
) -> std::io::Result<()> {
    let goal = column.saturating_add(count);
    let tab_goal = goal / tabwidth * tabwidth;
    if tabs && column.saturating_add(1) < tab_goal {
        while *column < tab_goal {
            ostream.write_all(b"\t")?;
            *column = (*column / tabwidth + 1) * tabwidth;
        }
    }
    let spaces = [b' '; 64];
    while *column < goal {
        let count = (goal - *column).min(spaces.len());
        ostream.write_all(&spaces[..count])?;
        *column += count;
    }
    Ok(())
}

fn write_indent(
    ostream: &mut BufWriter<Stdout>,
    indent: &[u8],
    tabs: bool,
    tabwidth: usize,
) -> std::io::Result<usize> {
    let mut column = 0;
    let mut position = 0;
    while position < indent.len() {
        let mut goal = column;
        while let Some(&byte) = indent.get(position) {
            match byte {
                b' ' => goal += 1,
                b'\t' => goal = (goal / tabwidth + 1) * tabwidth,
                _ => break,
            }
            position += 1;
        }
        let count = goal - column;
        write_spaces(ostream, &mut column, count, tabs, tabwidth)?;
        let start = position;
        while position < indent.len() && !matches!(indent[position], b' ' | b'\t') {
            position += 1;
        }
        ostream.write_all(&indent[start..position])?;
        column += position - start;
    }
    Ok(column)
}

pub fn write_unformatted(
    bytes: &[u8],
    tabs: bool,
    tabwidth: usize,
    ostream: &mut BufWriter<Stdout>,
) -> std::io::Result<()> {
    let indent = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(bytes.len());
    write_indent(ostream, &bytes[..indent], tabs, tabwidth)?;
    ostream.write_all(&bytes[indent..])
}

pub fn break_lines(
    para: &Paragraph,
    opts: &FmtOptions,
    ostream: &mut BufWriter<Stdout>,
) -> std::io::Result<()> {
    let para_words = ParaWords::new(opts, para);
    let infos = para_words.words().as_slice();
    let first_indent = if opts.crown || opts.tagged {
        &para.init_str
    } else {
        &para.indent_str
    };
    let first_length = if opts.crown || opts.tagged {
        para.init_len
    } else {
        para.indent_len
    };
    let uniform = opts.uniform || para.mail_header;
    let mut words = Vec::with_capacity(infos.len());
    let mut source_column = first_length;
    for (index, info) in infos.iter().enumerate() {
        if info.new_line {
            source_column = if index == 0 { first_length } else { para.indent_len };
        }
        let original_space = match info.before_tab {
            Some(before) => {
                (source_column + before) / opts.tabwidth * opts.tabwidth
                    + opts.tabwidth
                    + info.after_tab
                    - source_column
            }
            None => info.after_tab,
        };
        source_column += original_space + info.word_nchars;
        let space_before = if index == 0 {
            0
        } else if uniform || info.new_line {
            usize::from(info.sentence_start || (info.new_line && infos[index - 1].ends_punct)) + 1
        } else {
            original_space
        };
        let text = &info.word[info.word_start..];
        let unclosed = text
            .iter()
            .rposition(|byte| !matches!(byte, b')' | b']' | b'\'' | b'"'))
            .map_or(text, |end| &text[..=end]);
        let period = unclosed.last().is_some_and(|byte| matches!(byte, b'.' | b'?' | b'!'));
        let sentence_final = infos.get(index + 1).is_none_or(|next| {
            period && (next.new_line || next.sentence_start)
        });
        words.push(Word {
            info,
            space_before,
            period,
            sentence_final,
            punctuation: text.last().is_some_and(u8::is_ascii_punctuation),
            opening: text.first().is_some_and(|byte| matches!(byte, b'(' | b'[' | b'\'' | b'`' | b'"')),
        });
    }
    if words.is_empty() {
        return ostream.write_all(b"\n");
    }
    let breaks = if opts.quick || para.mail_header {
        Vec::new()
    } else {
        optimal_breaks(&words, opts, first_length, para.indent_len)
    };
    let mut start = 0;
    while start < words.len() {
        let indent = if start == 0 { first_indent } else { &para.indent_str };
        let mut column = write_indent(ostream, indent, para.tabs, opts.tabwidth)?;
        let end = if opts.quick || para.mail_header {
            let mut end = start + 1;
            let mut length = if start == 0 { first_length } else { para.indent_len }
                + words[start].info.word_nchars;
            while end < words.len()
                && length + words[end].space_before + words[end].info.word_nchars <= opts.width
            {
                length += words[end].space_before + words[end].info.word_nchars;
                end += 1;
            }
            end
        } else {
            breaks[start].next
        };
        for (index, word) in words.iter().enumerate().take(end).skip(start) {
            if index > start {
                write_spaces(
                    ostream,
                    &mut column,
                    word.space_before,
                    para.tabs,
                    opts.tabwidth,
                )?;
            }
            let info = word.info;
            ostream.write_all(&info.word[info.word_start..])?;
            column += info.word_nchars;
        }
        ostream.write_all(b"\n")?;
        start = end;
    }
    Ok(())
}
