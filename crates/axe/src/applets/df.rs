//! GNU coreutils-compatible `df`.

use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// GNU's notion of a remote file system: type-based, since statvfs does not
/// expose the mountlist REMOTE flag.
fn is_remote_fs(fs_type: &str, device: &str) -> bool {
    matches!(
        fs_type,
        "nfs" | "nfs4" | "cifs" | "smbfs" | "smb2" | "ncpfs" | "sshfs" | "fuse.sshfs"
    ) || device.starts_with(':')
        || (!device.starts_with('/') && device.contains("://"))
}

struct MountRow {
    device: String,
    /// The mount's root within its source filesystem (mountinfo field 4).
    root: String,
    mountpoint: PathBuf,
    fs_type: String,
    stats: libc::statvfs,
    /// st_dev of the mount point; bind mounts share it with their source.
    dev: Option<u64>,
}

#[derive(Default)]
struct DfOptions {
    all: bool,
    inodes: bool,
    print_type: bool,
    local: bool,
    total: bool,
    portability: bool,
    block_size: Option<String>,
    human: Option<HumanBase>,
    include_types: Vec<String>,
    exclude_types: Vec<String>,
    outputs: Option<Vec<String>>,
    files: Vec<PathBuf>,
}

#[derive(Clone, Copy)]
enum HumanBase {
    /// -h: powers of 1024.
    Kibi,
    /// -H: powers of 1000.
    Kilo,
}

pub fn df(args: Vec<std::ffi::OsString>) -> i32 {
    match run_df(args) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("df: {message}");
            1
        }
    }
}

fn run_df(args: Vec<std::ffi::OsString>) -> Result<(), String> {
    let mut options = DfOptions::default();
    let mut iter = args.into_iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        let value = arg.to_string_lossy().into_owned();
        match value.as_str() {
            "-a" | "--all" => options.all = true,
            "-i" | "--inodes" => options.inodes = true,
            "-T" | "--print-type" => options.print_type = true,
            "-l" | "--local" => options.local = true,
            "--total" => options.total = true,
            "-P" | "--portability" => options.portability = true,
            "--direct" | "--sync" | "--no-sync" => {}
            "-h" | "--human-readable" => options.human = Some(HumanBase::Kibi),
            "-H" | "--si" => options.human = Some(HumanBase::Kilo),
            "-k" => options.block_size = Some("1K".into()),
            "-t" | "--type" => options.include_types.push(take_value(&mut iter, &value)?),
            "-x" | "--exclude-type" => options.exclude_types.push(take_value(&mut iter, &value)?),
            "-B" | "--block-size" => options.block_size = Some(take_value(&mut iter, &value)?),
            "--output" => options.outputs = Some(Vec::new()),
            value if value.starts_with("--output=") => {
                options.outputs = Some(
                    value["--output=".len()..]
                        .split(',')
                        .map(str::to_owned)
                        .collect(),
                );
            }
            "--help" => {
                println!("Usage: df [OPTION]... [FILE]...");
                return Ok(());
            }
            "--version" => {
                println!("df from axe {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            value if value.starts_with('-') && value.len() > 1 && !value.starts_with("--") => {
                for flag in value[1..].chars() {
                    match flag {
                        'a' => options.all = true,
                        'i' => options.inodes = true,
                        'T' => options.print_type = true,
                        'l' => options.local = true,
                        'k' => options.block_size = Some("1K".into()),
                        'P' => options.portability = true,
                        'v' => {}
                        'h' => options.human = Some(HumanBase::Kibi),
                        'H' => options.human = Some(HumanBase::Kilo),
                        other => return Err(format!("invalid option -- '{other}'")),
                    }
                }
            }
            _ => options.files.push(PathBuf::from(value)),
        }
    }
    print_mounts(&options)
}

fn take_value(
    iter: &mut std::iter::Peekable<std::iter::Skip<std::vec::IntoIter<std::ffi::OsString>>>,
    flag: &str,
) -> Result<String, String> {
    iter.next()
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| format!("option '{flag}' requires an argument"))
}

fn print_mounts(options: &DfOptions) -> Result<(), String> {
    let columns = resolve_columns(options);
    let rows = collect_rows(options)?;
    let mut prepared: Vec<Vec<String>> = rows
        .iter()
        .map(|row| format_row(row, options, &columns))
        .collect::<Result<_, String>>()?;
    if options.total {
        let sums = sum_rows(&rows);
        let total = format_row(&sums, options, &columns)?;
        prepared.push(total);
    }
    let headers: Vec<String> = columns
        .iter()
        .map(|column| size_header(column, options))
        .collect();
    let mut widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| min_column_width(column).max(headers[index].len()))
        .collect();
    for row in &prepared {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.len());
        }
    }
    print_line(&headers, &columns, &widths, options);
    for row in &prepared {
        print_line(row, &columns, &widths, options);
    }
    Ok(())
}

