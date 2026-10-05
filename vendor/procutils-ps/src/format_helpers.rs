// AXE modification: fixed formats reuse the field registry's time helpers.

//! Cell-formatting helpers shared between the fixed-format output
//! path and the dynamic `-o` field registry. Pure functions — no I/O,
//! no global state.

use procfs::process::Stat;

/// Format a percentage already truncated to tenths, without rounding.
pub(super) fn format_percent_tenths(tenths: u64) -> String {
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Procps `%cpu` truncates to tenths and omits the fraction above 99.9%.
/// Unlike the `C` column, multi-core CPU usage is not capped.
pub(super) fn format_cpu_percent(percent: f64) -> String {
    let tenths = (percent * 10.0) as u64;
    if tenths > 999 {
        (tenths / 10).to_string()
    } else {
        format_percent_tenths(tenths)
    }
}

pub fn tty_name(tty_nr: i32) -> String {
    if tty_nr == 0 {
        return "?".into();
    }
    let major = ((tty_nr >> 8) & 0xff) as u32;
    let minor = ((tty_nr & 0xff) | ((tty_nr >> 12) & 0xfff00)) as u32;
    match major {
        4 if minor < 64 => format!("tty{minor}"),
        4 => format!("ttyS{}", minor - 64),
        136..=143 => format!("pts/{}", (major - 136) * 256 + minor),
        _ => "?".into(),
    }
}

/// Always `HH:MM:SS` with leading zeroes (procps `cputime`, `time`).
pub fn format_cputime(ticks: u64, tps: u64) -> String {
    let total_secs = ticks / tps.max(1);
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    format!("{hours:02}:{mins:02}:{secs:02}")
}

/// `etime` format: `[[DD-]HH:]MM:SS`. Procps drops leading components
/// that are zero.
pub fn format_elapsed(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{days}-{hours:02}:{mins:02}:{s:02}")
    } else if hours > 0 {
        format!("{hours:02}:{mins:02}:{s:02}")
    } else {
        format!("{mins:02}:{s:02}")
    }
}

/// `etimes` format: integer seconds since process start.
pub fn format_etimes(secs: u64) -> String {
    secs.to_string()
}

/// `cputimes` / `times` format: integer seconds of CPU time consumed.
/// Distinct from `cputime` (HH:MM:SS) and `bsdtime` (MMM:SS).
pub fn format_cputimes(ticks: u64, tps: u64) -> String {
    (ticks / tps.max(1)).to_string()
}

/// `bsdtime` format: `MMM:SS` — CPU time as minutes:seconds, with no
/// hour rollover (so a process with 65 minutes of CPU prints as
/// ` 65:00`). Right-aligned by the column infrastructure.
pub fn format_bsdtime(ticks: u64, tps: u64) -> String {
    let secs = ticks / tps.max(1);
    let mins = secs / 60;
    let s = secs % 60;
    format!("{mins:>3}:{s:02}")
}

/// `lstart` format: full local-time stamp in
/// `Day Mon DD HH:MM:SS YYYY` form (e.g. `Sat May  3 14:23:45 2025`).
/// Day-of-week is computed from the epoch (1970-01-01 was a Thursday).
pub fn format_lstart(starttime_ticks: u64, boot_time_secs: u64, tps: u64) -> String {
    let start_secs = boot_time_secs + starttime_ticks / tps.max(1);
    let local_offset = local_utc_offset_secs();
    let local_secs = (start_secs as i64 + local_offset) as u64;

    let days_since_epoch = local_secs / 86400;
    let secs_of_day = local_secs % 86400;

    // 1970-01-01 was a Thursday → day-of-week 4 (with Sun=0).
    let dow = (days_since_epoch + 4) % 7;
    let dow_name = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][dow as usize];

    let (year, month_idx, day) = epoch_days_to_year_month_day(days_since_epoch);
    let month_name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][month_idx as usize];

    let h = secs_of_day / 3600;
    let m = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;

    format!("{dow_name} {month_name} {day:>2} {h:02}:{m:02}:{s:02} {year}")
}

/// Same date breakdown as `epoch_days_to_month_day` but also returns
/// the year. Public so `format_lstart` can use it; the simpler helper
/// stays for the compact formats.
fn epoch_days_to_year_month_day(days: u64) -> (i64, u32, u32) {
    let mut y = 1970i64;
    let mut remaining = days as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = is_leap(y);
    let month_days = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 0u32;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        month += 1;
    }
    (y, month, remaining as u32 + 1)
}

/// `start` / `stime` format: `HH:MM` if started today, `MmmDD`
/// otherwise. Matches procps's "compact" rendering.
pub fn format_start_compact(starttime_ticks: u64, boot_time_secs: u64, tps: u64) -> String {
    let start_secs = boot_time_secs + starttime_ticks / tps.max(1);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let elapsed = now.saturating_sub(start_secs);

    if elapsed < 86400 {
        let secs_of_day = start_secs % 86400;
        let local_offset = local_utc_offset_secs();
        let local_secs = (secs_of_day as i64 + local_offset).rem_euclid(86400) as u64;
        let h = local_secs / 3600;
        let m = (local_secs % 3600) / 60;
        format!("{h:02}:{m:02}")
    } else {
        let days_since_epoch = start_secs / 86400;
        let (month, day) = epoch_days_to_month_day(days_since_epoch);
        let months = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        format!("{}{day:02}", months[month as usize])
    }
}

fn local_utc_offset_secs() -> i64 {
    if let Ok(tz) = std::env::var("TZ")
        && let Ok(offset_hrs) = tz
            .chars()
            .skip_while(|c| c.is_alphabetic())
            .take_while(|c| *c == '-' || *c == '+' || c.is_ascii_digit())
            .collect::<String>()
            .parse::<i64>()
    {
        return -offset_hrs * 3600;
    }
    0
}

fn epoch_days_to_month_day(days: u64) -> (u32, u32) {
    let mut y = 1970i64;
    let mut remaining = days as i64;

    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }

    let leap = is_leap(y);
    let month_days = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 0u32;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        month += 1;
    }

    (month, remaining as u32 + 1)
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// `STAT` field: state char plus modifier letters (`<`, `N`, `s`, `l`, `+`).
pub fn format_stat(state: char, stat: &Stat) -> String {
    let mut s = String::new();
    s.push(state);

    if stat.nice < 0 {
        s.push('<');
    } else if stat.nice > 0 {
        s.push('N');
    }

    if stat.session == stat.pid {
        s.push('s');
    }

    if stat.num_threads > 1 {
        s.push('l');
    }

    if stat.tpgid == stat.pgrp {
        s.push('+');
    }

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_picks_smallest_form() {
        assert_eq!(format_elapsed(0), "00:00");
        assert_eq!(format_elapsed(59), "00:59");
        assert_eq!(format_elapsed(60), "01:00");
        assert_eq!(format_elapsed(3600), "01:00:00");
        assert_eq!(format_elapsed(86400 + 7322), "1-02:02:02");
    }

    #[test]
    fn cputime_always_three_components() {
        assert_eq!(format_cputime(0, 100), "00:00:00");
        assert_eq!(format_cputime(360_000, 100), "01:00:00");
    }

    #[test]
    fn tty_console_devices() {
        assert_eq!(tty_name(0), "?");
        assert_eq!(tty_name(0x0401), "tty1");
        assert_eq!(tty_name(0x0440), "ttyS0");
    }
}
