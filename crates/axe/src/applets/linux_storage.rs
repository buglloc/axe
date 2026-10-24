use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
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
        Ok(()) => 0,
        Err(error) => {
            if !error.is_empty() {
                eprintln!("blockdev: {error}");
            }
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
    SetValue(&'static str, u32, usize),
    SetIntPointer(&'static str, u32, u32),
    Operation(&'static str, u32, usize),
}

fn run_blockdev(args: Vec<OsString>) -> Result<(), String> {
    let values = utf8_args(args)?;
    let mut actions = Vec::new();
    let mut devices = Vec::new();
    let mut report = false;
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-h" | "--help" => {
                print_blockdev_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("blockdev from axe {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--report" => report = true,
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
        // Like util-linux, a bare --report lists every block device.
        if report {
            println!("RO    RA   SSZ   BSZ        StartSec            Size   Device");
            let devices = system_block_devices();
            if devices.is_empty() {
                // util-linux fails when no block device can be reported.
                return Err(String::new());
            }
            for device in devices {
                report_block_device(&device.to_string_lossy())?;
            }
            return Ok(());
        }
        return Err("no device specified".into());
    }
    if report {
        if !actions.is_empty() {
            return Err("--report cannot be combined with other commands".into());
        }
        println!("RO    RA   SSZ   BSZ        StartSec            Size   Device");
        for device in devices {
            report_block_device(&device)?;
        }
        return Ok(());
    }

    for device in devices {
        let file = OpenOptions::new()
            .read(true)
            .open(&device)
            .map_err(|error| format!("{device}: {error}"))?;
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
                BlockAction::SetValue(description, request, value) => {
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
                BlockAction::Operation(description, request, value) => {
                    ioctl_value(&file, request, value)
                        .map_err(|error| format!("{device}: {error}"))?;
                    if verbose {
                        println!("{description} succeeded.");
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_block_action(
    name: &str,
    args: &mut impl Iterator<Item = String>,
) -> Result<BlockAction, String> {
    let get = |description, request, kind| BlockAction::Get(description, request, kind);
    Ok(match name {
        "flushbufs" => BlockAction::Operation("flush buffers", BLKFLSBUF, 0),
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
        "rereadpt" => BlockAction::Operation("reread partition table", BLKRRPART, 0),
        "setbsz" => BlockAction::SetIntPointer(
            "set blocksize",
            BLKBSZSET,
            parse_block_value(args.next(), name)?,
        ),
        "setfra" => BlockAction::SetValue(
            "set filesystem readahead",
            BLKFRASET,
            parse_block_value::<usize>(args.next(), name)?,
        ),
        "setra" => BlockAction::SetValue(
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
    let file = File::open(device).map_err(|error| format!("{device}: {error}"))?;
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
        Ok(found) => {
            if found {
                0
            } else {
                2
            }
        }
        Err(error) => {
            eprintln!("blkid: {error}");
            2
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

fn run_blkid(args: Vec<OsString>) -> Result<bool, String> {
    let values = utf8_args(args)?;
    let mut output = Output::Full;
    let mut tags = Vec::new();
    let mut token_filters: Vec<(String, String)> = Vec::new();
    let mut type_filter: Option<String> = None;
    let mut low_level_probe = false;
    let mut info = false;
    let mut devices = Vec::new();
    let mut iter = values.into_iter().skip(1);
    while let Some(value) = iter.next() {
        match value.as_str() {
            "-s" | "--match-tag" => {
                tags.push(iter.next().ok_or("-s requires a tag")?.to_ascii_uppercase())
            }
            "-o" | "--output" => {
                output = match iter.next().ok_or("-o requires a format")?.as_str() {
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
            "-l" | "--list" => {}
            "-t" | "--match-token" => {
                let token = iter.next().ok_or("-t requires a token")?;
                match token.split_once('=') {
                    Some((tag, value)) => {
                        token_filters.push((tag.to_ascii_uppercase(), value.to_string()))
                    }
                    None => type_filter = Some(token),
                }
            }
            "-L" | "--label" => {
                let label = iter.next().ok_or("-L requires a label")?;
                if !devices.is_empty() {
                    return Err("The -L option can't be used with a device".into());
                }
                token_filters.push(("LABEL".into(), label));
            }
            "-U" | "--uuid" => {
                let value = iter.next().ok_or("-U requires a uuid")?;
                if !devices.is_empty() {
                    return Err("The -U option can't be used with a device".into());
                }
                token_filters.push(("UUID".into(), value));
            }
            "-h" | "--help" => {
                println!(
                    "Usage: blkid [-p|-i] [-s TAG] [-o full|value|export|udev|device] [DEVICE ...]"
                );
                return Ok(true);
            }
            value if value.starts_with('-') => return Err(format!("unknown option '{value}'")),
            _ => devices.push(PathBuf::from(value)),
        }
    }
    if (low_level_probe || info) && devices.is_empty() {
        return Err("-p and -i require an explicit device".into());
    }
    if devices.is_empty() {
        devices = system_block_devices();
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
                eprintln!("blkid: {}: {error}", device.display());
                continue;
            }
        };
        if let Some(want) = &type_filter
            && want != signature.fs_type
        {
            continue;
        }
        if token_filters
            .iter()
            .any(|(tag, want)| signature.tag_value(tag).is_none_or(|value| value != want))
        {
            continue;
        }
        found = true;
        print_signature(&device, &signature, output, &tags);
    }
    Ok(found)
}

#[derive(Default, Debug)]
pub(super) struct Signature {
    pub(super) fs_type: &'static str,
    uuid: Option<String>,
    pub(super) label: Option<String>,
    block_size: Option<String>,
    version: Option<String>,
    pt_type: Option<&'static str>,
    pt_uuid: Option<String>,
}

impl Signature {
    fn tag_value(&self, tag: &str) -> Option<&str> {
        match tag {
            "LABEL" => self.label.as_deref(),
            "UUID" => self.uuid.as_deref(),
            "TYPE" => Some(self.fs_type),
            "PTTYPE" => self.pt_type,
            "PTUUID" => self.pt_uuid.as_deref(),
            _ => None,
        }
    }
}

pub(super) fn probe(path: &Path) -> std::io::Result<Option<Signature>> {
    let mut file = File::open(path)?;
    let mut bytes = vec![0u8; 131_072];
    let read = file.read(&mut bytes)?;
    bytes.truncate(read);

    let signature = if bytes.get(0..6) == Some(b"LUKS\xba\xbe") {
        Signature {
            fs_type: "crypto_LUKS",
            // Both LUKS1 and LUKS2 keep an ASCII UUID at offset 168.
            uuid: bytes.get(168..208).and_then(text_label),
            ..Signature::default()
        }
    } else if bytes.get(3..11) == Some(b"EXFAT   ") {
        Signature {
            fs_type: "exfat",
            uuid: bytes.get(100..104).map(|value| {
                format!(
                    "{:02X}{:02X}-{:02X}{:02X}",
                    value[3], value[2], value[1], value[0]
                )
            }),
            block_size: bytes.get(110).and_then(|shift| {
                (9..=12)
                    .contains(shift)
                    .then(|| (1u64 << shift).to_string())
                    .or_else(|| Some("512".into()))
            }),
            ..Signature::default()
        }
    } else if bytes.get(3..11) == Some(b"NTFS    ") {
        let serial = bytes.get(72..80).map(|value| {
            value
                .iter()
                .rev()
                .map(|byte| format!("{byte:02X}"))
                .collect::<String>()
        });
        Signature {
            fs_type: "ntfs",
            uuid: serial,
            ..Signature::default()
        }
    } else if bytes.get(54..62) == Some(b"FAT12   ")
        || bytes.get(54..62) == Some(b"FAT16   ")
        || bytes.get(82..90) == Some(b"FAT32   ")
    {
        let fat32 = bytes.get(82..90) == Some(b"FAT32   ");
        let (serial, label) = if fat32 { (67, 71..82) } else { (39, 43..54) };
        let version = if fat32 {
            "FAT32"
        } else if bytes.get(54..62) == Some(b"FAT12   ") {
            "FAT12"
        } else {
            "FAT16"
        };
        Signature {
            fs_type: "vfat",
            uuid: bytes.get(serial..serial + 4).map(|value| {
                format!(
                    "{:02X}{:02X}-{:02X}{:02X}",
                    value[3], value[2], value[1], value[0]
                )
            }),
            label: bytes.get(label).and_then(text_label),
            version: Some(version.into()),
            ..Signature::default()
        }
    } else if bytes.get(1080..1082) == Some(&[0x53, 0xef]) {
        let feature_compat = le_u32(&bytes, 1116).unwrap_or_default();
        let feature_incompat = le_u32(&bytes, 1120).unwrap_or_default();
        let fs_type = if feature_incompat & 0x40 != 0 {
            "ext4"
        } else if feature_compat & 0x4 != 0 {
            "ext3"
        } else {
            "ext2"
        };
        let block_size = le_u32(&bytes, 1048)
            .and_then(|log| 1024u64.checked_shl(log))
            .map(|size| size.to_string());
        Signature {
            fs_type,
            uuid: bytes.get(1128..1144).and_then(uuid),
            label: bytes.get(1144..1160).and_then(text_label),
            block_size,
            ..Signature::default()
        }
    } else if bytes.get(0..4) == Some(b"XFSB") {
        Signature {
            fs_type: "xfs",
            uuid: bytes.get(32..48).and_then(uuid),
            label: bytes.get(108..120).and_then(text_label),
            ..Signature::default()
        }
    } else if bytes.get(65_600..65_608) == Some(b"_BHRfS_M") {
        Signature {
            fs_type: "btrfs",
            uuid: bytes.get(65_568..65_584).and_then(uuid),
            label: bytes.get(65_835..66_091).and_then(text_label),
            ..Signature::default()
        }
    } else if bytes.get(1024..1028) == Some(&[0x10, 0x20, 0xf5, 0xf2]) {
        Signature {
            fs_type: "f2fs",
            uuid: bytes.get(1132..1148).and_then(uuid),
            ..Signature::default()
        }
    } else if bytes.get(0..4) == Some(b"hsqs") {
        Signature {
            fs_type: "squashfs",
            ..Signature::default()
        }
    } else if bytes.get(32_769..32_774) == Some(b"CD001") {
        Signature {
            fs_type: "iso9660",
            label: bytes.get(32_808..32_840).and_then(text_label),
            ..Signature::default()
        }
    } else if bytes.get(0..8) == Some(b"LABELONE") && bytes.get(24..32) == Some(b"LVM2 001") {
        Signature {
            fs_type: "LVM2_member",
            ..Signature::default()
        }
    } else if let Some(swap) = detect_swap(&mut file)? {
        swap
    } else if bytes.get(512..520) == Some(b"EFI PART") {
        Signature {
            fs_type: "",
            pt_type: Some("gpt"),
            pt_uuid: bytes.get(568..584).and_then(guid_mixed),
            ..Signature::default()
        }
    } else if bytes.get(510..512) == Some(&[0x55, 0xaa]) {
        Signature {
            fs_type: "",
            pt_type: Some("dos"),
            pt_uuid: bytes.get(440..444).map(|value| {
                format!(
                    "{:02x}{:02x}{:02x}{:02x}",
                    value[3], value[2], value[1], value[0]
                )
            }),
            ..Signature::default()
        }
    } else {
        return Ok(None);
    };
    Ok(Some(signature))
}

fn detect_swap(file: &mut File) -> std::io::Result<Option<Signature>> {
    let length = file.metadata()?.len();
    for page in [4096u64, 8192, 16_384, 32_768, 65_536] {
        if length < page {
            continue;
        }
        file.seek(SeekFrom::Start(page - 10))?;
        let mut magic = [0u8; 10];
        if file.read_exact(&mut magic).is_err()
            || (&magic != b"SWAPSPACE2" && &magic != b"SWAP-SPACE")
        {
            continue;
        }
        let mut header = [0u8; 32];
        file.seek(SeekFrom::Start(0x40c))?;
        if file.read_exact(&mut header).is_err() {
            return Ok(Some(Signature {
                fs_type: "swap",
                ..Signature::default()
            }));
        }
        return Ok(Some(Signature {
            fs_type: "swap",
            uuid: uuid(&header[..16]),
            label: text_label(&header[16..]),
            ..Signature::default()
        }));
    }
    Ok(None)
}

fn print_signature(path: &Path, signature: &Signature, output: Output, tags: &[String]) {
    let fields = [
        ("LABEL", signature.label.as_deref()),
        ("UUID", signature.uuid.as_deref()),
        ("BLOCK_SIZE", signature.block_size.as_deref()),
        ("VERSION", signature.version.as_deref()),
        (
            "TYPE",
            (!signature.fs_type.is_empty()).then_some(signature.fs_type),
        ),
        ("PTUUID", signature.pt_uuid.as_deref()),
        ("PTTYPE", signature.pt_type),
    ]
    .into_iter()
    .filter(|(name, value)| {
        value.is_some() && (tags.is_empty() || tags.iter().any(|tag| tag == name))
    })
    .collect::<Vec<_>>();
    match output {
        Output::Device => println!("{}", path.display()),
        Output::Value => {
            for (_, value) in fields {
                println!("{}", value.expect("filtered"));
            }
        }
        Output::Export => {
            println!("DEVNAME={}", path.display());
            for (name, value) in fields {
                println!("{name}={}", value.expect("filtered"));
            }
        }
        Output::Full => {
            print!("{}:", path.display());
            for (name, value) in fields {
                print!(" {name}=\"{}\"", escape(value.expect("filtered")));
            }
            println!();
        }
        Output::Udev => {
            println!("ID_FS_TYPE={}", signature.fs_type);
            if let Some(label) = &signature.label {
                println!("ID_FS_LABEL={label}");
            }
            if let Some(uuid) = &signature.uuid {
                println!("ID_FS_UUID={uuid}");
            }
            if let Some(version) = &signature.version {
                println!("ID_FS_VERSION={version}");
            }
        }
    }
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

fn system_block_devices() -> Vec<PathBuf> {
    let mut devices = std::fs::read_dir("/sys/class/block")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| Path::new("/dev").join(entry.file_name()))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    devices.sort_unstable();
    devices
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

fn le_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes(value.try_into().ok()?))
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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
            Ok(BlockAction::SetValue("set readahead", BLKRASET, 128))
        ));
        assert!(parse_block_action("setbsz", &mut std::iter::empty()).is_err());
    }
}