/// One line with GNU df's alignment: source and type left-justified, numbers
/// right-justified, the mount point flush left.
fn print_line(cells: &[String], columns: &[String], widths: &[usize], _options: &DfOptions) {
    let last = cells.len().saturating_sub(1);
    let mut line = String::new();
    for (index, cell) in cells.iter().enumerate() {
        let width = widths[index];
        let left_aligned = matches!(columns[index].as_str(), "source" | "fstype" | "file");
        if index == last && columns[last] == "target" {
            line.push_str(cell);
        } else if left_aligned {
            line.push_str(&format!("{cell:<width$}"));
        } else {
            line.push_str(&format!("{cell:>width$}"));
        }
        if index != last {
            line.push(' ');
        }
    }
    println!("{line}");
}

/// GNU df's minimum column widths.
fn min_column_width(column: &str) -> usize {
    match column {
        "source" => 14,
        "fstype" => 4,
        "size" | "used" | "avail" => 5,
        "pcent" => 4,
        "itotal" | "iused" | "iavail" => 5,
        "ipcent" => 4,
        _ => 0,
    }
}

/// A synthetic "total" row summing the displayed filesystems, as --total does.
fn sum_rows(rows: &[MountRow]) -> MountRow {
    // SAFETY: statvfs is a plain data struct; an all-zero value is a valid base.
    let mut stats = unsafe { std::mem::zeroed::<libc::statvfs>() };
    let mut block_size = 0;
    for row in rows {
        block_size = block_size.max(row.stats.f_bsize);
        stats.f_blocks = stats.f_blocks.saturating_add(row.stats.f_blocks);
        stats.f_bfree = stats.f_bfree.saturating_add(row.stats.f_bfree);
        stats.f_bavail = stats.f_bavail.saturating_add(row.stats.f_bavail);
        stats.f_files = stats.f_files.saturating_add(row.stats.f_files);
        stats.f_ffree = stats.f_ffree.saturating_add(row.stats.f_ffree);
    }
    stats.f_bsize = if block_size == 0 { 1 } else { block_size };
    stats.f_frsize = stats.f_bsize;
    MountRow {
        device: "total".into(),
        root: String::new(),
        mountpoint: PathBuf::from("-"),
        fs_type: String::new(),
        stats,
        dev: None,
    }
}

/// The columns GNU df prints for the given flags, in order.
fn resolve_columns(options: &DfOptions) -> Vec<String> {
    if let Some(requested) = &options.outputs {
        return requested.clone();
    }
    let mut columns = vec!["source"];
    if options.print_type {
        columns.push("fstype");
    }
    if options.inodes {
        columns.extend(["itotal", "iused", "iavail", "ipcent"]);
    } else {
        columns.extend(["size", "used", "avail", "pcent"]);
    }
    columns.push("target");
    columns.into_iter().map(str::to_owned).collect()
}

fn header_name(column: &str) -> &'static str {
    match column {
        "source" => "Filesystem",
        "fstype" => "Type",
        "size" => "1K-blocks",
        "used" => "Used",
        "avail" => "Available",
        "pcent" => "Use%",
        "itotal" => "Inodes",
        "iused" => "IUsed",
        "iavail" => "IFree",
        "ipcent" => "IUse%",
        "target" => "Mounted on",
        _ => "File",
    }
}

/// The header for a column, honoring -B, human units, and POSIX mode.
fn size_header(column: &str, options: &DfOptions) -> String {
    if column == "pcent" && options.portability && options.human.is_none() {
        return "Capacity".into();
    }
    if column != "size" && column != "avail" {
        return header_name(column).to_string();
    }
    if options.human.is_some() {
        return match column {
            "size" => "Size".into(),
            _ => "Avail".into(),
        };
    }
    if options.portability && column == "size" {
        let bytes = options
            .block_size
            .as_deref()
            .and_then(parse_block_size)
            .unwrap_or(1024);
        return format!("{bytes}-blocks");
    }
    match options.block_size.as_deref().and_then(parse_block_spec) {
        Some((number, unit)) if column == "size" => format!("{number}{unit}-blocks"),
        _ => header_name(column).to_string(),
    }
}

/// Splits a block-size spec like "1M" into its printed parts.
fn parse_block_spec(spec: &str) -> Option<(&str, &str)> {
    let split = spec
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(spec.len());
    let (number, scale) = spec.split_at(split);
    let number = if number.is_empty() { "1" } else { number };
    Some((number, scale))
}

