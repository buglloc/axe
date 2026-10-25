use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use linux_raw_sys::ioctl::*;

pub type AppletFn = fn(Vec<OsString>) -> i32;

pub fn commands() -> HashMap<String, AppletFn> {
    let mut commands = HashMap::new();
    commands.insert("blkid".into(), blkid as AppletFn);
    commands.insert("blockdev".into(), blockdev as AppletFn);
    commands.insert("mount".into(), super::mount::mount as AppletFn);
    commands.insert("umount".into(), super::mount::umount as AppletFn);
    commands
}

pub fn blockdev(args: Vec<OsString>) -> i32 {
    match run_blockdev(args) {
        Ok(success) => i32::from(!success),
        Err(error) => {
            eprintln!("blockdev: {error}");
            1
        }
    }
}

#[derive(Clone, Copy)]
enum IoctlValue {
    Short,
    Int,
    Long,
    U64,
    U64Sectors,
}

#[derive(Clone, Copy)]
enum BlockAction {
    Verbose(bool),
    Get(&'static str, u32, IoctlValue),
    SetImmediate(&'static str, u32, usize),
    SetIntPointer(&'static str, u32, u32),
    NoArgument(&'static str, u32),
}

fn run_blockdev(args: Vec<OsString>) -> Result<bool, String> {
    let values = utf8_args(args)?;
    if values.get(1).is_some_and(|value| value == "--report") {
        let quiet = values.len() == 2;
        let devices = if quiet {
            system_block_devices().map_err(|error| format!("/proc/partitions: {error}"))?
        } else {
            values.into_iter().skip(2).map(PathBuf::from).collect()
        };
        println!("RO    RA   SSZ   BSZ        StartSec            Size   Device");
        return Ok(report_devices(&devices, quiet));
    }

    let mut actions = Vec::new();
    let mut devices = Vec::new();
    let mut late_report = false;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-h" | "--help" => {
                print_blockdev_help();
                return Ok(true);
            }
            "-V" | "--version" => {
                println!("blockdev from axe {}", env!("CARGO_PKG_VERSION"));
                return Ok(true);
            }
            "--report" => late_report = true,
            "--" => {
                devices.extend(iter);
                break;
            }
            "-v" | "--verbose" => actions.push(BlockAction::Verbose(true)),
            "-q" | "--quiet" => actions.push(BlockAction::Verbose(false)),
            value if value.starts_with("--") => {
                let name = &value[2..];
                actions.push(parse_block_action(name, &mut iter)?);
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => devices.push(value),
        }
    }
    if devices.is_empty() {
        return Err("no device specified".into());
    }
    if late_report {
        return Err("Unknown command: --report".into());
    }

    for device in devices {
        let file =
            open_block_device(Path::new(&device)).map_err(|error| format!("{device}: {error}"))?;
        let mut verbose = false;
        for action in &actions {
            match *action {
                BlockAction::Verbose(value) => verbose = value,
                BlockAction::Get(description, request, kind) => {
                    let value = get_ioctl_value(&file, request, kind)
                        .map_err(|error| format!("{device}: {error}"))?;
                    if verbose {
                        println!("{description}: {value}");
                    } else {
                        println!("{value}");
                    }
                }
                BlockAction::SetImmediate(description, request, value) => {
                    ioctl_value(&file, request, value)
                        .map_err(|error| format!("{device}: {error}"))?;
                    if verbose {
                        println!("{description} succeeded.");
                    }
                }
                BlockAction::SetIntPointer(description, request, value) => {
                    let mut value = value;
                    ioctl_value(&file, request, (&mut value as *mut u32) as usize)
                        .map_err(|error| format!("{device}: {error}"))?;
                    if verbose {
                        println!("{description} succeeded.");
                    }
                }
                BlockAction::NoArgument(description, request) => {
                    ioctl_value(&file, request, 0).map_err(|error| format!("{device}: {error}"))?;
                    if verbose {
                        println!("{description} succeeded.");
                    }
                }
            }
        }
    }
    Ok(true)
}

fn open_block_device(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

fn report_devices(devices: &[PathBuf], quiet: bool) -> bool {
    let mut failed = false;
    for device in devices {
        let result = report_block_device(&device.to_string_lossy());
        failed |= result.is_err();
        if !quiet && let Err(error) = result {
            eprintln!("blockdev: {error}");
        }
    }
    !failed
}

fn parse_block_action(
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<BlockAction, String> {
    let get = |description, request, kind| BlockAction::Get(description, request, kind);
    Ok(match name {
        "flushbufs" => BlockAction::NoArgument("flush buffers", BLKFLSBUF),
        "getalignoff" => get(
            "get alignment offset in bytes",
            BLKALIGNOFF,
            IoctlValue::Int,
        ),
        "getbsz" => get("get blocksize", BLKBSZGET, IoctlValue::Int),
        "getdiscardzeroes" => get(
            "get discard zeroes support status",
            BLKDISCARDZEROES,
            IoctlValue::Int,
        ),
        "getfra" => get("get filesystem readahead", BLKFRAGET, IoctlValue::Long),
        "getiomin" => get("get minimum I/O size", BLKIOMIN, IoctlValue::Int),
        "getioopt" => get("get optimal I/O size", BLKIOOPT, IoctlValue::Int),
        "getmaxsect" => get("get max sectors per request", BLKSECTGET, IoctlValue::Short),
        "getpbsz" => get(
            "get physical block (sector) size",
            BLKPBSZGET,
            IoctlValue::Int,
        ),
        "getra" => get("get readahead", BLKRAGET, IoctlValue::Long),
        "getro" => get("get read-only", BLKROGET, IoctlValue::Int),
        "getsize64" => get("get size in bytes", BLKGETSIZE64, IoctlValue::U64),
        "getsize" => get("get 32-bit sector count", BLKGETSIZE, IoctlValue::Long),
        "getss" => get(
            "get logical block (sector) size",
            BLKSSZGET,
            IoctlValue::Int,
        ),
        "getsz" => get(
            "get size in 512-byte sectors",
            BLKGETSIZE64,
            IoctlValue::U64Sectors,
        ),
        "rereadpt" => BlockAction::NoArgument("reread partition table", BLKRRPART),
        "setbsz" => BlockAction::SetIntPointer(
            "set blocksize",
            BLKBSZSET,
            parse_block_value(args.next(), name)?,
        ),
        "setfra" => BlockAction::SetImmediate(
            "set filesystem readahead",
            BLKFRASET,
            parse_block_value::<usize>(args.next(), name)?,
        ),
        "setra" => BlockAction::SetImmediate(
            "set readahead",
            BLKRASET,
            parse_block_value::<usize>(args.next(), name)?,
        ),
        "setro" => BlockAction::SetIntPointer("set read-only", BLKROSET, 1),
        "setrw" => BlockAction::SetIntPointer("set read-write", BLKROSET, 0),
        _ => return Err(format!("unknown command '--{name}'")),
    })
}

fn parse_block_value<T: std::str::FromStr>(value: Option<String>, name: &str) -> Result<T, String> {
    let value = value.ok_or_else(|| format!("--{name} requires an argument"))?;
    value
        .parse()
        .map_err(|_| format!("invalid value for --{name}: '{value}'"))
}

fn ioctl_value(file: &File, request: u32, value: usize) -> std::io::Result<()> {
    // SAFETY: the request determines whether value is an integer or a valid pointer
    // supplied by the caller; file remains open for the duration of ioctl.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request as _, value) };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn get_ioctl_value(file: &File, request: u32, kind: IoctlValue) -> std::io::Result<u64> {
    macro_rules! get {
        ($type:ty) => {{
            let mut value: $type = 0;
            ioctl_value(file, request, (&mut value as *mut $type) as usize)?;
            value as u64
        }};
    }
    Ok(match kind {
        IoctlValue::Short => get!(u16),
        IoctlValue::Int => get!(u32),
        IoctlValue::Long => get!(libc::c_ulong),
        IoctlValue::U64 => get!(u64),
        IoctlValue::U64Sectors => get!(u64) / 512,
    })
}

fn report_block_device(device: &str) -> Result<(), String> {
    let file =
        open_block_device(Path::new(device)).map_err(|error| format!("{device}: {error}"))?;
    let read_only = get_ioctl_value(&file, BLKROGET, IoctlValue::Int)
        .map_err(|error| format!("{device}: {error}"))?;
    let readahead = get_ioctl_value(&file, BLKRAGET, IoctlValue::Long)
        .map_err(|error| format!("{device}: {error}"))?;
    let sector_size = get_ioctl_value(&file, BLKSSZGET, IoctlValue::Int)
        .map_err(|error| format!("{device}: {error}"))?;
    let block_size = get_ioctl_value(&file, BLKBSZGET, IoctlValue::Int)
        .map_err(|error| format!("{device}: {error}"))?;
    let size = get_ioctl_value(&file, BLKGETSIZE64, IoctlValue::U64)
        .map_err(|error| format!("{device}: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    let sysfs = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    ));
    let start = if sysfs.join("partition").exists() {
        std::fs::read_to_string(sysfs.join("start"))
            .map_err(|error| error.to_string())?
            .trim()
            .parse::<u64>()
            .map_err(|_| "cannot parse partition start offset".to_string())?
    } else {
        0
    };
    println!(
        "{} {:5} {:5} {:5} {:15} {:15}   {device}",
        if read_only == 1 { "ro" } else { "rw" },
        readahead,
        sector_size,
        block_size,
        start,
        size
    );
    Ok(())
}

fn print_blockdev_help() {
    println!(
        "Usage: blockdev [OPTIONS] COMMAND... DEVICE...\n\
         Get or set block device attributes.\n\n\
         Commands: --getro --setro --setrw --getra --setra N --getss\n\
                   --getpbsz --getbsz --setbsz N --getsize64 --getsz\n\
                   --flushbufs --rereadpt --report"
    );
}

pub fn blkid(args: Vec<OsString>) -> i32 {
    match run_blkid(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("blkid: {error}");
            4
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Output {
    Full,
    Value,
    Export,
    Device,
    Udev,
}

fn run_blkid(args: Vec<OsString>) -> Result<i32, String> {
    let mut output = Output::Full;
    let mut tags = Vec::new();
    let mut token_filter: Option<(String, String)> = None;
    let mut lookup = false;
    let mut evaluate = false;
    let mut low_level_probe = false;
    let mut info = false;
    let mut devices = Vec::new();
    let mut iter = args.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.to_str().unwrap_or_default() {
            "-s" | "--match-tag" => tags.push(blkid_text(iter.next(), "-s requires a tag")?),
            "-o" | "--output" => {
                output = match blkid_text(iter.next(), "-o requires a format")?.as_str() {
                    "full" => Output::Full,
                    "value" => Output::Value,
                    "export" => Output::Export,
                    "udev" => Output::Udev,
                    "device" => Output::Device,
                    format => return Err(format!("unsupported output format '{format}'")),
                }
            }
            "-p" | "--probe" => low_level_probe = true,
            "-i" | "--info" => info = true,
            "-l" | "--list-one" => lookup = true,
            "-t" | "--match-token" => {
                if token_filter.is_some() {
                    return Err("Can only search for one NAME=value pair".into());
                }
                let token = blkid_text(iter.next(), "-t requires a token")?;
                let (tag, value) = token
                    .split_once('=')
                    .filter(|(tag, _)| !tag.is_empty())
                    .ok_or("-t needs NAME=value pair")?;
                token_filter = Some((tag.to_owned(), value.to_owned()));
            }
            "-L" | "--label" => {
                token_filter = Some((
                    "LABEL".into(),
                    blkid_text(iter.next(), "-L requires a label")?,
                ));
                evaluate = true;
            }
            "-U" | "--uuid" => {
                token_filter = Some((
                    "UUID".into(),
                    blkid_text(iter.next(), "-U requires a uuid")?,
                ));
                evaluate = true;
            }
            "-h" | "--help" => {
                println!(
                    "Usage: blkid [-p|-i] [-s TAG] [-o full|value|export|udev|device]\n\
                     \x20            [-l -t NAME=value] [DEVICE ...]\n\
                     \x20      blkid -L LABEL | -U UUID"
                );
                return Ok(0);
            }
            "--" => {
                devices.extend(iter.map(PathBuf::from));
                break;
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => devices.push(PathBuf::from(value)),
        }
    }
    if (low_level_probe || info) && devices.is_empty() {
        return Err("-p and -i require an explicit device".into());
    }
    if lookup && token_filter.is_none() {
        return Err("The lookup option requires a search type specified using -t".into());
    }
    if lookup
        && output == Output::Device
        && token_filter
            .as_ref()
            .is_some_and(|(tag, _)| matches!(tag.as_str(), "LABEL" | "UUID"))
    {
        evaluate = true;
    }
    if evaluate && !low_level_probe && !info {
        let (tag, value) = token_filter.as_ref().expect("lookup has a token");
        if let Some(device) = evaluate_tag(tag, value) {
            let mut output = io::stdout().lock();
            output
                .write_all(device.as_os_str().as_bytes())
                .map_err(|error| error.to_string())?;
            output.write_all(b"\n").map_err(|error| error.to_string())?;
            return Ok(0);
        }
        return Ok(2);
    }

    let explicit_devices = !devices.is_empty();
    if !explicit_devices {
        devices = system_block_devices().map_err(|error| format!("/proc/partitions: {error}"))?;
    }

    let mut found = false;
    for device in devices {
        if info {
            match print_topology(&device, output) {
                Ok(()) => found = true,
                Err(error) => eprintln!("blkid: {}: {error}", device.display()),
            }
            continue;
        }
        let signature = match probe(&device) {
            Ok(Some(signature)) => signature,
            Ok(None) => continue,
            Err(error) => {
                if explicit_devices {
                    eprintln!("blkid: {}: {error}", device.display());
                }
                continue;
            }
        };
        if signature.ambivalent {
            if low_level_probe {
                eprintln!(
                    "blkid: {}: ambivalent result (more than one filesystem detected)",
                    device.display()
                );
                return Ok(8);
            }
            continue;
        }
        if let Some((tag, want)) = &token_filter
            && signature
                .tag_value(tag, low_level_probe)
                .is_none_or(|value| value != want)
        {
            continue;
        }
        found = true;
        print_signature(&device, &signature, output, &tags, low_level_probe)
            .map_err(|error| error.to_string())?;
        if lookup {
            break;
        }
    }
    Ok(if found { 0 } else { 2 })
}

fn evaluate_tag(tag: &str, value: &str) -> Option<PathBuf> {
    let directory = match tag {
        "LABEL" => Path::new("/dev/disk/by-label"),
        "UUID" => Path::new("/dev/disk/by-uuid"),
        _ => return None,
    };
    if Path::new(value).file_name() == Some(std::ffi::OsStr::new(value))
        && let Ok(path) = fs::canonicalize(directory.join(value))
    {
        return Some(path);
    }
    system_block_devices().ok()?.into_iter().find(|device| {
        probe(device)
            .ok()
            .flatten()
            .is_some_and(|signature| signature.tag_value(tag, false) == Some(value))
    })
}

#[derive(Debug)]
struct PartitionTable {
    kind: &'static str,
    uuid: Option<String>,
}

#[derive(Default, Debug)]
pub(super) struct Signature {
    pub(super) fs_type: Option<&'static str>,
    uuid: Option<String>,
    pub(super) label: Option<String>,
    block_size: Option<String>,
    fat_boot_label: Option<String>,
    pub(super) ambivalent: bool,
    version: Option<String>,
    partition_table: Option<PartitionTable>,
}

impl Signature {
    fn tag_value(&self, tag: &str, low_level: bool) -> Option<&str> {
        match tag {
            "LABEL" => self.label.as_deref(),
            "LABEL_FATBOOT" if low_level => self.fat_boot_label.as_deref(),
            "UUID" => self.uuid.as_deref(),
            "TYPE" => self.fs_type,
            "BLOCK_SIZE" => self.block_size.as_deref(),
            "VERSION" if low_level => self.version.as_deref(),
            "PTTYPE" => self.partition_table.as_ref().map(|table| table.kind),
            "PTUUID" => self.partition_table.as_ref()?.uuid.as_deref(),
            _ => None,
        }
    }
}

pub(super) fn probe(path: &Path) -> io::Result<Option<Signature>> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let is_block = metadata.file_type().is_block_device();
    let length = if is_block {
        get_ioctl_value(&file, BLKGETSIZE64, IoctlValue::U64)?
    } else {
        metadata.len()
    };
    let mut bytes = Vec::with_capacity(131_072);
    (&mut file).take(131_072).read_to_end(&mut bytes)?;
    if let Some(mut signature) = probe_filesystem(&file, &bytes, length) {
        if signature.fs_type == Some("vfat") {
            populate_fat_label(&file, &bytes, length, &mut signature);
        }
        return Ok(Some(signature));
    }
    let sector_size = if is_block {
        Some(get_ioctl_value(&file, BLKSSZGET, IoctlValue::Int)?)
    } else {
        None
    };
    Ok(
        probe_partition_table(&mut file, &bytes, length, sector_size)?.map(|table| Signature {
            partition_table: Some(table),
            ..Signature::default()
        }),
    )
}

fn probe_filesystem(file: &File, bytes: &[u8], length: u64) -> Option<Signature> {
    // Containers take precedence over signatures in their payload.
    if let Some(signature) = detect_lvm(bytes, length).or_else(|| detect_luks(bytes, length)) {
        return Some(signature);
    }
    let candidates = [
        detect_fat(bytes, length),
        detect_swap(bytes, length),
        detect_xfs(bytes, length),
        detect_ext(bytes, length),
        detect_iso9660(bytes, length),
        detect_ntfs(file, bytes, length),
        detect_squashfs(bytes, length),
        detect_btrfs(bytes, length),
        detect_exfat(bytes, length),
        detect_f2fs(bytes, length),
    ];
    let mut candidates = candidates.into_iter().flatten();
    let first = candidates.next()?;
    if candidates.next().is_some() {
        Some(Signature {
            ambivalent: true,
            ..Signature::default()
        })
    } else {
        Some(first)
    }
}

fn detect_ext(bytes: &[u8], length: u64) -> Option<Signature> {
    let sb = bytes.get(1024..2048)?;
    if sb.get(56..58) != Some(&[0x53, 0xef]) {
        return None;
    }
    let compat = le_u32(sb, 92)?;
    let incompat = le_u32(sb, 96)?;
    let read_only = le_u32(sb, 100)?;
    let log_block = le_u32(sb, 24)?;
    if log_block > 6 || le_u32(sb, 76)? > 1 {
        return None;
    }
    let block_size = 1024u64 << log_block;
    let log_cluster = if read_only & 0x200 != 0 {
        le_u32(sb, 28)?
    } else {
        log_block
    };
    if log_cluster < log_block {
        return None;
    }
    let cluster_size = 1u64.checked_shl(log_cluster.checked_add(10)?)?;
    let blocks = u64::from(le_u32(sb, 4)?)
        | if incompat & 0x80 != 0 {
            u64::from(le_u32(sb, 336)?) << 32
        } else {
            0
        };
    let first_block = u64::from(le_u32(sb, 20)?);
    let blocks_per_group = u64::from(le_u32(sb, 32)?);
    if blocks <= first_block
        || first_block != u64::from(cluster_size == 1024)
        || blocks.checked_mul(block_size)? > length
        || blocks_per_group == 0
        || u64::from(le_u32(sb, 12)?) > blocks
    {
        return None;
    }
    if incompat & 8 == 0 {
        let inodes = u64::from(le_u32(sb, 0)?);
        let inodes_per_group = u64::from(le_u32(sb, 40)?);
        let inode_size = if le_u32(sb, 76)? == 0 {
            128
        } else {
            u64::from(le_u16(sb, 88)?)
        };
        if inodes == 0
            || inodes_per_group == 0
            || inodes_per_group > block_size * 8
            || inodes
                > (blocks - first_block)
                    .div_ceil(blocks_per_group)
                    .checked_mul(inodes_per_group)?
            || inode_size < 128
            || inode_size > block_size
            || !inode_size.is_power_of_two()
        {
            return None;
        }
    }
    if read_only & 0x400 != 0 && crc32c_update(!0, &sb[..1020]) != le_u32(sb, 1020)? {
        return None;
    }
    let fs_type = if incompat & 8 != 0 {
        "jbd"
    } else if read_only & !0x7 != 0 || incompat & !0x16 != 0 {
        if le_u32(sb, 352)? & 4 != 0 {
            "ext4dev"
        } else {
            "ext4"
        }
    } else if compat & 4 != 0 {
        "ext3"
    } else {
        "ext2"
    };
    Some(Signature {
        fs_type: Some(fs_type),
        uuid: uuid(&sb[104..120]),
        label: text_label(&sb[120..136]),
        block_size: Some(block_size.to_string()),
        version: Some(format!("{}.{}", le_u32(sb, 76)?, le_u16(sb, 62)?)),
        ..Signature::default()
    })
}

fn detect_xfs(bytes: &[u8], length: u64) -> Option<Signature> {
    let sb = bytes.get(..264)?;
    if sb.get(..4) != Some(b"XFSB") {
        return None;
    }
    let block_size = u64::from(be_u32(sb, 4)?);
    let blocks = be_u64(sb, 8)?;
    let sector_size = u64::from(be_u16(sb, 102)?);
    let inode_size = u64::from(be_u16(sb, 104)?);
    let ag_blocks = u64::from(be_u32(sb, 84)?);
    let ag_count = u64::from(be_u32(sb, 88)?);
    let version = be_u16(sb, 100)? & 0xf;
    if !(1..=5).contains(&version)
        || !(512..=65_536).contains(&block_size)
        || !(512..=32_768).contains(&sector_size)
        || !(256..=2048).contains(&inode_size)
        || !block_size.is_power_of_two()
        || !sector_size.is_power_of_two()
        || !inode_size.is_power_of_two()
        || sector_size > block_size
        || inode_size > block_size
        || block_size != 1u64.checked_shl(u32::from(sb[120]))?
        || sector_size != 1u64.checked_shl(u32::from(sb[121]))?
        || inode_size != 1u64.checked_shl(u32::from(sb[122]))?
        || block_size / inode_size != u64::from(be_u16(sb, 106)?)
        || u16::from(sb[120]).checked_sub(u16::from(sb[122]))? != u16::from(sb[123])
        || !(4096..=1024 * 1024 * 1024).contains(&(u64::from(be_u32(sb, 80)?) * block_size))
        || ag_count == 0
        || ag_blocks < 64
        || blocks < (ag_count - 1).checked_mul(ag_blocks)?.checked_add(64)?
        || blocks > ag_count.checked_mul(ag_blocks)?
        || blocks.checked_mul(block_size)? > length
        || sb[127] > 100
    {
        return None;
    }
    let sector = bytes.get(..sector_size as usize)?;
    if version == 5 {
        if be_u16(sb, 100)? & 0x8000 == 0 || be_u32(sb, 200)? & 0x100 == 0 {
            return None;
        }
        let mut crc = crc32c_update(!0, &sector[..224]);
        crc = crc32c_update(crc, &[0; 4]);
        crc = crc32c_update(crc, &sector[228..]);
        if !crc != le_u32(sector, 224)? {
            return None;
        }
    }
    Some(Signature {
        fs_type: Some("xfs"),
        uuid: uuid(&sb[32..48]),
        label: text_label(&sb[108..120]),
        block_size: Some(sector_size.to_string()),
        ..Signature::default()
    })
}

fn detect_btrfs(bytes: &[u8], length: u64) -> Option<Signature> {
    let sb = bytes.get(65_536..69_632)?;
    if sb.get(64..72) != Some(b"_BHRfS_M") || le_u64(sb, 48)? != 65_536 {
        return None;
    }
    let sector_size = le_u32(sb, 144)?;
    let node_size = le_u32(sb, 148)?;
    let total = le_u64(sb, 112)?;
    let device_total = le_u64(sb, 209)?;
    if !(4096..=65_536).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || !(sector_size..=65_536).contains(&node_size)
        || !node_size.is_power_of_two()
        || le_u64(sb, 72)? == 0
        || total == 0
        || le_u64(sb, 120)? > total
        || le_u64(sb, 136)? == 0
        || device_total < 69_632
        || device_total > length
        || le_u64(sb, 217)? > device_total
        || le_u32(sb, 160)? > 2048
        || sb[198..201].iter().any(|level| *level > 8)
    {
        return None;
    }
    let data = &sb[32..];
    let valid_checksum = match le_u16(sb, 196)? {
        0 => !crc32c_update(!0, data) == le_u32(sb, 0)?,
        1 => xxhash_rust::xxh64::xxh64(data, 0) == le_u64(sb, 0)?,
        2 => {
            use sha2::Digest;
            sha2::Sha256::digest(data).as_slice() == &sb[..32]
        }
        3 => {
            blake2b_simd::Params::new()
                .hash_length(32)
                .hash(data)
                .as_bytes()
                == &sb[..32]
        }
        _ => false,
    };
    if !valid_checksum {
        return None;
    }
    Some(Signature {
        fs_type: Some("btrfs"),
        uuid: uuid(&sb[32..48]),
        label: text_label(&sb[299..555]),
        block_size: Some(sector_size.to_string()),
        ..Signature::default()
    })
}

fn detect_squashfs(bytes: &[u8], length: u64) -> Option<Signature> {
    if bytes.get(..4) == Some(b"sqsh")
        || le_u16(bytes, 28).is_some_and(|version| (1..=3).contains(&version))
    {
        return detect_legacy_squashfs(bytes, length);
    }
    let sb = bytes.get(..96)?;
    if sb.get(..4) != Some(b"hsqs") {
        return None;
    }
    let block_size = le_u32(sb, 12)?;
    let used = le_u64(sb, 40)?;
    if le_u16(sb, 28)? != 4
        || le_u16(sb, 30)? != 0
        || le_u32(sb, 4)? == 0
        || !(4096..=1_048_576).contains(&block_size)
        || !block_size.is_power_of_two()
        || block_size != 1u32.checked_shl(u32::from(le_u16(sb, 22)?))?
        || !(1..=6).contains(&le_u16(sb, 20)?)
        || !(96..=length).contains(&used)
        || !(96..used).contains(&le_u64(sb, 64)?)
        || !(96..used).contains(&le_u64(sb, 72)?)
        || [48, 56, 80, 88].into_iter().any(|offset| {
            le_u64(sb, offset).is_none_or(|table| table != u64::MAX && !(96..used).contains(&table))
        })
    {
        return None;
    }
    Some(Signature {
        fs_type: Some("squashfs"),
        block_size: Some(block_size.to_string()),
        version: Some("4.0".into()),
        ..Signature::default()
    })
}

fn detect_legacy_squashfs(bytes: &[u8], length: u64) -> Option<Signature> {
    let big_endian = match bytes.get(..4)? {
        b"sqsh" => true,
        b"hsqs" => false,
        _ => return None,
    };
    let u16_at = |offset| {
        if big_endian {
            be_u16(bytes, offset)
        } else {
            le_u16(bytes, offset)
        }
    };
    let u32_at = |offset| {
        if big_endian {
            be_u32(bytes, offset)
        } else {
            le_u32(bytes, offset)
        }
    };
    let u64_at = |offset| {
        if big_endian {
            be_u64(bytes, offset)
        } else {
            le_u64(bytes, offset)
        }
    };
    let major = u16_at(28)?;
    let minor = u16_at(30)?;
    let (header_size, block_size, used, inode_table, directory_table) = match major {
        1 => (
            51u64,
            u32::from(u16_at(32)?),
            u64::from(u32_at(8)?),
            u64::from(u32_at(20)?),
            u64::from(u32_at(24)?),
        ),
        2 => (
            63,
            u32_at(51)?,
            u64::from(u32_at(8)?),
            u64::from(u32_at(20)?),
            u64::from(u32_at(24)?),
        ),
        3 => (119, u32_at(51)?, u64_at(63)?, u64_at(87)?, u64_at(95)?),
        _ => return None,
    };
    bytes.get(..header_size as usize)?;
    if u32_at(4)? == 0
        || !(4096..=1_048_576).contains(&block_size)
        || !block_size.is_power_of_two()
        || block_size != 1u32.checked_shl(u32::from(u16_at(34)?))?
        || !(header_size..=length).contains(&used)
        || !(header_size..used).contains(&inode_table)
        || !(header_size..used).contains(&directory_table)
    {
        return None;
    }
    Some(Signature {
        fs_type: Some("squashfs3"),
        block_size: Some("1024".into()),
        version: Some(format!("{major}.{minor}")),
        ..Signature::default()
    })
}

fn detect_exfat(bytes: &[u8], length: u64) -> Option<Signature> {
    let sb = bytes.get(..512)?;
    if sb.get(3..11) != Some(b"EXFAT   ")
        || sb.get(510..512) != Some(&[0x55, 0xaa])
        || sb[11..64].iter().any(|byte| *byte != 0)
        || !(9..=12).contains(&sb[108])
        || u16::from(sb[108]) + u16::from(sb[109]) > 25
        || !matches!(sb[110], 1 | 2)
        || sb[105] != 1
    {
        return None;
    }
    let sector_size = 1u64 << sb[108];
    let cluster_sectors = 1u64 << sb[109];
    let sectors = le_u64(sb, 72)?;
    let fat_start = u64::from(le_u32(sb, 80)?);
    let fat_sectors = u64::from(le_u32(sb, 84)?);
    let heap_start = u64::from(le_u32(sb, 88)?);
    let clusters = u64::from(le_u32(sb, 92)?);
    if sectors.checked_mul(sector_size)? > length
        || fat_start < 24
        || fat_sectors == 0
        || heap_start < fat_start.checked_add(fat_sectors * u64::from(sb[110]))?
        || !(1..=0xffff_fff5).contains(&clusters)
        || heap_start.checked_add(clusters.checked_mul(cluster_sectors)?)? > sectors
        || fat_sectors * sector_size / 4 < clusters + 2
        || !(2..=clusters + 1).contains(&u64::from(le_u32(sb, 96)?))
    {
        return None;
    }
    Some(Signature {
        fs_type: Some("exfat"),
        uuid: Some(fat_serial(&sb[100..104])),
        block_size: Some(sector_size.to_string()),
        version: Some(format!("{}.{}", sb[105], sb[104])),
        ..Signature::default()
    })
}

fn detect_f2fs(bytes: &[u8], length: u64) -> Option<Signature> {
    let sb = bytes.get(1024..4096)?;
    if le_u32(sb, 0)? != 0xf2f5_2010 {
        return None;
    }
    let sector_log = le_u32(sb, 8)?;
    let sectors_per_block_log = le_u32(sb, 12)?;
    let segment_log = le_u32(sb, 20)?;
    let blocks = le_u64(sb, 36)?;
    if !(9..=12).contains(&sector_log)
        || sector_log.checked_add(sectors_per_block_log)? != 12
        || le_u32(sb, 16)? != 12
        || segment_log > 31
        || blocks == 0
        || le_u32(sb, 48)? == 0
        || u64::from(le_u32(sb, 48)?).checked_shl(segment_log)? > blocks
        || le_u32(sb, 24)? == 0
        || le_u32(sb, 28)? == 0
    {
        return None;
    }
    let mut device_count = 0;
    let mut device_segments = 0u64;
    let mut primary_segments = 0u64;
    let mut end_of_devices = false;
    for device in sb.get(2201..2745)?.as_chunks::<68>().0 {
        let segments = u64::from(le_u32(device, 64)?);
        if device[0] == 0 {
            if segments != 0 {
                return None;
            }
            end_of_devices = true;
            continue;
        }
        if end_of_devices || segments == 0 {
            return None;
        }
        if device_count == 0 {
            primary_segments = segments;
        }
        device_count += 1;
        device_segments = device_segments.checked_add(segments)?;
    }
    if device_count <= 1 && blocks.checked_mul(4096)? > length {
        return None;
    }
    if device_count != 0 {
        let first_segment = u64::from(le_u32(sb, 72)?);
        let blocks_per_segment = 1u64 << segment_log;
        let primary_blocks = primary_segments
            .checked_mul(blocks_per_segment)?
            .checked_add(first_segment)?;
        let described_blocks = device_segments
            .checked_mul(blocks_per_segment)?
            .checked_add(first_segment)?;
        if primary_blocks.checked_mul(4096)? > length
            || described_blocks > blocks
            || device_segments < u64::from(le_u32(sb, 48)?)
        {
            return None;
        }
    }
    let words = sb[124..1148]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .take_while(|word| *word != 0);
    let label = char::decode_utf16(words)
        .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>();
    Some(Signature {
        fs_type: Some("f2fs"),
        uuid: uuid(&sb[108..124]),
        label: (!label.is_empty()).then_some(label),
        block_size: Some("4096".into()),
        ..Signature::default()
    })
}

fn detect_iso9660(bytes: &[u8], length: u64) -> Option<Signature> {
    for descriptor in bytes.get(32_768..)?.as_chunks::<2048>().0 {
        if descriptor.get(1..6) != Some(b"CD001") || descriptor[6] != 1 {
            return None;
        }
        if descriptor[0] == 255 {
            break;
        }
        if descriptor[0] != 1 {
            continue;
        }
        let blocks = le_u32(descriptor, 80)?;
        let block_size = le_u16(descriptor, 128)?;
        if blocks == 0
            || blocks != be_u32(descriptor, 84)?
            || block_size != be_u16(descriptor, 130)?
            || !block_size.is_power_of_two()
            || !(512..=4096).contains(&block_size)
            || u64::from(blocks).checked_mul(u64::from(block_size))? > length
        {
            return None;
        }
        return Some(Signature {
            fs_type: Some("iso9660"),
            label: text_label(&descriptor[40..72]),
            block_size: Some(block_size.to_string()),
            ..Signature::default()
        });
    }
    None
}

fn detect_lvm(bytes: &[u8], length: u64) -> Option<Signature> {
    for (index, sector) in bytes.as_chunks::<512>().0.iter().take(4).enumerate() {
        if sector.get(..8) != Some(b"LABELONE")
            || sector.get(24..32) != Some(b"LVM2 001")
            || le_u64(sector, 8)? != index as u64
            || crc32_update(0xf597_a6cf, &sector[20..]) != le_u32(sector, 16)?
        {
            continue;
        }
        let offset = le_u32(sector, 20)? as usize;
        if offset < 32 {
            continue;
        }
        let pv = sector.get(offset..offset.checked_add(40)?)?;
        if pv[..32].iter().any(|byte| !byte.is_ascii_alphanumeric())
            || le_u64(pv, 32)? > length
            || le_u64(pv, 32)? == 0
        {
            continue;
        }
        return Some(Signature {
            fs_type: Some("LVM2_member"),
            ..Signature::default()
        });
    }
    None
}

fn read_at<const N: usize>(file: &File, length: u64, offset: u64) -> Option<[u8; N]> {
    if offset.checked_add(N as u64)? > length {
        return None;
    }
    let mut buffer = [0; N];
    file.read_exact_at(&mut buffer, offset).ok()?;
    Some(buffer)
}

fn detect_ntfs(file: &File, bytes: &[u8], length: u64) -> Option<Signature> {
    let sb = bytes.get(..512)?;
    if sb.get(3..11) != Some(b"NTFS    ") || sb.get(510..512) != Some(&[0x55, 0xaa]) {
        return None;
    }
    let sector_size = u64::from(le_u16(sb, 11)?);
    let sectors_per_cluster = match sb[13] {
        value if value.is_power_of_two() => u64::from(value),
        240..=249 => 1u64 << (256 - u16::from(sb[13])),
        _ => return None,
    };
    let cluster_size = sector_size.checked_mul(sectors_per_cluster)?;
    let sectors = le_u64(sb, 40)?;
    let clusters = sectors / sectors_per_cluster;
    let mft = le_u64(sb, 48)?;
    let mirror = le_u64(sb, 56)?;
    let record_size = match sb[64] as i8 {
        value @ 1..=64 if (value as u8).is_power_of_two() => cluster_size * value as u64,
        value @ -31..=-9 => 1u64 << (-value as u32),
        _ => return None,
    };
    if !(256..=4096).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || cluster_size > 2 * 1024 * 1024
        || sb[14..21].iter().any(|byte| *byte != 0)
        || le_u16(sb, 22)? != 0
        || le_u32(sb, 32)? != 0
        || sectors == 0
        || sectors.checked_mul(sector_size)? > length
        || mft >= clusters
        || mirror >= clusters
        || record_size < 48
    {
        return None;
    }
    let mft_offset = mft.checked_mul(cluster_size)?;
    let first = read_at::<48>(file, length, mft_offset)?;
    let volume_offset = mft_offset.checked_add(3 * record_size)?;
    let volume = read_at::<48>(file, length, volume_offset)?;
    for header in [&first, &volume] {
        if header.get(..4) != Some(b"FILE")
            || u64::from(le_u32(header, 24)?) > record_size
            || u64::from(le_u32(header, 28)?) > record_size
            || le_u32(header, 24)? > le_u32(header, 28)?
            || le_u32(header, 24)? < 48
        {
            return None;
        }
    }
    let mut label = None;
    let mut offset = u64::from(le_u16(&volume, 20)?);
    let used = u64::from(le_u32(&volume, 24)?);
    while offset.checked_add(24)? <= used {
        let attribute = read_at::<24>(file, length, volume_offset.checked_add(offset)?)?;
        let kind = le_u32(&attribute, 0)?;
        if kind == u32::MAX {
            break;
        }
        let size = u64::from(le_u32(&attribute, 4)?);
        if size < 24 || offset.checked_add(size)? > used {
            return None;
        }
        if kind == 0x60 && attribute[8] == 0 {
            let value_size = u64::from(le_u32(&attribute, 16)?);
            let value_offset = u64::from(le_u16(&attribute, 20)?);
            if value_size > 512
                || value_size % 2 != 0
                || value_offset.checked_add(value_size)? > size
            {
                return None;
            }
            let mut value = [0u8; 512];
            let start = volume_offset
                .checked_add(offset)?
                .checked_add(value_offset)?;
            if start.checked_add(value_size)? > length {
                return None;
            }
            file.read_exact_at(&mut value[..value_size as usize], start)
                .ok()?;
            let words = value[..value_size as usize]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|word| u16::from_le_bytes([word[0], word[1]]));
            let text = char::decode_utf16(words)
                .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect::<String>();
            label = (!text.is_empty()).then_some(text);
            break;
        }
        offset += size;
    }
    Some(Signature {
        fs_type: Some("ntfs"),
        uuid: Some(format!("{:016X}", le_u64(sb, 72)?)),
        label,
        block_size: Some(sector_size.to_string()),
        ..Signature::default()
    })
}

fn detect_luks(bytes: &[u8], length: u64) -> Option<Signature> {
    if bytes.get(..6) != Some(b"LUKS\xba\xbe") {
        return None;
    }
    let version = be_u16(bytes, 6)?;
    let label = match version {
        1 => {
            bytes.get(..592)?;
            None
        }
        2 => {
            bytes.get(..4096)?;
            let header_size = be_u64(bytes, 8)?;
            if !(16_384..=4_194_304).contains(&header_size)
                || !header_size.is_power_of_two()
                || header_size > length
                || be_u64(bytes, 256)? != 0
            {
                return None;
            }
            text_label(&bytes[24..72])
        }
        _ => return None,
    };
    Some(Signature {
        fs_type: Some("crypto_LUKS"),
        uuid: text_label(&bytes[168..208]),
        label,
        version: Some(version.to_string()),
        ..Signature::default()
    })
}

fn detect_fat(bytes: &[u8], length: u64) -> Option<Signature> {
    bytes.get(..512)?;
    let magic = bytes.get(54..62)?;
    if magic == b"JFS     " || magic == b"HPFS    " {
        return None;
    }
    let has_magic = matches!(magic, b"FAT12   " | b"FAT16   " | b"FAT     ")
        || bytes.get(54..59) == Some(b"MSDOS")
        || bytes.get(82..87) == Some(b"MSWIN")
        || bytes.get(82..90) == Some(b"FAT32   ");
    if !has_magic && bytes.get(510..512) != Some(&[0x55, 0xaa]) {
        return None;
    }
    let sector_size = u64::from(le_u16(bytes, 11)?);
    let cluster_size = u64::from(bytes[13]);
    let reserved = u64::from(le_u16(bytes, 14)?);
    let fats = u64::from(bytes[16]);
    if !(512..=4096).contains(&sector_size)
        || !sector_size.is_power_of_two()
        || !cluster_size.is_power_of_two()
        || cluster_size > 128
        || reserved == 0
        || fats == 0
        || !matches!(bytes[21], 0xf0 | 0xf8..=0xff)
    {
        return None;
    }
    let root_entries = u64::from(le_u16(bytes, 17)?);
    let total_sectors = match le_u16(bytes, 19)? {
        0 => u64::from(le_u32(bytes, 32)?),
        value => u64::from(value),
    };
    let fat16_length = le_u16(bytes, 22)?;
    let fat32 = fat16_length == 0;
    let fat_length = if fat32 {
        u64::from(le_u32(bytes, 36)?)
    } else {
        u64::from(fat16_length)
    };
    if fat_length == 0 {
        return None;
    }
    let directory_sectors = (root_entries * 32).div_ceil(sector_size);
    let data_sectors =
        total_sectors.checked_sub(reserved + fat_length * fats + directory_sectors)?;
    let clusters = data_sectors / cluster_size;
    let fat_entry_bits = if fat32 {
        32
    } else if clusters <= 0xff4 {
        12
    } else {
        16
    };
    if clusters == 0
        || total_sectors.checked_mul(sector_size)? > length
        || (clusters + 2).checked_mul(fat_entry_bits)? > fat_length * sector_size * 8
    {
        return None;
    }
    let (serial, label, version, extended_signature) = if fat32 {
        let root_cluster = u64::from(le_u32(bytes, 44)?);
        if clusters > 0x0fff_fff6
            || root_entries != 0
            || le_u16(bytes, 42)? != 0
            || !(2..=clusters + 1).contains(&root_cluster)
        {
            return None;
        }
        (67, 71..82, "FAT32", bytes[66])
    } else {
        if clusters > 0xfff4 || root_entries == 0 {
            return None;
        }
        let version = if clusters <= 0xff4 { "FAT12" } else { "FAT16" };
        (39, 43..54, version, bytes[38])
    };
    Some(Signature {
        fs_type: Some("vfat"),
        uuid: (fat32 || matches!(extended_signature, 0x28 | 0x29))
            .then(|| fat_serial(&bytes[serial..serial + 4])),
        label: (extended_signature == 0x29)
            .then(|| text_label(&bytes[label.clone()]))
            .flatten(),
        fat_boot_label: (extended_signature == 0x29)
            .then(|| text_label(&bytes[label]))
            .flatten(),
        block_size: Some(sector_size.to_string()),
        version: Some(version.into()),
        ..Signature::default()
    })
}

fn populate_fat_label(file: &File, bytes: &[u8], length: u64, signature: &mut Signature) {
    let sector_size = u64::from(le_u16(bytes, 11).unwrap_or_default());
    let reserved = u64::from(le_u16(bytes, 14).unwrap_or_default());
    let fats = u64::from(bytes[16]);
    let fat16 = u64::from(le_u16(bytes, 22).unwrap_or_default());
    let fat_sectors = if fat16 == 0 {
        u64::from(le_u32(bytes, 36).unwrap_or_default())
    } else {
        fat16
    };
    let root_entries = u64::from(le_u16(bytes, 17).unwrap_or_default());
    let data_offset = (reserved + fats * fat_sectors) * sector_size;
    if fat16 != 0 {
        if let Some(label) = fat_directory_label(file, length, data_offset, root_entries) {
            signature.label = Some(label);
        }
        return;
    }
    let cluster_size = u64::from(bytes[13]) * sector_size;
    let mut cluster = u64::from(le_u32(bytes, 44).unwrap_or_default());
    let entries = fat_sectors * sector_size / 4;
    // Bound work on corrupt/cyclic root chains, as libblkid does.
    for _ in 0..100 {
        if !(2..entries).contains(&cluster) {
            break;
        }
        let Some(offset) = (cluster - 2)
            .checked_mul(cluster_size)
            .and_then(|offset| data_offset.checked_add(offset))
        else {
            break;
        };
        if let Some(label) = fat_directory_label(file, length, offset, cluster_size / 32) {
            signature.label = Some(label);
            break;
        }
        let Some(entry) = read_at::<4>(file, length, reserved * sector_size + cluster * 4) else {
            break;
        };
        cluster = u64::from(u32::from_le_bytes(entry) & 0x0fff_ffff);
    }
}

fn fat_directory_label(file: &File, length: u64, offset: u64, entries: u64) -> Option<String> {
    for index in 0..entries {
        let entry = read_at::<32>(file, length, offset.checked_add(index.checked_mul(32)?)?)?;
        if entry[0] == 0 {
            return None;
        }
        if entry[0] == 0xe5
            || entry[11] & 0x3f == 0x0f
            || entry[11] & 0x18 != 0x08
            || le_u16(&entry, 20)? != 0
            || le_u16(&entry, 26)? != 0
        {
            continue;
        }
        let mut label = [0u8; 11];
        label.copy_from_slice(&entry[..11]);
        if label[0] == 5 {
            label[0] = 0xe5;
        }
        return text_label(&label);
    }
    None
}

fn fat_serial(bytes: &[u8]) -> String {
    format!(
        "{:02X}{:02X}-{:02X}{:02X}",
        bytes[3], bytes[2], bytes[1], bytes[0]
    )
}

fn detect_swap(bytes: &[u8], length: u64) -> Option<Signature> {
    if length < 10 * 4096 {
        return None;
    }
    for page in [4096usize, 8192, 16_384, 32_768, 65_536] {
        if length / (page as u64) < 2 {
            continue;
        }
        let magic = bytes.get(page - 10..page)?;
        if magic == b"SWAP-SPACE" {
            return Some(Signature {
                fs_type: Some("swap"),
                version: Some("0".into()),
                ..Signature::default()
            });
        }
        if magic != b"SWAPSPACE2" {
            continue;
        }
        let (last_page, bad_pages) = match le_u32(bytes, 1024)? {
            1 => (le_u32(bytes, 1028)?, le_u32(bytes, 1032)?),
            0x0100_0000 => (be_u32(bytes, 1028)?, be_u32(bytes, 1032)?),
            _ => continue,
        };
        if last_page == 0
            || (u64::from(last_page) + 1) * page as u64 > length
            || u64::from(bad_pages) * 4 > (page - 10 - 1536) as u64
        {
            continue;
        }
        let has_metadata = le_u32(bytes, 1196)? == 0 && le_u32(bytes, 1200)? == 0;
        return Some(Signature {
            fs_type: Some("swap"),
            uuid: has_metadata.then(|| uuid(&bytes[1036..1052])).flatten(),
            label: has_metadata
                .then(|| text_label(&bytes[1052..1068]))
                .flatten(),
            version: Some("1".into()),
            ..Signature::default()
        });
    }
    None
}

fn probe_partition_table(
    file: &mut File,
    bytes: &[u8],
    length: u64,
    sector_size: Option<u64>,
) -> io::Result<Option<PartitionTable>> {
    if bytes.get(510..512) != Some(&[0x55, 0xaa]) {
        return Ok(None);
    }
    let entries = bytes[446..510].as_chunks::<16>().0;
    if entries.iter().any(|entry| entry[4] == 0xee) {
        let candidates = sector_size.map_or([512, 4096], |size| [size, size]);
        for (index, size) in candidates.into_iter().enumerate() {
            if index != 0 && sector_size.is_some() {
                break;
            }
            if !(512..=4096).contains(&size) || !size.is_power_of_two() || length < size * 2 {
                continue;
            }
            let last_lba = length / size - 1;
            for lba in [1, last_lba] {
                if let Some(table) = detect_gpt(file, bytes, length, size, lba)? {
                    return Ok(Some(table));
                }
            }
        }
        return Ok(None);
    }
    if entries.iter().any(|entry| !matches!(entry[0], 0 | 0x80)) {
        return Ok(None);
    }
    let id = le_u32(bytes, 440).unwrap_or_default();
    Ok(Some(PartitionTable {
        kind: "dos",
        uuid: (id != 0).then(|| format!("{id:08x}")),
    }))
}

fn detect_gpt(
    file: &mut File,
    bytes: &[u8],
    length: u64,
    sector_size: u64,
    lba: u64,
) -> io::Result<Option<PartitionTable>> {
    let offset = lba * sector_size;
    let mut sector = [0u8; 4096];
    let header = if let Ok(start) = usize::try_from(offset)
        && let Some(header) = start
            .checked_add(sector_size as usize)
            .and_then(|end| bytes.get(start..end))
    {
        header
    } else {
        let sector = &mut sector[..sector_size as usize];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(sector)?;
        sector
    };
    if header.get(..8) != Some(b"EFI PART") {
        return Ok(None);
    }
    let header_size = le_u32(header, 12).unwrap_or_default() as usize;
    if !(92..=sector_size as usize).contains(&header_size) {
        return Ok(None);
    }
    let mut crc = crc32_update(!0, &header[..16]);
    crc = crc32_update(crc, &[0; 4]);
    crc = crc32_update(crc, &header[20..header_size]);
    if !crc != le_u32(header, 16).unwrap_or_default() || le_u64(header, 24) != Some(lba) {
        return Ok(None);
    }
    let last_lba = length / sector_size - 1;
    let first = le_u64(header, 40).unwrap_or_default();
    let last = le_u64(header, 48).unwrap_or_default();
    let alternate = le_u64(header, 32).unwrap_or_default();
    if first <= 1
        || first > last
        || last >= last_lba
        || (first..=last).contains(&lba)
        || alternate > last_lba
        || alternate == lba
    {
        return Ok(None);
    }
    let count = u64::from(le_u32(header, 80).unwrap_or_default());
    let entry_size = u64::from(le_u32(header, 84).unwrap_or_default());
    let entries_lba = le_u64(header, 72).unwrap_or_default();
    let entries_length = count * entry_size;
    let Some(entries_offset) = entries_lba.checked_mul(sector_size) else {
        return Ok(None);
    };
    let Some(entries_end) = entries_offset.checked_add(entries_length) else {
        return Ok(None);
    };
    if entry_size != 128
        || entries_length == 0
        || entries_length >= u64::from(u32::MAX)
        || entries_lba < 2
        || entries_end > length
        || (entries_offset < offset + sector_size && entries_end > offset)
        || (entries_offset < (last + 1) * sector_size && entries_end > first * sector_size)
    {
        return Ok(None);
    }
    let expected_crc = le_u32(header, 88).unwrap_or_default();
    let mut crc = !0;
    if let (Ok(start), Ok(end)) = (
        usize::try_from(entries_offset),
        usize::try_from(entries_end),
    ) && let Some(entries) = bytes.get(start..end)
    {
        crc = crc32_update(crc, entries);
    } else {
        file.seek(SeekFrom::Start(entries_offset))?;
        let mut remaining = entries_length;
        let mut buffer = [0u8; 4096];
        while remaining != 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..count])?;
            crc = crc32_update(crc, &buffer[..count]);
            remaining -= count as u64;
        }
    }
    if !crc != expected_crc {
        return Ok(None);
    }
    Ok(Some(PartitionTable {
        kind: "gpt",
        uuid: guid_mixed(&header[56..72]),
    }))
}

fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

fn crc32c_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}
fn print_signature(
    path: &Path,
    signature: &Signature,
    output: Output,
    tags: &[String],
    low_level: bool,
) -> io::Result<()> {
    let mut writer = io::stdout().lock();
    if output == Output::Device {
        writer.write_all(path.as_os_str().as_bytes())?;
        return writer.write_all(b"\n");
    }
    let mut fields = [
        "LABEL_FATBOOT",
        "LABEL",
        "UUID",
        "VERSION",
        "BLOCK_SIZE",
        "TYPE",
        "PTUUID",
        "PTTYPE",
    ]
    .into_iter()
    .filter(|name| tags.is_empty() || tags.iter().any(|tag| tag == name))
    .filter_map(|name| {
        signature
            .tag_value(name, low_level)
            .map(|value| (name, value))
    })
    .peekable();
    if fields.peek().is_none() {
        return Ok(());
    }
    match output {
        Output::Device => {}
        Output::Value => {
            for (_, value) in fields {
                writeln!(writer, "{value}")?;
            }
        }
        Output::Export => {
            writer.write_all(b"DEVNAME=")?;
            writer.write_all(path.as_os_str().as_bytes())?;
            writer.write_all(b"\n")?;
            for (name, value) in fields {
                writeln!(writer, "{name}={}", safe_print(value, b" \\\"'$`<>"))?;
            }
        }
        Output::Full => {
            writer.write_all(path.as_os_str().as_bytes())?;
            writer.write_all(b":")?;
            for (name, value) in fields {
                write!(writer, " {name}=\"{}\"", safe_print(value, b"\"\\"))?;
            }
            writeln!(writer)?;
        }
        Output::Udev => {
            for (name, value) in fields {
                match name {
                    "PTUUID" => writeln!(writer, "ID_PART_TABLE_UUID={value}")?,
                    "PTTYPE" => writeln!(writer, "ID_PART_TABLE_TYPE={value}")?,
                    "BLOCK_SIZE" => {}
                    _ => writeln!(writer, "ID_FS_{name}={value}")?,
                }
            }
        }
    }
    writer.flush()
}

fn print_topology(path: &Path, output: Output) -> Result<(), String> {
    let name = path
        .file_name()
        .ok_or("device has no basename")?
        .to_string_lossy();
    let base = Path::new("/sys/class/block").join(name.as_ref());
    let fields = [
        ("MINIMUM_IO_SIZE", base.join("queue/minimum_io_size")),
        (
            "PHYSICAL_SECTOR_SIZE",
            base.join("queue/physical_block_size"),
        ),
        ("LOGICAL_SECTOR_SIZE", base.join("queue/logical_block_size")),
        ("OPTIMAL_IO_SIZE", base.join("queue/optimal_io_size")),
        ("ALIGNMENT_OFFSET", base.join("alignment_offset")),
    ]
    .into_iter()
    .filter_map(|(name, source)| {
        fs::read_to_string(source)
            .ok()
            .map(|value| (name, value.trim().to_owned()))
    })
    .collect::<Vec<_>>();
    if fields.is_empty() {
        return Err("I/O topology is unavailable".into());
    }
    match output {
        Output::Device => println!("{}", path.display()),
        Output::Value => {
            for (_, value) in fields {
                println!("{value}");
            }
        }
        Output::Export => {
            println!("DEVNAME={}", path.display());
            for (name, value) in fields {
                println!("{name}={value}");
            }
        }
        Output::Udev => {
            for (name, value) in fields {
                println!("ID_{name}={value}");
            }
        }
        Output::Full => {
            print!("{}:", path.display());
            for (name, value) in fields {
                print!(" {name}=\"{value}\"");
            }
            println!();
        }
    }
    Ok(())
}

fn system_block_devices() -> io::Result<Vec<PathBuf>> {
    fs::read_to_string("/proc/partitions")
        .map(|partitions| block_devices_from_partitions(&partitions))
}

fn block_devices_from_partitions(partitions: &str) -> Vec<PathBuf> {
    partitions
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next()?.parse::<u32>().ok()?;
            fields.next()?.parse::<u32>().ok()?;
            let blocks = fields.next()?.parse::<u64>().ok()?;
            let name = fields.next()?;
            (blocks != 0).then(|| Path::new("/dev").join(name))
        })
        .collect()
}

