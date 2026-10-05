//! Width policy and plain-text rendering shared by every ps output format.

use crate::{
    Args,
    fields::Align,
    format::{FieldSpec, suppress_headers},
};
use std::{
    env,
    ffi::{CStr, CString},
    fs::OpenOptions,
    io::{self, IsTerminal, Write},
    os::{fd::AsRawFd, unix::ffi::OsStrExt, unix::fs::OpenOptionsExt},
};
use unicode_width::UnicodeWidthChar;

// procps-ng's output buffer also bounds its nominally unlimited output.
const OUTPUT_LIMIT: usize = 128 * 1024;

pub(super) fn parse_width(value: &str) -> Result<usize, String> {
    let value = value.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let value = value.strip_prefix('+').unwrap_or(value);
    let (radix, digits) = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        (16, hex)
    } else if value.starts_with('0') && value.len() > 1 {
        (8, value)
    } else {
        (10, value)
    };
    if !digits.starts_with(['+', '-']) {
        if let Ok(width) = u32::from_str_radix(digits, radix) {
            if width > 0 && width < 2_000_000_000 {
                return Ok(width as usize);
            }
        }
    }
    Err("width must be a positive number of columns below 2000000000".into())
}

pub(super) fn width(args: &Args) -> Result<Option<usize>, String> {
    let explicit = args
        .width
        .iter()
        .try_fold(None, |_, value| parse_width(value).map(Some))?;
    let wide = args
        .bsd_options
        .iter()
        .fold(args.w, |count, options| count.saturating_add(options.wide));
    if wide > 1 {
        return Ok(None);
    }
    let columns = explicit.or_else(|| {
        env::var("COLUMNS")
            .ok()
            .and_then(|value| parse_width(&value).ok())
            .filter(|&value| value < OUTPUT_LIMIT)
    });
    let columns = columns.or_else(|| {
        // Like procps, redirected stdout is unlimited even if another fd or
        // the controlling terminal has a usable window size.
        io::stdout()
            .is_terminal()
            .then(|| terminal_width().unwrap_or(80))
    });
    Ok(columns.map(|columns| if wide == 1 { columns.max(132) } else { columns }))
}

fn terminal_width() -> Option<usize> {
    for fd in [
        io::stdout().as_raw_fd(),
        io::stderr().as_raw_fd(),
        io::stdin().as_raw_fd(),
    ] {
        if let Some(width) = fd_width(fd) {
            return Some(width);
        }
    }
    let tty = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open("/dev/tty")
        .ok()?;
    fd_width(tty.as_raw_fd())
}

fn fd_width(fd: i32) -> Option<usize> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes a correctly aligned, initialized winsize;
    // the standard fd or borrowed File remains owned by its caller.
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) };
    (result == 0 && size.ws_col > 0 && size.ws_row > 0).then_some(size.ws_col as usize)
}

fn utf8_locale() -> bool {
    let name = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(env::var_os)
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "C".into());
    let Ok(name) = CString::new(name.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: name is NUL-terminated; a null base creates an owned locale.
    // This never changes the process-global locale used by AXE's other tasks.
    let locale =
        unsafe { libc::newlocale(libc::LC_CTYPE_MASK, name.as_ptr(), std::ptr::null_mut()) };
    if locale.is_null() {
        return false;
    }
    // SAFETY: locale is valid until freelocale below. CODESET returns a
    // NUL-terminated string owned by that locale, which is only borrowed here.
    let codeset = unsafe { libc::nl_langinfo_l(libc::CODESET, locale) };
    let utf8 = !codeset.is_null()
        // SAFETY: the non-null CODESET string is valid while locale is alive.
        && unsafe { CStr::from_ptr(codeset) }
            .to_bytes()
            .eq_ignore_ascii_case(b"UTF-8");
    // SAFETY: this locale was created above and no borrowed string is retained.
    unsafe { libc::freelocale(locale) };
    utf8
}

fn command_column(spec: &FieldSpec) -> bool {
    matches!(spec.field.name, "args" | "comm")
}

struct DisplayUnit<'a> {
    start: usize,
    end: usize,
    replacement: Option<&'a str>,
    columns: usize,
}

fn units(text: &str, command_line: bool, utf8: bool) -> impl Iterator<Item = DisplayUnit<'_>> {
    text.char_indices().map(move |(start, ch)| {
        let end = start + ch.len_utf8();
        let (replacement, columns) = if command_line && ch == '\n' {
            // read_unvectored in procps replaces command-line newlines, not
            // tabs, with spaces before its printable-character sanitizing.
            (Some(" "), 1)
        } else if (!utf8 && !(' '..='~').contains(&ch))
            || ch.is_control()
            || matches!(ch as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd)
        {
            // The procps acquisition layer replaces unsafe bytes with '?'.
            // In the C locale each non-ASCII UTF-8 byte is a separate cell.
            let columns = ch.len_utf8();
            (Some(&"????"[..columns]), columns)
        } else if let Some(columns) = ch.width() {
            (None, columns)
        } else {
            (Some("?"), 1)
        };
        DisplayUnit {
            start,
            end,
            replacement,
            columns,
        }
    })
}