fn format_row(
    row: &MountRow,
    options: &DfOptions,
    columns: &[String],
) -> Result<Vec<String>, String> {
    let used = row.stats.f_blocks.saturating_sub(row.stats.f_bfree);
    let available = row.stats.f_bavail;
    let iused = row.stats.f_files.saturating_sub(row.stats.f_ffree);
    columns
        .iter()
        .map(|column| match column.as_str() {
            "source" => Ok(row.device.clone()),
            "fstype" => Ok(row.fs_type.clone()),
            "target" => Ok(row.mountpoint.display().to_string()),
            "size" => Ok(format_capacity(
                row.stats.f_blocks,
                row.stats.f_bsize,
                options,
            )),
            "used" => Ok(format_capacity(used, row.stats.f_bsize, options)),
            "avail" => Ok(format_capacity(available, row.stats.f_bsize, options)),
            "pcent" => Ok(percent(used, used.saturating_add(available))),
            "itotal" => Ok(row.stats.f_files.to_string()),
            "iused" => Ok(iused.to_string()),
            "iavail" => Ok(row.stats.f_ffree.to_string()),
            "ipcent" => Ok(percent(iused, row.stats.f_files)),
            "file" => Ok(String::new()),
            other => Err(format!("unsupported column '{other}'")),
        })
        .collect()
}

fn collect_rows(options: &DfOptions) -> Result<Vec<MountRow>, String> {
    if options.files.is_empty() {
        let mut rows = read_mounts();
        filter_mounts(&mut rows, options);
        return Ok(rows);
    }
    // When FILE arguments name files on a filesystem, report that filesystem;
    // GNU matches by the file's st_dev, deepest mount wins.
    let mounts = read_mounts();
    let mut rows = Vec::new();
    for file in &options.files {
        let dev = std::fs::metadata(file)
            .map_err(|error| format!("{}: {}", file.display(), error))?
            .dev();
        let canonical = std::fs::canonicalize(file)
            .map_err(|error| format!("{}: {}", file.display(), error))?;
        let found = mounts
            .iter()
            .filter(|row| row.dev == Some(dev) && canonical.starts_with(&row.mountpoint))
            .max_by_key(|row| row.mountpoint.components().count())
            .ok_or_else(|| format!("{}: no file system mounted there", file.display()))?;
        rows.push(MountRow {
            device: found.device.clone(),
            mountpoint: found.mountpoint.clone(),
            fs_type: found.fs_type.clone(),
            stats: statvfs(&found.mountpoint)
                .ok_or_else(|| format!("{}: cannot read statistics", found.mountpoint.display()))?,
            dev: found.dev,
            root: found.root.clone(),
        });
    }
    Ok(rows)
}

fn read_mounts() -> Vec<MountRow> {
    let mut content = String::new();
    if File::open("/proc/self/mountinfo")
        .and_then(|mut file| file.read_to_string(&mut content))
        .is_err()
    {
        return Vec::new();
    }
    content
        .lines()
        .filter_map(|line| {
            // id parent maj:min root mountpoint [options]... - fstype source superopts
            let separator = line.find(" - ")?;
            let mut fields = line[..separator].split_whitespace();
            fields.next()?; // mount id
            fields.next()?; // parent id
            fields.next()?; // major:minor
            let root = fields.next()?;
            let mountpoint = fields.next()?;
            let mut right = line[separator + 3..].split_whitespace();
            let fs_type = right.next()?;
            let device = right.next()?;
            let mountpoint = PathBuf::from(Path::new(&unescape(mountpoint)));
            let stats = statvfs(&mountpoint)?;
            let dev = std::fs::metadata(&mountpoint).ok().map(|meta| meta.dev());
            Some(MountRow {
                device: unescape(device),
                root: unescape(root),
                mountpoint,
                fs_type: fs_type.to_owned(),
                stats,
                dev,
            })
        })
        .collect()
}

fn statvfs(path: &Path) -> Option<libc::statvfs> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = unsafe { std::mem::zeroed::<libc::statvfs>() };
    // SAFETY: `path` is a valid NUL-terminated path for the call.
    (unsafe { libc::statvfs(path.as_ptr(), &mut stats) } == 0).then_some(stats)
}

fn unescape(value: &str) -> String {
    // /proc/self/mounts escapes spaces, tabs, and newlines as \040 and friends.
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match (chars.next(), chars.next(), chars.next()) {
            (Some('0'), Some('4'), Some(digit @ '0'..='7')) => {
                result.push(match digit {
                    '0' => ' ',
                    '1' => '\t',
                    other => other,
                });
            }
            _ => result.push('\\'),
        }
    }
    result
}