fn uuid(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 16 || bytes.iter().all(|byte| *byte == 0) {
        return None;
    }

    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

/// Formats a GPT GUID: the first three groups are little-endian.
fn guid_mixed(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 16 || bytes.iter().all(|byte| *byte == 0) {
        return None;
    }
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[3],
        bytes[2],
        bytes[1],
        bytes[0],
        bytes[5],
        bytes[4],
        bytes[7],
        bytes[6],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

fn text_label(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    let value = String::from_utf8_lossy(&bytes[..end]).trim().to_string();
    (!value.is_empty() && value != "NO NAME").then_some(value)
}

fn le_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn be_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_be_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn le_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(value.try_into().ok()?))
}

fn safe_print(value: &str, escaped: &[u8]) -> String {
    let mut output = String::new();
    for mut byte in value.bytes() {
        if byte >= 128 {
            output.push_str("M-");
            byte -= 128;
        }
        if byte < 32 || byte == 127 {
            output.push('^');
            byte ^= 0x40;
        } else if escaped.contains(&byte) {
            output.push('\\');
        }
        output.push(char::from(byte));
    }
    output
}

fn blkid_text(value: Option<OsString>, missing: &str) -> Result<String, String> {
    value
        .ok_or_else(|| missing.to_owned())?
        .into_string()
        .map_err(|_| "option value is not valid UTF-8".to_owned())
}

