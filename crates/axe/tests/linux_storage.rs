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