fn text_width(text: &str, command_line: bool, utf8: bool, limit: usize) -> usize {
    let mut width = 0;
    for unit in units(text, command_line, utf8) {
        width += unit.columns.min(limit - width);
        if width == limit {
            break;
        }
    }
    width
}

fn write_text(
    out: &mut impl Write,
    text: &str,
    command_line: bool,
    utf8: bool,
    limit: usize,
) -> io::Result<usize> {
    let mut width = 0;
    let mut start = 0;
    let mut end = 0;
    for unit in units(text, command_line, utf8) {
        if width == limit {
            break;
        }
        if unit.columns > limit - width {
            out.write_all(&text.as_bytes()[start..unit.start])?;
            if let Some(replacement) = unit.replacement {
                out.write_all(&replacement.as_bytes()[..limit - width])?;
                width = limit;
            }
            return Ok(width);
        }
        if let Some(replacement) = unit.replacement {
            out.write_all(&text.as_bytes()[start..unit.start])?;
            out.write_all(replacement.as_bytes())?;
            start = unit.end;
        }
        end = unit.end;
        width += unit.columns;
    }
    out.write_all(&text.as_bytes()[start..end])?;
    Ok(width)
}

fn write_spaces(out: &mut impl Write, mut count: usize) -> io::Result<()> {
    const SPACES: &[u8] = b"                                                                ";
    while count > 0 {
        let chunk = count.min(SPACES.len());
        out.write_all(&SPACES[..chunk])?;
        count -= chunk;
    }
    Ok(())
}

pub(super) fn write_table(
    specs: &[FieldSpec],
    rows: &[(i32, Vec<String>)],
    width: Option<usize>,
    out: &mut impl Write,
) -> io::Result<()> {
    let utf8 = utf8_locale();
    let limit = width.unwrap_or(OUTPUT_LIMIT).max(1);
    let mut widths: Vec<usize> = specs
        .iter()
        .map(|spec| {
            text_width(
                spec.label.as_deref().unwrap_or(spec.field.header),
                false,
                utf8,
                usize::MAX,
            )
        })
        .collect();
    for (_, cells) in rows {
        for (index, (spec, cell)) in specs.iter().zip(cells).enumerate() {
            let cell_width = text_width(cell, spec.field.name == "args", utf8, OUTPUT_LIMIT - 1);
            widths[index] = widths[index].max(cell_width);
        }
    }

    // Fixed fields are never discarded or clipped to fit a narrow screen.
    // procps expands to a multiple of the screen width when the fixed layout
    // plus its command minimum will not fit. Keep the registry's existing
    // dynamic widths rather than introducing a second field-width registry.
    let minimum = specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            if command_column(spec) {
                if index + 1 == specs.len() {
                    3
                } else {
                    text_width(
                        spec.label.as_deref().unwrap_or(spec.field.header),
                        false,
                        utf8,
                        usize::MAX,
                    )
                    .max(3)
                }
            } else {
                widths[index]
            }
        })
        .sum::<usize>()
        + specs.len().saturating_sub(1);
    let active = minimum.div_ceil(limit).max(1).saturating_mul(limit);
    let mut available = active.saturating_sub(minimum);
    for (index, spec) in specs.iter().enumerate() {
        if command_column(spec) {
            let minimum = if index + 1 == specs.len() {
                3
            } else {
                text_width(
                    spec.label.as_deref().unwrap_or(spec.field.header),
                    false,
                    utf8,
                    usize::MAX,
                )
                .max(3)
            };
            let extra = widths[index].saturating_sub(minimum).min(available);
            widths[index] = minimum + extra;
            available -= extra;
        }
    }

    if !suppress_headers(specs) {
        write_row(
            specs,
            &widths,
            specs
                .iter()
                .map(|spec| spec.label.as_deref().unwrap_or(spec.field.header)),
            utf8,
            true,
            out,
        )?;
    }
    for (_, cells) in rows {
        write_row(
            specs,
            &widths,
            cells.iter().map(String::as_str),
            utf8,
            false,
            out,
        )?;
    }
    Ok(())
}

fn write_row<'a>(
    specs: &[FieldSpec],
    widths: &[usize],
    cells: impl Iterator<Item = &'a str>,
    utf8: bool,
    header: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    for (index, ((spec, &width), cell)) in specs.iter().zip(widths).zip(cells).enumerate() {
        if index > 0 {
            out.write_all(b" ")?;
        }
        let command_line = !header && spec.field.name == "args";
        let limit = if !header && command_column(spec) {
            width
        } else {
            usize::MAX
        };
        if matches!(spec.field.align, Align::Right) {
            write_spaces(
                out,
                width.saturating_sub(text_width(cell, command_line, utf8, limit)),
            )?;
        }
        let printed = write_text(out, cell, command_line, utf8, limit)?;
        if index + 1 < specs.len() && matches!(spec.field.align, Align::Left) {
            write_spaces(out, width.saturating_sub(printed))?;
        }
    }
    out.write_all(b"\n")
}