fn utf8_args(args: Vec<OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|value| {
            value.into_string().map_err(|value| {
                format!("argument is not valid UTF-8: {}", value.to_string_lossy())
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blockdev_setters_consume_their_value() {
        let mut values = vec!["128".to_string()].into_iter();
        assert!(matches!(
            parse_block_action("setra", &mut values),
            Ok(BlockAction::SetImmediate("set readahead", BLKRASET, 128))
        ));
        assert!(parse_block_action("setbsz", &mut std::iter::empty()).is_err());
    }

    #[test]
    fn report_devices_preserves_kernel_order_and_skips_zero_capacity() {
        let partitions = "major minor  #blocks  name\n\n\
                          259 0 1953525168 nvme1n1\n\
                          8 16 0 sdb\n\
                          7 10 4096 loop10\n\
                          7 2 4096 loop2\n\
                          8 32 4294967296 sdc\n";
        assert_eq!(
            block_devices_from_partitions(partitions),
            ["nvme1n1", "loop10", "loop2", "sdc"].map(|name| Path::new("/dev").join(name))
        );
        assert!(block_devices_from_partitions("major minor #blocks name\n").is_empty());
    }

    #[test]
    fn report_aggregates_failures_and_accepts_an_empty_device_list() {
        assert!(report_devices(&[], true));
        assert!(!report_devices(
            &[PathBuf::from("/dev/null"), PathBuf::from("/dev/zero")],
            true,
        ));
    }
}
