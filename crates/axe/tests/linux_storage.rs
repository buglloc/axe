#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-storage-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    fn image(&self, name: &str, header: &[u8], length: u64) -> PathBuf {
        let path = self.0.join(name);
        let mut file = File::create(&path).expect("create image");
        file.write_all(header).expect("write image header");
        file.set_len(length).expect("set image length");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(applet: &str, args: &[&str], devices: &[&Path]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .args(devices)
        .output()
        .expect("run storage applet")
}

fn success(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "status: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    std::str::from_utf8(&output.stdout).expect("UTF-8 storage output")
}

#[test]
fn mountpoint_status_distinguishes_mounts_directories_and_missing_paths() {
    let scratch = Scratch::new("mountpoint-status");
    for cwd in [Path::new("/"), scratch.0.as_path()] {
        for (path, expected) in [
            (PathBuf::from("/"), 0),
            (scratch.0.clone(), 32),
            (scratch.0.join("missing"), 1),
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_axe"))
                .args(["--applet", "mountpoint", "--"])
                .arg(&path)
                .current_dir(cwd)
                .env("PATH", "/nonexistent")
                .env("AXE_STORE_MODE", "off")
                .output()
                .expect("run mountpoint");
            assert_eq!(
                output.status.code(),
                Some(expected),
                "path={path:?}, cwd={cwd:?}, stderr={:?}",
                output.stderr
            );
        }
    }
}

fn fat16_header() -> Vec<u8> {
    let mut bytes = vec![0u8; 512];
    bytes[..3].copy_from_slice(&[0xeb, 0x3c, 0x90]);
    bytes[11..13].copy_from_slice(&512u16.to_le_bytes());
    bytes[13] = 1;
    bytes[14..16].copy_from_slice(&1u16.to_le_bytes());
    bytes[16] = 2;
    bytes[17..19].copy_from_slice(&512u16.to_le_bytes());
    bytes[19..21].copy_from_slice(&32_768u16.to_le_bytes());
    bytes[21] = 0xf8;
    bytes[22..24].copy_from_slice(&128u16.to_le_bytes());
    bytes[38] = 0x29;
    bytes[39..43].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    bytes[43..54].copy_from_slice(b"FAT VOLUME ");
    bytes[54..62].copy_from_slice(b"FAT     ");
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    bytes
}