fn filter_mounts(rows: &mut Vec<MountRow>, options: &DfOptions) {
    // Port of GNU df's filter_mount_list: keep one entry per device, with the
    // replacement rules that make bind mounts and overmounts pick the same
    // survivor coreutils does.
    let mut kept: Vec<MountRow> = Vec::new();
    'entry: for row in rows.drain(..) {
        if !options.include_types.is_empty() && !options.include_types.contains(&row.fs_type) {
            continue;
        }
        if options.exclude_types.contains(&row.fs_type) {
            continue;
        }
        if options.local && is_remote_fs(&row.fs_type, &row.device) {
            continue;
        }
        if !options.all {
            if row.stats.f_blocks == 0 {
                continue;
            }
            if let Some(dev) = row.dev
                && let Some(seen) = kept.iter_mut().find(|seen| seen.dev == Some(dev))
            {
                let target_nearer_root =
                    seen.mountpoint.as_os_str().len() > row.mountpoint.as_os_str().len();
                let source_below_root = seen.root.len() < row.root.len();
                let real_device_wins = row.device.contains('/') && !seen.device.contains('/');
                let overmounted = seen.device != row.device && seen.mountpoint == row.mountpoint;
                if real_device_wins || (target_nearer_root && !source_below_root) || overmounted {
                    *seen = row;
                }
                continue 'entry;
            }
        }
        kept.push(row);
    }
    *rows = kept;
}

fn percent(used: u64, total: u64) -> String {
    if total == 0 {
        return "-".into();
    }
    let percent = (used as f64 / total as f64 * 100.0).ceil() as u64;
    format!("{percent}%")
}

/// Formats a block count in the requested unit: fixed block size, or human units.
fn format_capacity(blocks: u64, block_size: u64, options: &DfOptions) -> String {
    let bytes = blocks.saturating_mul(block_size.max(1));
    if let Some(base) = options.human {
        return human(bytes, base);
    }
    let unit = options
        .block_size
        .as_deref()
        .and_then(parse_block_size)
        .unwrap_or(1024);
    // GNU df rounds -B sizes up.
    bytes.div_ceil(unit).to_string()
}

fn parse_block_size(spec: &str) -> Option<u64> {
    let split = spec
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(spec.len());
    let (number, scale) = spec.split_at(split);
    let number: u64 = if number.is_empty() {
        1
    } else {
        number.parse().ok()?
    };
    let multiplier = match scale.to_ascii_uppercase().as_str() {
        "" => 1,
        "K" => 1024,
        "M" => 1024 * 1024,
        "G" => 1024 * 1024 * 1024,
        "T" => 1024_u64.pow(4),
        "P" => 1024_u64.pow(5),
        _ => return None,
    };
    Some(number * multiplier)
}

fn human(bytes: u64, base: HumanBase) -> String {
    // Faithful port of gnulib human_readable with human_autoscale and
    // human_ceiling: GNU df rounds sizes UP at every scale step.
    let kind = base;
    let base: u64 = match kind {
        HumanBase::Kibi => 1024,
        HumanBase::Kilo => 1000,
    };
    let suffixes: Vec<char> = match kind {
        HumanBase::Kibi => ['K', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y'].to_vec(),
        // GNU prints a lowercase 'k' for the first SI step.
        HumanBase::Kilo => ['k', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y'].to_vec(),
    };
    let mut amt = bytes;
    let mut tenths = 0u64;
    let mut rounding = 0u64;
    let mut exponent = 0usize;
    while amt >= base && exponent < suffixes.len() {
        let r10 = (amt % base) * 10 + tenths;
        let r2 = (r10 % base) * 2 + (rounding >> 1);
        amt /= base;
        tenths = r10 / base;
        rounding = if r2 < base {
            u64::from(r2 + rounding != 0)
        } else {
            2 + u64::from(base < r2 + rounding)
        };
        exponent += 1;
    }
    if amt < 10 {
        if rounding > 0 {
            tenths += 1;
            rounding = 0;
            if tenths == 10 {
                amt += 1;
                tenths = 0;
            }
        }
        if amt < 10 && exponent > 0 {
            return format!("{amt}.{tenths}{}", suffixes[exponent - 1]);
        }
    }
    if tenths + rounding > 0 {
        amt += 1;
        if amt == base && exponent < suffixes.len() {
            // The carry prints as 1.0 of the next unit.
            return format!("1.0{}", suffixes[exponent]);
        }
    }
    if exponent == 0 {
        format!("{amt}")
    } else {
        format!("{amt}{}", suffixes[exponent - 1])
    }
}