#[test]
fn generic_fat_bpb_is_a_filesystem_and_version_is_low_level_only() {
    let scratch = Scratch::new("fat-bpb");
    let image = scratch.image("fat.img", &fat16_header(), 16 * 1024 * 1024);
    let output = run("blkid", &["-o", "export"], &[&image]);
    let text = success(&output);
    assert!(text.contains("TYPE=vfat\n"), "{text}");
    assert!(text.contains("UUID=1234-5678\n"), "{text}");
    assert!(text.contains("BLOCK_SIZE=512\n"), "{text}");
    assert!(!text.contains("PTTYPE="), "{text}");
    assert!(!text.contains("VERSION="), "{text}");

    let output = run("blkid", &["-p", "-s", "VERSION", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "FAT16\n");
}

#[test]
fn fat_magic_without_a_valid_bpb_is_not_a_filesystem() {
    let scratch = Scratch::new("bad-fat");
    let mut truncated = vec![0; 62];
    truncated[54..62].copy_from_slice(b"FAT12   ");
    let image = scratch.image("truncated.img", &truncated, truncated.len() as u64);
    let output = run("blkid", &[], &[&image]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    for (name, offset, replacement) in [
        ("sector-size", 11, vec![1, 2]),
        ("cluster-size", 13, vec![3]),
        ("reserved", 14, vec![0, 0]),
        ("fats", 16, vec![0]),
        ("size-underflow", 19, vec![1, 0]),
    ] {
        let mut bytes = fat16_header();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        let image = scratch.image(name, &bytes, 16 * 1024 * 1024);
        let output = run("blkid", &["-s", "TYPE", "-o", "value"], &[&image]);
        assert!(output.stdout.is_empty(), "{name}: {:?}", output.stdout);
    }
}

#[test]
fn exfat_block_size_uses_the_sector_shift_not_the_fat_count() {
    let scratch = Scratch::new("exfat-sector-size");
    let mut bytes = vec![0; 512];
    bytes[..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
    bytes[3..11].copy_from_slice(b"EXFAT   ");
    bytes[72..80].copy_from_slice(&256u64.to_le_bytes());
    bytes[80..84].copy_from_slice(&24u32.to_le_bytes());
    bytes[84..88].copy_from_slice(&1u32.to_le_bytes());
    bytes[88..92].copy_from_slice(&25u32.to_le_bytes());
    bytes[92..96].copy_from_slice(&231u32.to_le_bytes());
    bytes[96..100].copy_from_slice(&2u32.to_le_bytes());
    bytes[100..104].copy_from_slice(&0x89ab_cdefu32.to_le_bytes());
    bytes[105] = 1;
    bytes[108] = 12;
    bytes[110] = 1;
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    let image = scratch.image("exfat.img", &bytes, 1024 * 1024);
    let output = run("blkid", &["-s", "BLOCK_SIZE", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "4096\n");

    bytes[108] = 31;
    let image = scratch.image("bad-shift.img", &bytes, 1024 * 1024);
    let output = run("blkid", &["-s", "TYPE", "-o", "value"], &[&image]);
    assert!(output.stdout.is_empty());
}

#[test]
fn partition_metadata_does_not_create_empty_filesystem_tags() {
    let scratch = Scratch::new("partition-tags");
    let mut bytes = vec![0; 512];
    bytes[450] = 0x83;
    bytes[454..458].copy_from_slice(&1u32.to_le_bytes());
    bytes[458..462].copy_from_slice(&127u32.to_le_bytes());
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    let image = scratch.image("mbr.img", &bytes, 128 * 512);

    let output = run("blkid", &["-o", "udev"], &[&image]);
    assert_eq!(success(&output), "ID_PART_TABLE_TYPE=dos\n");
    let output = run("blkid", &["-s", "TYPE"], &[&image]);
    assert_eq!(success(&output), "");
    let output = run("blkid", &["-t", "TYPE="], &[&image]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    bytes[440..444].copy_from_slice(&0x1234_abcdu32.to_le_bytes());
    let image = scratch.image("mbr-id.img", &bytes, 128 * 512);
    let output = run("blkid", &["-s", "PTUUID", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "1234abcd\n");

    bytes[446] = 0x01;
    let image = scratch.image("bad-boot-indicator.img", &bytes, 128 * 512);
    let output = run("blkid", &[], &[&image]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn mount_never_uses_an_empty_filesystem_type_for_partition_metadata() {
    let scratch = Scratch::new("mount-partition");
    let mut bytes = vec![0; 512];
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    let image = scratch.image("mbr.img", &bytes, 128 * 512);
    let target = scratch.0.join("mnt");
    fs::create_dir(&target).expect("create mount target");
    let filesystems =
        fs::read_to_string("/proc/filesystems").expect("read kernel filesystem types");
    let excluded = filesystems
        .lines()
        .filter(|line| !line.starts_with("nodev"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    let types = format!("no{}", excluded.join(","));
    let output = run("mount", &["-f", "-i", "-t", &types], &[&image, &target]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("failed to determine filesystem type"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn luks_headers_require_their_versioned_header_and_expose_luks2_label() {
    let scratch = Scratch::new("luks");
    let image = scratch.image("short.img", b"LUKS\xba\xbe", 6);
    let output = run("blkid", &[], &[&image]);
    assert_eq!(output.status.code(), Some(2));

    let mut bytes = vec![0; 4096];
    bytes[..6].copy_from_slice(b"LUKS\xba\xbe");
    bytes[6..8].copy_from_slice(&2u16.to_be_bytes());
    bytes[8..16].copy_from_slice(&16_384u64.to_be_bytes());
    bytes[24..31].copy_from_slice(b"secrets");
    bytes[168..204].copy_from_slice(b"12345678-1234-5678-9abc-123456789abc");
    let image = scratch.image("luks2.img", &bytes, 32_768);
    let output = run("blkid", &["-o", "export"], &[&image]);
    let text = success(&output);
    assert!(text.contains("LABEL=secrets\n"), "{text}");
    assert!(
        text.contains("UUID=12345678-1234-5678-9abc-123456789abc\n"),
        "{text}"
    );
    assert!(text.contains("TYPE=crypto_LUKS\n"), "{text}");
    assert!(!text.contains("VERSION="), "{text}");
    let output = run("blkid", &["-p", "-s", "VERSION", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "2\n");

    bytes[256..264].copy_from_slice(&16_384u64.to_be_bytes());
    let image = scratch.image("wrong-offset.img", &bytes, 32_768);
    assert_eq!(run("blkid", &[], &[&image]).status.code(), Some(2));
    bytes[256..264].fill(0);
    bytes[6..8].copy_from_slice(&3u16.to_be_bytes());
    let image = scratch.image("unknown-version.img", &bytes, 32_768);
    assert_eq!(run("blkid", &[], &[&image]).status.code(), Some(2));

    bytes[6..8].copy_from_slice(&1u16.to_be_bytes());
    let image = scratch.image("luks1.img", &bytes[..592], 592);
    let output = run("blkid", &["-p", "-s", "VERSION", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "1\n");
}

fn swap_header(page: usize, version: u32, last_page: u32, big_endian: bool) -> Vec<u8> {
    let mut bytes = vec![0; page];
    let encode = |value: u32| {
        if big_endian {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        }
    };
    bytes[1024..1028].copy_from_slice(&encode(version));
    bytes[1028..1032].copy_from_slice(&encode(last_page));
    bytes[1036..1052].fill(0x11);
    bytes[1052..1060].copy_from_slice(b"swapvol\0");
    bytes[page - 10..].copy_from_slice(b"SWAPSPACE2");
    bytes
}

#[test]
fn swap_versions_and_page_bounds_are_checked_without_reusing_v0_metadata() {
    let scratch = Scratch::new("swap");
    for (page, big_endian) in [(4096, false), (65_536, true)] {
        let bytes = swap_header(page, 1, 9, big_endian);
        let image = scratch.image(&format!("swap-{page}.img"), &bytes, 10 * page as u64);
        let output = run("blkid", &["-p", "-o", "export"], &[&image]);
        let text = success(&output);
        assert!(text.contains("TYPE=swap\n"), "{text}");
        assert!(text.contains("VERSION=1\n"), "{text}");
        assert!(text.contains("LABEL=swapvol\n"), "{text}");
    }
    for (name, version, last_page, length) in [
        ("bad-version", 2, 9, 40_960),
        ("zero-pages", 1, 0, 40_960),
        ("past-end", 1, 10, 40_960),
        ("single-page", 1, 9, 4096),
    ] {
        let bytes = swap_header(4096, version, last_page, false);
        let image = scratch.image(name, &bytes, length);
        let output = run("blkid", &[], &[&image]);
        assert_eq!(output.status.code(), Some(2), "{name}");
        assert!(output.stdout.is_empty(), "{name}");
    }
    let mut bytes = swap_header(4096, 99, 9, false);
    bytes[4086..].copy_from_slice(b"SWAP-SPACE");
    let image = scratch.image("v0.img", &bytes, 40_960);
    let output = run("blkid", &["-p", "-o", "export"], &[&image]);
    assert_eq!(
        success(&output),
        format!("DEVNAME={}\nVERSION=0\nTYPE=swap\n", image.display())
    );
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

fn gpt_image(sector: usize, backup_only: bool) -> Vec<u8> {
    let mut bytes = vec![0; 1024 * 1024];
    let last_lba = bytes.len() / sector - 1;
    bytes[450] = 0xee;
    bytes[454..458].copy_from_slice(&1u32.to_le_bytes());
    bytes[458..462].copy_from_slice(&(last_lba as u32).to_le_bytes());
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    let entries_length: usize = 128 * 128;
    let entries_lba = if backup_only {
        last_lba - entries_length.div_ceil(sector)
    } else {
        2
    };
    let header_lba = if backup_only { last_lba } else { 1 };
    let entries_start = entries_lba * sector;
    bytes[entries_start..entries_start + 16].fill(0x11);
    let entries_crc = crc32(&bytes[entries_start..entries_start + entries_length]);
    let header = &mut bytes[header_lba * sector..(header_lba + 1) * sector];
    header[..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[24..32].copy_from_slice(&(header_lba as u64).to_le_bytes());
    let alternate = if backup_only { 1 } else { last_lba };
    header[32..40].copy_from_slice(&(alternate as u64).to_le_bytes());
    header[40..48].copy_from_slice(&34u64.to_le_bytes());
    header[48..56].copy_from_slice(&((last_lba - 34) as u64).to_le_bytes());
    header[56..72].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    header[72..80].copy_from_slice(&(entries_lba as u64).to_le_bytes());
    header[80..84].copy_from_slice(&128u32.to_le_bytes());
    header[84..88].copy_from_slice(&128u32.to_le_bytes());
    header[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let header_crc = crc32(&header[..92]);
    header[16..20].copy_from_slice(&header_crc.to_le_bytes());
    bytes
}

#[test]
fn gpt_uses_logical_sector_size_and_checks_both_crcs_and_protective_mbr() {
    let scratch = Scratch::new("gpt");
    for sector in [512, 4096] {
        for backup_only in [false, true] {
            let bytes = gpt_image(sector, backup_only);
            let image = scratch.image(
                &format!("gpt-{sector}-{backup_only}.img"),
                &bytes,
                bytes.len() as u64,
            );
            let output = run("blkid", &["-o", "export"], &[&image]);
            assert_eq!(
                success(&output),
                format!(
                    "DEVNAME={}\nPTUUID=04030201-0605-0807-090a-0b0c0d0e0f10\nPTTYPE=gpt\n",
                    image.display()
                )
            );
        }
        for (name, offset) in [
            ("header-crc", sector + 16),
            ("entries-crc", sector * 2),
            ("protective-mbr", 450),
        ] {
            let mut bytes = gpt_image(sector, false);
            bytes[offset] ^= 1;
            let image = scratch.image(&format!("{name}-{sector}.img"), &bytes, bytes.len() as u64);
            let output = run("blkid", &["-s", "PTTYPE", "-o", "value"], &[&image]);
            assert_ne!(output.stdout, b"gpt\n", "{name}, sector {sector}");
        }
    }
}

#[test]
fn blkid_token_lookup_is_case_sensitive_and_list_one_stops_after_first_match() {
    let scratch = Scratch::new("lookup");
    let first = scratch.image("first.img", &fat16_header(), 16 * 1024 * 1024);
    let second = scratch.image("second.img", &fat16_header(), 16 * 1024 * 1024);
    let output = run(
        "blkid",
        &["-l", "-t", "TYPE=vfat", "-o", "device"],
        &[&first, &second],
    );
    assert_eq!(success(&output), format!("{}\n", first.display()));
    let output = run("blkid", &["-t", "type=vfat"], &[&first]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    for args in [
        vec!["-l"],
        vec!["-t", "vfat"],
        vec!["-t", "TYPE=vfat", "-t", "LABEL=FAT VOLUME"],
        vec!["-o", "not-a-format"],
    ] {
        let output = run("blkid", &args, &[&first]);
        assert_eq!(output.status.code(), Some(4), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn label_and_uuid_lookups_allow_device_operands_on_either_side() {
    let scratch = Scratch::new("evaluate-order");
    let image = scratch.image("image.img", &fat16_header(), 16 * 1024 * 1024);
    let image_name = image.to_str().expect("UTF-8 image path");
    let missing = format!("axe-storage-missing-{}", std::process::id());
    for option in ["-L", "-U"] {
        for args in [
            vec![image_name, option, missing.as_str()],
            vec![option, missing.as_str(), image_name],
        ] {
            let output = run("blkid", &args, &[]);
            assert_eq!(output.status.code(), Some(2), "{args:?}");
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty(), "{args:?}");
        }
    }
}

#[test]
fn blockdev_report_is_first_argument_only_and_continues_after_explicit_failures() {
    let scratch = Scratch::new("blockdev-report");
    let first = scratch.0.join("missing-first");
    let second = scratch.0.join("missing-second");
    let output = run("blockdev", &["--report"], &[&first, &second]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        output.stdout,
        b"RO    RA   SSZ   BSZ        StartSec            Size   Device\n"
    );
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(
        text.contains(first.to_str().expect("UTF-8 first path")),
        "{text}"
    );
    assert!(
        text.contains(second.to_str().expect("UTF-8 second path")),
        "{text}"
    );

    let output = run("blockdev", &["-v", "--report"], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no device specified"));
}

#[test]
fn blockdev_opens_report_and_command_devices_without_waiting_for_fifo_writers() {
    let scratch = Scratch::new("blockdev-nonblock");
    let path = scratch.0.join("fifo");
    let c_path = CString::new(path.as_os_str().as_bytes()).expect("FIFO path has no NUL");
    // SAFETY: c_path is a NUL-terminated path and mode has only permission bits.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    for option in ["--report", "--getss"] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", "blockdev", "--", option])
            .arg(&path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start blockdev on FIFO");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("inspect blockdev exit").is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().expect("stop blocked blockdev");
                child.wait().expect("reap blocked blockdev");
                panic!("blockdev {option} waited for a FIFO writer");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().expect("collect blockdev output");
        assert_eq!(output.status.code(), Some(1));
    }
}

#[test]
fn default_blockdev_report_is_quiet_and_returns_the_aggregate_status() {
    let output = run("blockdev", &["--report"], &[]);
    assert!(matches!(output.status.code(), Some(0 | 1)));
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output
            .stdout
            .starts_with(b"RO    RA   SSZ   BSZ        StartSec            Size   Device\n")
    );
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

fn ext_header(compat: u32, incompat: u32, read_only: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; 2048];
    let sb = &mut bytes[1024..];
    sb[0..4].copy_from_slice(&2048u32.to_le_bytes());
    sb[4..8].copy_from_slice(&16_384u32.to_le_bytes());
    sb[20..24].copy_from_slice(&1u32.to_le_bytes());
    sb[32..36].copy_from_slice(&8192u32.to_le_bytes());
    sb[40..44].copy_from_slice(&1024u32.to_le_bytes());
    sb[56..58].copy_from_slice(&[0x53, 0xef]);
    sb[76..80].copy_from_slice(&1u32.to_le_bytes());
    sb[88..90].copy_from_slice(&128u16.to_le_bytes());
    sb[92..96].copy_from_slice(&compat.to_le_bytes());
    sb[96..100].copy_from_slice(&incompat.to_le_bytes());
    sb[100..104].copy_from_slice(&read_only.to_le_bytes());
    sb[104..120].fill(0x11);
    sb[120..130].copy_from_slice(b"NO_EXTENTS");
    if read_only & 0x400 != 0 {
        let checksum = crc32c(&sb[..1020]);
        sb[1020..].copy_from_slice(&checksum.to_le_bytes());
    }
    bytes
}

fn xfs_header(version: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 512];
    bytes[..4].copy_from_slice(b"XFSB");
    bytes[4..8].copy_from_slice(&4096u32.to_be_bytes());
    bytes[8..16].copy_from_slice(&4096u64.to_be_bytes());
    bytes[32..48].fill(0x22);
    bytes[80..84].copy_from_slice(&1u32.to_be_bytes());
    bytes[84..88].copy_from_slice(&4096u32.to_be_bytes());
    bytes[88..92].copy_from_slice(&1u32.to_be_bytes());
    bytes[100..102].copy_from_slice(&version.to_be_bytes());
    bytes[102..104].copy_from_slice(&512u16.to_be_bytes());
    bytes[104..106].copy_from_slice(&512u16.to_be_bytes());
    bytes[106..108].copy_from_slice(&8u16.to_be_bytes());
    bytes[120] = 12;
    bytes[121] = 9;
    bytes[122] = 9;
    bytes[123] = 3;
    bytes[124] = 12;
    if version & 0xf == 5 {
        bytes[200..204].copy_from_slice(&0x100u32.to_be_bytes());
        let checksum = !crc32c(&bytes);
        bytes[224..228].copy_from_slice(&checksum.to_le_bytes());
    }
    bytes
}

fn btrfs_header() -> Vec<u8> {
    let mut bytes = vec![0u8; 69_632];
    let sb = &mut bytes[65_536..];
    sb[32..48].fill(0x33);
    sb[48..56].copy_from_slice(&65_536u64.to_le_bytes());
    sb[64..72].copy_from_slice(b"_BHRfS_M");
    sb[72..80].copy_from_slice(&1u64.to_le_bytes());
    sb[80..88].copy_from_slice(&4096u64.to_le_bytes());
    sb[88..96].copy_from_slice(&8192u64.to_le_bytes());
    sb[112..120].copy_from_slice(&(16u64 * 1024 * 1024).to_le_bytes());
    sb[136..144].copy_from_slice(&1u64.to_le_bytes());
    sb[144..148].copy_from_slice(&4096u32.to_le_bytes());
    sb[148..152].copy_from_slice(&16_384u32.to_le_bytes());
    sb[209..217].copy_from_slice(&(16u64 * 1024 * 1024).to_le_bytes());
    let checksum = !crc32c(&sb[32..]);
    sb[..4].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn squashfs_header() -> Vec<u8> {
    let mut bytes = vec![0u8; 96];
    bytes[..4].copy_from_slice(b"hsqs");
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    bytes[12..16].copy_from_slice(&131_072u32.to_le_bytes());
    bytes[20..22].copy_from_slice(&1u16.to_le_bytes());
    bytes[22..24].copy_from_slice(&17u16.to_le_bytes());
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&4u16.to_le_bytes());
    bytes[40..48].copy_from_slice(&4096u64.to_le_bytes());
    for (offset, table) in [
        (48, 256),
        (56, u64::MAX),
        (64, 96),
        (72, 128),
        (80, u64::MAX),
        (88, u64::MAX),
    ] {
        bytes[offset..offset + 8].copy_from_slice(&table.to_le_bytes());
    }
    bytes
}

fn ntfs_header() -> Vec<u8> {
    let mut bytes = vec![0u8; 8192];
    bytes[3..11].copy_from_slice(b"NTFS    ");
    bytes[11..13].copy_from_slice(&512u16.to_le_bytes());
    bytes[13] = 8;
    bytes[40..48].copy_from_slice(&2048u64.to_le_bytes());
    bytes[48..56].copy_from_slice(&1u64.to_le_bytes());
    bytes[56..64].copy_from_slice(&2u64.to_le_bytes());
    bytes[64] = 0xf6;
    bytes[68] = 0xf6;
    bytes[72..80].copy_from_slice(&0x1234_5678_90ab_cdefu64.to_le_bytes());
    bytes[510..512].copy_from_slice(&[0x55, 0xaa]);
    for offset in [4096, 7168] {
        bytes[offset..offset + 4].copy_from_slice(b"FILE");
        bytes[offset + 20..offset + 22].copy_from_slice(&48u16.to_le_bytes());
        bytes[offset + 24..offset + 28].copy_from_slice(&48u32.to_le_bytes());
        bytes[offset + 28..offset + 32].copy_from_slice(&1024u32.to_le_bytes());
    }
    bytes
}

#[test]
fn filesystem_magic_requires_complete_structurally_valid_superblocks() {
    let scratch = Scratch::new("superblocks");
    let mut ntfs = vec![0u8; 16];
    ntfs[3..11].copy_from_slice(b"NTFS    ");
    let mut ext = vec![0u8; 1082];
    ext[1080..].copy_from_slice(&[0x53, 0xef]);
    let mut btrfs = vec![0u8; 66_091];
    btrfs[65_600..65_608].copy_from_slice(b"_BHRfS_M");
    for (name, header) in [
        ("xfs", b"XFSB".to_vec()),
        ("squashfs", b"hsqs".to_vec()),
        ("ntfs", ntfs),
        ("ext", ext),
        ("btrfs", btrfs),
    ] {
        let image = scratch.image(name, &header, header.len() as u64);
        let output = run("blkid", &["-p", "-o", "export"], &[&image]);
        assert_eq!(output.status.code(), Some(2), "{name}");
        assert!(output.stdout.is_empty(), "{name}: {:?}", output.stdout);
    }
    for (name, header, length, expected) in [
        ("xfs4", xfs_header(4), 16 * 1024 * 1024, "xfs"),
        ("xfs5", xfs_header(0x8005), 16 * 1024 * 1024, "xfs"),
        ("squashfs", squashfs_header(), 4096, "squashfs"),
        ("ntfs", ntfs_header(), 1024 * 1024, "ntfs"),
        ("btrfs", btrfs_header(), 16 * 1024 * 1024, "btrfs"),
    ] {
        let image = scratch.image(name, &header, length);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert_eq!(success(&output), format!("{expected}\n"), "{name}");
        let truncated = scratch.image(&format!("{name}-short"), &header, header.len() as u64);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&truncated]);
        assert!(output.stdout.is_empty(), "{name}: {:?}", output.stdout);
    }
}

#[test]
fn ext_family_uses_all_incompatible_and_read_only_features_without_requiring_extents() {
    let scratch = Scratch::new("ext-family");
    for (name, compat, incompat, read_only, expected) in [
        ("ext2", 0, 2, 3, "ext2"),
        ("ext3", 4, 2, 3, "ext3"),
        ("ext4-no-extents", 4, 0x202, 0x46b, "ext4"),
        ("ext4-no-journal", 0, 2, 0x40, "ext4"),
    ] {
        let bytes = ext_header(compat, incompat, read_only);
        let image = scratch.image(name, &bytes, 16 * 1024 * 1024);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert_eq!(success(&output), format!("{expected}\n"));
        let output = run("blkid", &["-s", "LABEL", "-o", "value"], &[&image]);
        assert_eq!(success(&output), "NO_EXTENTS\n");
    }
}

#[test]
fn superblock_geometry_and_metadata_checksums_reject_corruption() {
    let scratch = Scratch::new("corrupt-superblocks");
    for (name, mut bytes, offset) in [
        ("ext-geometry", ext_header(4, 2, 3), 1056),
        ("ext-checksum", ext_header(4, 2, 0x400), 1144),
        ("xfs-geometry", xfs_header(4), 102),
        ("xfs-checksum", xfs_header(0x8005), 108),
        ("btrfs-checksum", btrfs_header(), 65_835),
        ("squashfs-blocksize", squashfs_header(), 12),
        ("ntfs-record", ntfs_header(), 4096),
    ] {
        bytes[offset] ^= 0x80;
        if name == "ext-geometry" {
            bytes[1056..1060].fill(0);
        }
        let image = scratch.image(name, &bytes, 16 * 1024 * 1024);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert!(output.stdout.is_empty(), "{name}: {:?}", output.stdout);
    }
}

#[test]
fn swap_precedes_stale_ext_metadata_and_independent_valid_filesystems_are_ambivalent() {
    let scratch = Scratch::new("signature-preference");
    let mut bytes = ext_header(4, 0x242, 0x46b);
    bytes.resize(4096, 0);
    bytes[1024..1028].copy_from_slice(&1u32.to_le_bytes());
    bytes[1028..1032].copy_from_slice(&4095u32.to_le_bytes());
    bytes[1032..1036].fill(0);
    bytes[4086..].copy_from_slice(b"SWAPSPACE2");
    let image = scratch.image("swap-over-ext", &bytes, 16 * 1024 * 1024);
    let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "swap\n");

    let mut bytes = btrfs_header();
    bytes[..2048].copy_from_slice(&ext_header(0, 2, 3));
    let image = scratch.image("two-filesystems", &bytes, 16 * 1024 * 1024);
    let output = run("blkid", &["-p", "-o", "export"], &[&image]);
    assert_eq!(output.status.code(), Some(8));
    assert!(output.stdout.is_empty());
    let output = run("blkid", &["-o", "export"], &[&image]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn fat_root_label_overrides_boot_label_and_export_uses_shell_escaping() {
    let scratch = Scratch::new("fat-root-label");
    let image = scratch.image("fat.img", &fat16_header(), 16 * 1024 * 1024);
    let mut entry = [0u8; 32];
    entry[..11].copy_from_slice(b"ROOT LABEL ");
    entry[11] = 8;
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .expect("open FAT image");
    std::os::unix::fs::FileExt::write_all_at(&file, &entry, 257 * 512).expect("write root label");
    let output = run("blkid", &["-p", "-o", "export"], &[&image]);
    let text = success(&output);
    assert!(text.contains("LABEL_FATBOOT=FAT\\ VOLUME\n"), "{text}");
    assert!(text.contains("LABEL=ROOT\\ LABEL\n"), "{text}");
    let output = run("blkid", &["-s", "LABEL", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "ROOT LABEL\n");

    let mut bytes = ext_header(0, 2, 3);
    bytes[1144..1160].fill(0);
    bytes[1144..1153].copy_from_slice(b"a b\"'$`<>");
    let image = scratch.image("escaped-label", &bytes, 16 * 1024 * 1024);
    let output = run("blkid", &["-o", "export", "-s", "LABEL"], &[&image]);
    assert_eq!(
        success(&output),
        format!(
            "DEVNAME={}\nLABEL=a\\ b\\\"\\'\\$\\`\\<\\>\n",
            image.display()
        )
    );
}

#[test]
fn blkid_mountpoint_and_hexdump_preserve_non_utf8_operands() {
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::MetadataExt;
    let scratch = Scratch::new("raw-paths");
    let image = scratch
        .0
        .join(std::ffi::OsString::from_vec(b"fat-\xff".to_vec()));
    let temporary = scratch.image("temporary", &fat16_header(), 16 * 1024 * 1024);
    fs::rename(temporary, &image).expect("rename image");
    let output = run("blkid", &["-o", "device"], &[&image]);
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        output.stdout,
        [image.as_os_str().as_bytes(), b"\n"].concat()
    );
    let directory = scratch
        .0
        .join(std::ffi::OsString::from_vec(b"directory-\xfe".to_vec()));
    fs::create_dir(&directory).expect("create raw directory");
    let output = run("mountpoint", &[], &[&directory]);
    assert_eq!(output.status.code(), Some(32));
    assert_eq!(
        output.stdout,
        [directory.as_os_str().as_bytes(), b" is not a mountpoint\n"].concat()
    );
    let missing = directory.join("missing");
    for (path, expected) in [
        (Path::new("/"), 0),
        (directory.as_path(), 32),
        (missing.as_path(), 1),
    ] {
        let output = run("mountpoint", &["-q"], &[path]);
        assert_eq!(output.status.code(), Some(expected));
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    let metadata = fs::metadata("/").expect("root metadata");
    let output = run("mountpoint", &["-d"], &[Path::new("/")]);
    assert_eq!(
        success(&output),
        format!(
            "{}:{}\n",
            libc::major(metadata.dev()),
            libc::minor(metadata.dev())
        )
    );
    let output = run("mountpoint", &["-x"], &[Path::new("/dev/null")]);
    assert_eq!(output.status.code(), Some(32));
    assert!(output.stdout.is_empty());

    let input = scratch
        .0
        .join(std::ffi::OsString::from_vec(b"bytes-\xfd".to_vec()));
    fs::write(&input, [0x61, 0xff]).expect("write byte input");
    for (applet, args) in [("hexdump", &["-C"][..]), ("hd", &[][..])] {
        let output = run(applet, args, &[&input]);
        assert_eq!(
            success(&output),
            "00000000  61 ff                                             |a.|\n00000002\n"
        );
    }
}

#[test]
fn btrfs_accepts_all_standard_checksum_types_and_checks_the_entire_superblock() {
    let scratch = Scratch::new("btrfs-checksum-types");
    for kind in 0u16..=3 {
        let mut bytes = btrfs_header();
        let sb = &mut bytes[65_536..];
        sb[..32].fill(0);
        sb[196..198].copy_from_slice(&kind.to_le_bytes());
        let checksum = match kind {
            0 => (!crc32c(&sb[32..])).to_le_bytes().to_vec(),
            1 => xxhash_rust::xxh64::xxh64(&sb[32..], 0)
                .to_le_bytes()
                .to_vec(),
            2 => {
                use sha2::Digest;
                sha2::Sha256::digest(&sb[32..]).to_vec()
            }
            3 => blake2b_simd::Params::new()
                .hash_length(32)
                .hash(&sb[32..])
                .as_bytes()
                .to_vec(),
            _ => unreachable!(),
        };
        sb[..checksum.len()].copy_from_slice(&checksum);
        let image = scratch.image(&format!("valid-{kind}"), &bytes, 16 * 1024 * 1024);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert_eq!(success(&output), "btrfs\n", "checksum type {kind}");
        bytes[69_631] ^= 1;
        let image = scratch.image(&format!("corrupt-{kind}"), &bytes, 16 * 1024 * 1024);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert!(output.stdout.is_empty(), "checksum type {kind}");
    }
}

#[test]
fn fat32_root_label_search_follows_the_chain_beyond_the_initial_probe_buffer() {
    let scratch = Scratch::new("fat32-root-chain");
    let mut bytes = fat16_header();
    bytes[14..16].copy_from_slice(&32u16.to_le_bytes());
    bytes[17..19].fill(0);
    bytes[22..24].fill(0);
    bytes[36..40].copy_from_slice(&256u32.to_le_bytes());
    bytes[42..44].fill(0);
    bytes[44..48].copy_from_slice(&2u32.to_le_bytes());
    bytes[66] = 0x29;
    bytes[67..71].copy_from_slice(&0x1234_abcdu32.to_le_bytes());
    bytes[71..82].copy_from_slice(b"BOOTLABEL  ");
    bytes[82..90].copy_from_slice(b"FAT32   ");
    let image = scratch.image("fat32.img", &bytes, 16 * 1024 * 1024);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .expect("open FAT32 image");
    std::os::unix::fs::FileExt::write_all_at(&file, &3u32.to_le_bytes(), 32 * 512 + 8)
        .expect("write root chain");
    let data_offset = (32 + 2 * 256) * 512;
    let mut first = [0u8; 512];
    for entry in first.as_chunks_mut::<32>().0 {
        entry[0] = 0xe5;
    }
    std::os::unix::fs::FileExt::write_all_at(&file, &first, data_offset)
        .expect("write first root cluster");
    let mut label = [0u8; 32];
    label[..11].copy_from_slice(b"ROOTLABEL  ");
    label[11] = 8;
    std::os::unix::fs::FileExt::write_all_at(&file, &label, data_offset + 512)
        .expect("write second root cluster");
    let output = run("blkid", &["-p", "-o", "export"], &[&image]);
    let text = success(&output);
    assert!(text.contains("LABEL_FATBOOT=BOOTLABEL\n"), "{text}");
    assert!(text.contains("LABEL=ROOTLABEL\n"), "{text}");
    assert!(text.contains("VERSION=FAT32\n"), "{text}");
}

#[test]
fn exfat_f2fs_iso_and_lvm_require_consistent_full_superblocks() {
    let scratch = Scratch::new("other-superblocks");
    let mut exfat = vec![0u8; 512];
    exfat[3..11].copy_from_slice(b"EXFAT   ");
    exfat[72..80].copy_from_slice(&2048u64.to_le_bytes());
    exfat[80..84].copy_from_slice(&24u32.to_le_bytes());
    exfat[84..88].copy_from_slice(&16u32.to_le_bytes());
    exfat[88..92].copy_from_slice(&40u32.to_le_bytes());
    exfat[92..96].copy_from_slice(&2008u32.to_le_bytes());
    exfat[96..100].copy_from_slice(&2u32.to_le_bytes());
    exfat[105] = 1;
    exfat[108] = 9;
    exfat[110] = 1;
    exfat[510..512].copy_from_slice(&[0x55, 0xaa]);
    let mut f2fs = vec![0u8; 4096];
    f2fs[1024..1028].copy_from_slice(&0xf2f5_2010u32.to_le_bytes());
    f2fs[1032..1036].copy_from_slice(&9u32.to_le_bytes());
    f2fs[1036..1040].copy_from_slice(&3u32.to_le_bytes());
    f2fs[1040..1044].copy_from_slice(&12u32.to_le_bytes());
    f2fs[1044..1048].copy_from_slice(&9u32.to_le_bytes());
    f2fs[1048..1052].copy_from_slice(&1u32.to_le_bytes());
    f2fs[1052..1056].copy_from_slice(&1u32.to_le_bytes());
    f2fs[1060..1068].copy_from_slice(&512u64.to_le_bytes());
    f2fs[1068..1072].copy_from_slice(&1u32.to_le_bytes());
    f2fs[1072..1076].copy_from_slice(&1u32.to_le_bytes());
    let mut iso = vec![0u8; 34_816];
    let descriptor = &mut iso[32_768..];
    descriptor[0] = 1;
    descriptor[1..6].copy_from_slice(b"CD001");
    descriptor[6] = 1;
    descriptor[80..84].copy_from_slice(&40u32.to_le_bytes());
    descriptor[84..88].copy_from_slice(&40u32.to_be_bytes());
    descriptor[128..130].copy_from_slice(&2048u16.to_le_bytes());
    descriptor[130..132].copy_from_slice(&2048u16.to_be_bytes());
    let mut lvm = vec![0u8; 1024];
    let label = &mut lvm[512..];
    label[..8].copy_from_slice(b"LABELONE");
    label[8..16].copy_from_slice(&1u64.to_le_bytes());
    label[20..24].copy_from_slice(&32u32.to_le_bytes());
    label[24..32].copy_from_slice(b"LVM2 001");
    label[32..64].fill(b'a');
    label[64..72].copy_from_slice(&(1024u64 * 1024).to_le_bytes());
    let mut checksum = 0xf597_a6cfu32;
    for byte in &label[20..] {
        checksum ^= u32::from(*byte);
        for _ in 0..8 {
            checksum = (checksum >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(checksum & 1));
        }
    }
    label[16..20].copy_from_slice(&checksum.to_le_bytes());
    for (name, bytes, length, invalid_offset, expected) in [
        ("exfat", exfat, 1024 * 1024, 80, "exfat"),
        ("f2fs", f2fs, 2 * 1024 * 1024, 1040, "f2fs"),
        ("iso", iso, 40 * 2048, 32_852, "iso9660"),
        ("lvm", lvm, 1024 * 1024, 600, "LVM2_member"),
    ] {
        let image = scratch.image(name, &bytes, length);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert_eq!(success(&output), format!("{expected}\n"));
        let mut invalid = bytes;
        invalid[invalid_offset] ^= 0x80;
        let image = scratch.image(&format!("bad-{name}"), &invalid, length);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert!(output.stdout.is_empty(), "{name}: {:?}", output.stdout);
    }
}

#[test]
fn legacy_squashfs_versions_keep_their_endianness_and_geometry() {
    let scratch = Scratch::new("legacy-squashfs");
    for major in 1u16..=3 {
        for big_endian in [false, true] {
            let mut bytes = vec![0u8; 119];
            let encode16 = |value: u16| {
                if big_endian {
                    value.to_be_bytes()
                } else {
                    value.to_le_bytes()
                }
            };
            let encode32 = |value: u32| {
                if big_endian {
                    value.to_be_bytes()
                } else {
                    value.to_le_bytes()
                }
            };
            let encode64 = |value: u64| {
                if big_endian {
                    value.to_be_bytes()
                } else {
                    value.to_le_bytes()
                }
            };
            bytes[..4].copy_from_slice(if big_endian { b"sqsh" } else { b"hsqs" });
            bytes[4..8].copy_from_slice(&encode32(1));
            bytes[8..12].copy_from_slice(&encode32(4096));
            bytes[20..24].copy_from_slice(&encode32(128));
            bytes[24..28].copy_from_slice(&encode32(160));
            bytes[28..30].copy_from_slice(&encode16(major));
            bytes[32..34].copy_from_slice(&encode16(4096));
            bytes[34..36].copy_from_slice(&encode16(12));
            bytes[51..55].copy_from_slice(&encode32(4096));
            bytes[63..71].copy_from_slice(&encode64(4096));
            bytes[87..95].copy_from_slice(&encode64(128));
            bytes[95..103].copy_from_slice(&encode64(160));
            let image = scratch.image(&format!("v{major}-{big_endian}"), &bytes, 4096);
            let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
            assert_eq!(success(&output), "squashfs3\n");
            let output = run("blkid", &["-p", "-s", "VERSION", "-o", "value"], &[&image]);
            assert_eq!(success(&output), format!("{major}.0\n"));
            bytes[34..36].copy_from_slice(&encode16(31));
            let image = scratch.image(&format!("bad-v{major}-{big_endian}"), &bytes, 4096);
            let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn ext4_bigalloc_first_data_block_uses_cluster_geometry() {
    let scratch = Scratch::new("ext4-bigalloc");
    let mut bytes = ext_header(4, 0x242, 0x66b);
    let sb = &mut bytes[1024..];
    sb[4..8].copy_from_slice(&131_072u32.to_le_bytes());
    sb[20..24].copy_from_slice(&0u32.to_le_bytes());
    sb[28..32].copy_from_slice(&4u32.to_le_bytes());
    sb[32..36].copy_from_slice(&131_072u32.to_le_bytes());
    sb[36..40].copy_from_slice(&8192u32.to_le_bytes());
    sb[40..44].copy_from_slice(&2048u32.to_le_bytes());
    let checksum = crc32c(&sb[..1020]);
    sb[1020..].copy_from_slice(&checksum.to_le_bytes());
    let image = scratch.image("bigalloc.img", &bytes, 128 * 1024 * 1024);
    let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "ext4\n");
    let output = run(
        "blkid",
        &["-p", "-s", "BLOCK_SIZE", "-o", "value"],
        &[&image],
    );
    assert_eq!(success(&output), "1024\n");

    bytes[1044..1048].copy_from_slice(&1u32.to_le_bytes());
    let checksum = crc32c(&bytes[1024..2044]);
    bytes[2044..2048].copy_from_slice(&checksum.to_le_bytes());
    let image = scratch.image("inconsistent-first-block.img", &bytes, 128 * 1024 * 1024);
    let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}

#[test]
fn f2fs_multi_device_bounds_use_primary_member_segments_not_aggregate_blocks() {
    let scratch = Scratch::new("f2fs-members");
    let mut bytes = vec![0u8; 4096];
    let sb = &mut bytes[1024..];
    for (offset, value) in [
        (0, 0xf2f5_2010u32),
        (8, 9),
        (12, 3),
        (16, 12),
        (20, 9),
        (24, 1),
        (28, 1),
        (44, 127),
        (48, 127),
        (72, 512),
    ] {
        sb[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    sb[36..44].copy_from_slice(&65_536u64.to_le_bytes());
    sb[2201..2208].copy_from_slice(b"primary");
    sb[2265..2269].copy_from_slice(&63u32.to_le_bytes());
    sb[2269..2278].copy_from_slice(b"secondary");
    sb[2333..2337].copy_from_slice(&64u32.to_le_bytes());
    let image = scratch.image("primary.img", &bytes, 128 * 1024 * 1024);
    let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
    assert_eq!(success(&output), "f2fs\n");

    for (name, offset, replacement) in [
        ("primary-past-end", 1024 + 2265, 64u32.to_le_bytes()),
        ("segments-past-total", 1024 + 2333, 65u32.to_le_bytes()),
        ("missing-secondary", 1024 + 2269, [0u8; 4]),
    ] {
        let mut invalid = bytes.clone();
        invalid[offset..offset + replacement.len()].copy_from_slice(&replacement);
        let image = scratch.image(name, &invalid, 128 * 1024 * 1024);
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert_eq!(output.status.code(), Some(2), "{name}");
        assert!(output.stdout.is_empty(), "{name}: {:?}", output.stdout);
    }
}

#[test]
fn fat12_includes_4084_clusters_in_both_entry_capacity_and_version() {
    let scratch = Scratch::new("fat12-boundary");
    for (clusters, fat_sectors, expected) in [
        (4083u16, 12u16, "FAT12"),
        (4084, 12, "FAT12"),
        (4085, 16, "FAT16"),
    ] {
        let mut bytes = fat16_header();
        bytes[17..19].copy_from_slice(&224u16.to_le_bytes());
        let sectors = clusters + 1 + 2 * fat_sectors + 14;
        bytes[19..21].copy_from_slice(&sectors.to_le_bytes());
        bytes[22..24].copy_from_slice(&fat_sectors.to_le_bytes());
        let image = scratch.image(
            &format!("clusters-{clusters}"),
            &bytes,
            u64::from(sectors) * 512,
        );
        let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
        assert_eq!(success(&output), "vfat\n");
        let output = run("blkid", &["-p", "-s", "VERSION", "-o", "value"], &[&image]);
        assert_eq!(success(&output), format!("{expected}\n"));
    }
    let mut bytes = fat16_header();
    bytes[17..19].copy_from_slice(&224u16.to_le_bytes());
    bytes[19..21].copy_from_slice(&4121u16.to_le_bytes());
    bytes[22..24].copy_from_slice(&11u16.to_le_bytes());
    let image = scratch.image("undersized-fat", &bytes, 4121 * 512);
    let output = run("blkid", &["-p", "-s", "TYPE", "-o", "value"], &[&image]);
    assert!(output.stdout.is_empty());
}

#[test]
fn f2fs_labels_decode_the_utf16_volume_name_on_single_and_multi_device_images() {
    let scratch = Scratch::new("f2fs-labels");
    let full_label = "n".repeat(512);
    for (index, (label, multiple)) in [
        ("AXE_F2FS", false),
        ("AXE_MULTI", true),
        ("ĀXE_\u{1d11e}", false),
        (full_label.as_str(), true),
        ("", false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut bytes = vec![0u8; 4096];
        let sb = &mut bytes[1024..];
        let segments = if multiple { 127u32 } else { 63 };
        for (offset, value) in [
            (0, 0xf2f5_2010u32),
            (8, 9),
            (12, 3),
            (16, 12),
            (20, 9),
            (24, 1),
            (28, 1),
            (44, segments),
            (48, segments),
            (72, 512),
        ] {
            sb[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        sb[4..6].copy_from_slice(&1u16.to_le_bytes());
        sb[6..8].copy_from_slice(&1u16.to_le_bytes());
        let blocks = if multiple { 65_536u64 } else { 32_768 };
        sb[36..44].copy_from_slice(&blocks.to_le_bytes());
        sb[108..124].fill(0x44);
        for (word, encoded) in sb[124..1148]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(label.encode_utf16())
        {
            word.copy_from_slice(&encoded.to_le_bytes());
        }
        sb[1148..1152].copy_from_slice(&1u32.to_le_bytes());
        sb[1152..1160].copy_from_slice(b"txt\0\0\0\0\0");
        if multiple {
            sb[2201..2208].copy_from_slice(b"primary");
            sb[2265..2269].copy_from_slice(&63u32.to_le_bytes());
            sb[2269..2278].copy_from_slice(b"secondary");
            sb[2333..2337].copy_from_slice(&64u32.to_le_bytes());
        }
        let image = scratch.image(&format!("label-{index}.img"), &bytes, 128 * 1024 * 1024);
        let output = run("blkid", &["-p", "-s", "LABEL", "-o", "value"], &[&image]);
        let expected = if label.is_empty() {
            String::new()
        } else {
            format!("{label}\n")
        };
        assert_eq!(success(&output), expected);
        let output = run(
            "blkid",
            &["-p", "-s", "UUID", "-s", "TYPE", "-o", "export"],
            &[&image],
        );
        assert_eq!(
            success(&output),
            format!(
                "DEVNAME={}\nUUID=44444444-4444-4444-4444-444444444444\nTYPE=f2fs\n",
                image.display()
            )
        );
        if index < 2 {
            let output = run(
                "blkid",
                &[
                    "-p", "-s", "LABEL", "-s", "UUID", "-s", "TYPE", "-o", "export",
                ],
                &[&image],
            );
            assert_eq!(
                success(&output),
                format!(
                    "DEVNAME={}\nLABEL={label}\nUUID=44444444-4444-4444-4444-444444444444\nTYPE=f2fs\n",
                    image.display()
                )
            );
        }
    }
}

#[test]
fn automatic_mount_rejects_ambiguous_filesystems_but_keeps_unknown_fallback_and_explicit_types() {
    let scratch = Scratch::new("mount-ambiguous");
    let mut bytes = btrfs_header();
    bytes[..2048].copy_from_slice(&ext_header(0, 2, 3));
    let image = scratch.image("ambiguous.img", &bytes, 16 * 1024 * 1024);
    let target = scratch.0.join("target");
    fs::create_dir(&target).expect("create dry-run mount target");
    for args in [
        vec!["-f", "-i"],
        vec!["-f", "-i", "-t", "auto"],
        vec!["-f", "-i", "-t", "no__axe_unknown_filesystem__"],
    ] {
        let output = run("mount", &args, &[&image, &target]);
        assert_eq!(
            output.status.code(),
            Some(32),
            "{args:?}: {:?}",
            output.stderr
        );
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("multiple filesystem signatures"),
            "{args:?}: {:?}",
            output.stderr
        );
    }
    for kind in ["ext2", "btrfs"] {
        let output = run("mount", &["-f", "-i", "-t", kind], &[&image, &target]);
        assert_eq!(success(&output), "");
    }
    let unknown = scratch.image("unknown.img", &[0u8; 4096], 4096);
    let output = run("mount", &["-f", "-i"], &[&unknown, &target]);
    let has_kernel_filesystems = fs::read_to_string("/proc/filesystems")
        .expect("read available kernel filesystem types")
        .lines()
        .any(|line| !line.starts_with("nodev") && !line.trim().is_empty());
    assert_eq!(
        output.status.code(),
        Some(if has_kernel_filesystems { 0 } else { 32 })
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("multiple filesystem signatures"));
}

#[test]
fn blkid_export_keeps_version_before_block_size_for_ext_and_fat() {
    let scratch = Scratch::new("export-field-order");
    for (name, header, version, block_size, kind) in [
        ("ext.img", ext_header(0, 2, 3), "1.0", "1024", "ext2"),
        ("fat.img", fat16_header(), "FAT16", "512", "vfat"),
    ] {
        let image = scratch.image(name, &header, 16 * 1024 * 1024);
        let output = run(
            "blkid",
            &[
                "-p",
                "-o",
                "export",
                "-s",
                "BLOCK_SIZE",
                "-s",
                "VERSION",
                "-s",
                "TYPE",
            ],
            &[&image],
        );
        assert_eq!(
            success(&output),
            format!(
                "DEVNAME={}\nVERSION={version}\nBLOCK_SIZE={block_size}\nTYPE={kind}\n",
                image.display()
            )
        );
    }
}
