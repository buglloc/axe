#![cfg(unix)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::num::NonZeroU64;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lzma_rust2::{CheckType, XzOptions, XzWriter};

const PAYLOAD: &[u8] = b"secret\0payload\xff\n";
const SENTINEL: &[u8] = b"do not replace this target\n";
const FORMATS: [(&str, &str); 4] = [
    ("gzip", "gz"),
    ("bzip2", "bz2"),
    ("xz", "xz"),
    ("zstd", "zst"),
];
const DECODERS: [(&str, &str, &str, bool); 11] = [
    ("gzip", "gzip", "gz", false),
    ("gunzip", "gzip", "gz", false),
    ("zcat", "gzip", "gz", true),
    ("gzcat", "gzip", "gz", true),
    ("bzip2", "bzip2", "bz2", false),
    ("bunzip2", "bzip2", "bz2", false),
    ("bzcat", "bzip2", "bz2", true),
    ("xz", "xz", "xz", false),
    ("unxz", "xz", "xz", false),
    ("xzcat", "xz", "xz", true),
    ("zstd", "zstd", "zst", false),
];

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "axe-compression-{label}-{}-{time}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create isolated scratch directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(directory: &Path, applet: &str, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", applet, "--"])
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("HOME", directory)
        .env("LC_ALL", "C")
        .env("PATH", "/nonexistent")
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn compression applet");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .expect("write bounded fixture");
    child
        .wait_with_output()
        .expect("wait for compression applet")
}

fn success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
}

fn stamp(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    File::open(path)
        .unwrap()
        .set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        )
        .unwrap();
}

fn private_metadata(path: &Path) {
    let metadata = fs::metadata(path).unwrap();
    assert_eq!(metadata.mode() & 0o7777, 0o600, "{}", path.display());
    assert_eq!(metadata.mtime(), 1_700_000_000, "{}", path.display());
}

fn fixture(format: &str, payload: &[u8]) -> Vec<u8> {
    match format {
        "gzip" => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(payload).unwrap();
            encoder.finish().unwrap()
        }
        "bzip2" => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
            encoder.write_all(payload).unwrap();
            encoder.finish().unwrap()
        }
        "xz" => xz_fixture(payload, CheckType::Crc64, false),
        "zstd" => {
            let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
            encoder.include_checksum(true).unwrap();
            encoder.write_all(payload).unwrap();
            encoder.finish().unwrap()
        }
        _ => unreachable!(),
    }
}

fn xz_fixture(payload: &[u8], check: CheckType, multiblock: bool) -> Vec<u8> {
    let mut options = XzOptions::with_preset(1);
    options.check_type = check;
    if multiblock {
        options.lzma_options.dict_size = 4096;
        options.set_block_size(NonZeroU64::new(4096));
    }
    let mut encoder = XzWriter::new(Vec::new(), options).unwrap();
    encoder.write_all(payload).unwrap();
    encoder.finish().unwrap()
}

fn decode(format: &str, compressed: &[u8]) -> Vec<u8> {
    let mut plain = Vec::new();
    match format {
        "gzip" => flate2::read::MultiGzDecoder::new(compressed)
            .read_to_end(&mut plain)
            .unwrap(),
        "bzip2" => bzip2::read::MultiBzDecoder::new(compressed)
            .read_to_end(&mut plain)
            .unwrap(),
        "xz" => lzma_rust2::XzReader::new(compressed, true)
            .read_to_end(&mut plain)
            .unwrap(),
        "zstd" => zstd::stream::read::Decoder::new(compressed)
            .unwrap()
            .read_to_end(&mut plain)
            .unwrap(),
        _ => unreachable!(),
    };
    plain
}

fn no_staging_files(directory: &Path) {
    for entry in fs::read_dir(directory).unwrap() {
        assert!(
            !entry
                .unwrap()
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".axe-compress-")
        );
    }
}

#[test]
fn file_compression_preserves_private_permissions_and_source_timestamp() {
    for (format, suffix) in FORMATS {
        let scratch = Scratch::new(format);
        let source = scratch.path().join("payload");
        fs::write(&source, PAYLOAD).unwrap();
        stamp(&source);
        success(&run(scratch.path(), format, &["--keep", "payload"], b""));
        let destination = scratch.path().join(format!("payload.{suffix}"));
        private_metadata(&destination);
        private_metadata(&source);
        assert_eq!(fs::read(&source).unwrap(), PAYLOAD);
        assert_eq!(decode(format, &fs::read(destination).unwrap()), PAYLOAD);
        no_staging_files(scratch.path());
    }
}

#[test]
fn every_decoder_alias_preserves_private_metadata_and_never_follows_destination_symlinks() {
    for (applet, format, suffix, stdout) in DECODERS {
        let scratch = Scratch::new(applet);
        let compressed = fixture(format, PAYLOAD);
        let filename = format!("payload.{suffix}");
        let source = scratch.path().join(&filename);
        fs::write(&source, &compressed).unwrap();
        stamp(&source);
        let target = scratch.path().join("target");
        fs::write(&target, SENTINEL).unwrap();
        let target_inode = fs::metadata(&target).unwrap().ino();
        let destination = scratch.path().join("payload");
        symlink("target", &destination).unwrap();
        let output = run(scratch.path(), applet, &["-dfk", &filename], b"");
        success(&output);
        assert_eq!(fs::read(&target).unwrap(), SENTINEL, "{applet}");
        assert_eq!(
            fs::metadata(&target).unwrap().ino(),
            target_inode,
            "{applet}"
        );
        assert_eq!(fs::read(&source).unwrap(), compressed, "{applet}");
        private_metadata(&source);
        if stdout {
            assert_eq!(output.stdout, PAYLOAD, "{applet}");
            assert!(
                fs::symlink_metadata(destination)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        } else {
            assert!(output.stdout.is_empty(), "{output:?}");
            assert!(
                !fs::symlink_metadata(&destination)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read(&destination).unwrap(), PAYLOAD, "{applet}");
            private_metadata(&destination);
        }
        no_staging_files(scratch.path());
    }
}

#[test]
fn forced_encoder_publication_detaches_output_links_without_corrupting_kept_input() {
    for (format, suffix) in FORMATS {
        for hardlink_to_input in [false, true] {
            let scratch = Scratch::new(format);
            let source = scratch.path().join("payload");
            let destination = scratch.path().join(format!("payload.{suffix}"));
            fs::write(&source, PAYLOAD).unwrap();
            stamp(&source);
            fs::write(scratch.path().join("target"), SENTINEL).unwrap();
            if hardlink_to_input {
                fs::hard_link(&source, &destination).unwrap();
            } else {
                symlink("target", &destination).unwrap();
            }
            let input_inode = fs::metadata(&source).unwrap().ino();
            let output = run(scratch.path(), format, &["-kf", "payload"], b"");
            assert_eq!(fs::read(&source).unwrap(), PAYLOAD, "{format}: {output:?}");
            assert_eq!(fs::metadata(&source).unwrap().ino(), input_inode);
            private_metadata(&source);
            assert_eq!(fs::read(scratch.path().join("target")).unwrap(), SENTINEL);
            if format == "zstd" && hardlink_to_input {
                assert_eq!(output.status.code(), Some(1), "{output:?}");
                assert_eq!(fs::metadata(destination).unwrap().ino(), input_inode);
            } else {
                success(&output);
                assert_ne!(fs::metadata(&destination).unwrap().ino(), input_inode);
                assert!(
                    !fs::symlink_metadata(&destination)
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
                assert_eq!(decode(format, &fs::read(&destination).unwrap()), PAYLOAD);
                private_metadata(&destination);
            }
            no_staging_files(scratch.path());
        }
    }
}

#[test]
fn every_decoder_alias_handles_output_hardlinks_without_mutating_compressed_input() {
    for (applet, format, suffix, stdout) in DECODERS {
        let scratch = Scratch::new(applet);
        let compressed = fixture(format, PAYLOAD);
        let filename = format!("payload.{suffix}");
        let source = scratch.path().join(&filename);
        fs::write(&source, &compressed).unwrap();
        stamp(&source);
        let destination = scratch.path().join("payload");
        fs::hard_link(&source, &destination).unwrap();
        let inode = fs::metadata(&source).unwrap().ino();
        let output = run(scratch.path(), applet, &["-dfk", &filename], b"");
        assert_eq!(
            fs::read(&source).unwrap(),
            compressed,
            "{applet}: {output:?}"
        );
        assert_eq!(fs::metadata(&source).unwrap().ino(), inode);
        private_metadata(&source);
        if format == "zstd" && !stdout {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
        } else {
            success(&output);
            if stdout {
                assert_eq!(output.stdout, PAYLOAD);
                assert_eq!(fs::metadata(destination).unwrap().ino(), inode);
            } else {
                assert_eq!(fs::read(&destination).unwrap(), PAYLOAD);
                assert_ne!(fs::metadata(&destination).unwrap().ino(), inode);
                private_metadata(&destination);
            }
        }
        no_staging_files(scratch.path());
    }
}

#[test]
fn explicit_zstd_destinations_use_the_same_safe_publication_and_metadata_policy() {
    for decompress in [false, true] {
        for hardlink_to_input in [false, true] {
            let scratch = Scratch::new("zstd-explicit");
            let source = scratch.path().join("source");
            let input = if decompress {
                fixture("zstd", PAYLOAD)
            } else {
                PAYLOAD.to_vec()
            };
            fs::write(&source, &input).unwrap();
            stamp(&source);
            fs::write(scratch.path().join("target"), SENTINEL).unwrap();
            let destination = scratch.path().join("chosen");
            if hardlink_to_input {
                fs::hard_link(&source, &destination).unwrap();
            } else {
                symlink("target", &destination).unwrap();
            }
            let output = run(
                scratch.path(),
                "zstd",
                &[
                    if decompress { "-dfk" } else { "-fk" },
                    "-o",
                    "chosen",
                    "source",
                ],
                b"",
            );
            assert_eq!(fs::read(&source).unwrap(), input);
            assert_eq!(fs::read(scratch.path().join("target")).unwrap(), SENTINEL);
            if hardlink_to_input {
                assert_eq!(output.status.code(), Some(1), "{output:?}");
            } else {
                success(&output);
                private_metadata(&destination);
                let output = fs::read(destination).unwrap();
                assert_eq!(
                    if decompress {
                        output
                    } else {
                        decode("zstd", &output)
                    },
                    PAYLOAD
                );
            }
            no_staging_files(scratch.path());
        }
    }
}

#[test]
fn damaged_input_keeps_source_and_existing_destination_and_cleans_staging_files() {
    for (format, suffix) in FORMATS {
        for existing in [false, true] {
            let scratch = Scratch::new(format);
            let mut compressed = fixture(format, &vec![b'x'; 8192]);
            compressed.truncate(compressed.len() - 5);
            let filename = format!("payload.{suffix}");
            fs::write(scratch.path().join(&filename), &compressed).unwrap();
            let destination = scratch.path().join("payload");
            let inode = if existing {
                fs::write(&destination, SENTINEL).unwrap();
                Some(fs::metadata(&destination).unwrap().ino())
            } else {
                None
            };
            let output = run(scratch.path(), format, &["-df", &filename], b"");
            assert_eq!(
                output.status.code(),
                Some(if format == "bzip2" { 2 } else { 1 }),
                "{format}: {output:?}"
            );
            assert_eq!(
                fs::read(scratch.path().join(&filename)).unwrap(),
                compressed
            );
            if let Some(inode) = inode {
                assert_eq!(fs::metadata(&destination).unwrap().ino(), inode);
                assert_eq!(fs::read(destination).unwrap(), SENTINEL);
            } else {
                assert!(!destination.exists());
            }
            no_staging_files(scratch.path());
        }
    }
}

#[test]
fn encoders_continue_with_every_independent_operand_after_an_open_error() {
    for (format, suffix) in FORMATS {
        let scratch = Scratch::new(format);
        fs::write(scratch.path().join("first"), PAYLOAD).unwrap();
        fs::write(scratch.path().join("second"), SENTINEL).unwrap();
        let output = run(scratch.path(), format, &["missing", "first", "second"], b"");
        assert_eq!(output.status.code(), Some(1), "{format}: {output:?}");
        for (name, plain) in [("first", PAYLOAD), ("second", SENTINEL)] {
            let compressed = fs::read(scratch.path().join(format!("{name}.{suffix}"))).unwrap();
            assert_eq!(decode(format, &compressed), plain);
            assert_eq!(scratch.path().join(name).exists(), format == "zstd");
        }
        no_staging_files(scratch.path());
    }
}

#[test]
fn every_decoder_continues_after_missing_and_corrupt_operands() {
    for (applet, format, suffix, stdout_alias) in DECODERS {
        for corrupt in [false, true] {
            let scratch = Scratch::new(applet);
            let filename = format!("valid.{suffix}");
            fs::write(scratch.path().join(&filename), fixture(format, PAYLOAD)).unwrap();
            let missing = if corrupt {
                let bad = format!("bad.{suffix}");
                fs::write(scratch.path().join(&bad), &fixture(format, PAYLOAD)[..3]).unwrap();
                bad
            } else {
                "missing".to_owned()
            };
            let file_alias = matches!(applet, "gunzip" | "bunzip2" | "unxz");
            let options = if file_alias { "-d" } else { "-dc" };
            let output = run(scratch.path(), applet, &[options, &missing, &filename], b"");
            let expected = if corrupt && format == "bzip2" { 2 } else { 1 };
            assert_eq!(output.status.code(), Some(expected), "{applet}: {output:?}");
            if file_alias {
                assert_eq!(fs::read(scratch.path().join("valid")).unwrap(), PAYLOAD);
                assert!(!scratch.path().join(&filename).exists());
            } else {
                assert_eq!(output.stdout, PAYLOAD, "{applet}: {output:?}");
                assert!(scratch.path().join(&filename).exists());
            }
            if stdout_alias {
                assert!(!scratch.path().join("valid").exists());
            }
            no_staging_files(scratch.path());
        }
    }
}

#[test]
fn bzip2_trailing_garbage_is_successful_and_file_lifecycle_is_completed() {
    for applet in ["bzip2", "bunzip2", "bzcat"] {
        for tail in [b"TRAILING".as_slice(), b"X".as_slice(), &[0; 8][..]] {
            let scratch = Scratch::new(applet);
            let mut compressed = fixture("bzip2", PAYLOAD);
            compressed.extend_from_slice(&fixture("bzip2", b"second member\n"));
            compressed.extend_from_slice(tail);
            fs::write(scratch.path().join("payload.bz2"), &compressed).unwrap();
            let output = run(scratch.path(), applet, &["-d", "payload.bz2"], b"");
            success(&output);
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("trailing garbage after EOF ignored")
            );
            let expected = [PAYLOAD, b"second member\n"].concat();
            if applet == "bzcat" {
                assert_eq!(output.stdout, expected);
                assert!(scratch.path().join("payload.bz2").exists());
            } else {
                assert_eq!(fs::read(scratch.path().join("payload")).unwrap(), expected);
                assert!(!scratch.path().join("payload.bz2").exists());
            }
            let quiet = run(scratch.path(), applet, &["-dcq"], &compressed);
            success(&quiet);
            assert!(quiet.stderr.is_empty(), "{quiet:?}");
            no_staging_files(scratch.path());
        }
    }
}

#[test]
fn forced_stdout_plaintext_passthrough_never_hides_recognized_corrupt_members() {
    for (applet, format, _, _) in DECODERS {
        let scratch = Scratch::new(applet);
        let plain = b"plain, not compressed\n";
        let output = run(scratch.path(), applet, &["-dcf"], plain);
        success(&output);
        assert_eq!(output.stdout, plain, "{applet}: {output:?}");
        let mut corrupted = fixture(format, PAYLOAD);
        if format == "bzip2" {
            corrupted[10] ^= 1;
        } else {
            *corrupted.last_mut().unwrap() ^= 1;
        }
        let output = run(scratch.path(), applet, &["-dcf"], &corrupted);
        assert!(!output.status.success(), "{applet}: {output:?}");
        assert_ne!(output.stdout, corrupted, "{applet}");
        let output = run(
            scratch.path(),
            applet,
            &["-dcf"],
            &fixture(format, PAYLOAD)[..3],
        );
        assert!(!output.status.success(), "{applet}: {output:?}");
        let mut concatenated = fixture(format, PAYLOAD);
        concatenated.extend_from_slice(&fixture(format, PAYLOAD)[..3]);
        let output = run(scratch.path(), applet, &["-dcf"], &concatenated);
        assert!(!output.status.success(), "{applet}: {output:?}");
    }
}

#[test]
fn xz_forced_stdin_passthrough_requires_a_requested_stdout_mode() {
    let scratch = Scratch::new("xz-forced-stdin");
    let plain = b"plain, not compressed\n";
    let compressed = fixture("xz", PAYLOAD);
    for (applet, args) in [
        ("xz", &["-df"][..]),
        ("unxz", &["-f"][..]),
        ("unxz", &["-f", "-"][..]),
    ] {
        let output = run(scratch.path(), applet, args, plain);
        assert_eq!(
            output.status.code(),
            Some(1),
            "{applet}, {args:?}: {output:?}"
        );
        assert!(output.stdout.is_empty(), "{applet}, {args:?}: {output:?}");
        let output = run(scratch.path(), applet, args, &compressed);
        success(&output);
        assert_eq!(output.stdout, PAYLOAD, "{applet}, {args:?}: {output:?}");
    }
    for (applet, args) in [
        ("xz", &["-dcf"][..]),
        ("xz", &["-df", "--stdout"][..]),
        ("unxz", &["-cf"][..]),
        ("unxz", &["-f", "--stdout"][..]),
        ("xzcat", &["-f"][..]),
    ] {
        let output = run(scratch.path(), applet, args, plain);
        success(&output);
        assert_eq!(output.stdout, plain, "{applet}, {args:?}: {output:?}");
    }
}

#[test]
fn zstd_integrity_tests_report_tested_bytes_unless_quiet() {
    let scratch = Scratch::new("zstd-test-count");
    let filename = "payload.zst";
    for payload in [PAYLOAD, b"".as_slice()] {
        let compressed = fixture("zstd", payload);
        fs::write(scratch.path().join(filename), &compressed).unwrap();
        stamp(&scratch.path().join(filename));
        for stdin in [true, false] {
            for (flags, quiet) in [
                (&["-t"][..], false),
                (&["--test"][..], false),
                (&["-ct"][..], false),
                (&["-tq"][..], true),
                (&["--test", "--quiet"][..], true),
            ] {
                let mut args = flags.to_vec();
                if !stdin {
                    args.push(filename);
                }
                let output = run(
                    scratch.path(),
                    "zstd",
                    &args,
                    if stdin { &compressed } else { b"" },
                );
                success(&output);
                assert!(output.stdout.is_empty(), "{stdin}, {flags:?}: {output:?}");
                let expected = if quiet {
                    String::new()
                } else {
                    format!(
                        "{:20}: {} bytes \n",
                        if stdin { "/*stdin*\\" } else { filename },
                        payload.len()
                    )
                };
                assert_eq!(output.stderr, expected.as_bytes(), "{stdin}, {flags:?}");
                assert_eq!(fs::read(scratch.path().join(filename)).unwrap(), compressed);
                private_metadata(&scratch.path().join(filename));
                no_staging_files(scratch.path());
            }
        }
    }
}

#[test]
fn canonical_link_and_suffix_guards_do_not_mutate_skipped_inputs() {
    for (format, suffix) in FORMATS {
        for symlink_input in [false, true] {
            let scratch = Scratch::new(format);
            let target = scratch.path().join("target");
            fs::write(&target, PAYLOAD).unwrap();
            let source = scratch.path().join("payload");
            if symlink_input {
                symlink("target", &source).unwrap();
            } else {
                fs::hard_link(&target, &source).unwrap();
            }
            let output = run(scratch.path(), format, &["payload"], b"");
            if format == "zstd" && !symlink_input {
                success(&output);
                assert_eq!(
                    decode(
                        format,
                        &fs::read(scratch.path().join("payload.zst")).unwrap()
                    ),
                    PAYLOAD
                );
            } else {
                let code = match (format, symlink_input) {
                    ("gzip", false) | ("xz", _) => 2,
                    _ => 1,
                };
                assert_eq!(output.status.code(), Some(code), "{format}: {output:?}");
                assert!(!scratch.path().join(format!("payload.{suffix}")).exists());
            }
            assert_eq!(fs::read(&source).unwrap(), PAYLOAD);
            assert_eq!(fs::read(target).unwrap(), PAYLOAD);
        }
        let scratch = Scratch::new(format);
        let filename = format!("already.{suffix}");
        let compressed = fixture(format, PAYLOAD);
        fs::write(scratch.path().join(&filename), &compressed).unwrap();
        let output = run(scratch.path(), format, &[&filename], b"");
        let code = match format {
            "gzip" | "zstd" => 0,
            "xz" => 2,
            _ => 1,
        };
        assert_eq!(output.status.code(), Some(code), "{format}: {output:?}");
        assert_eq!(
            fs::read(scratch.path().join(&filename)).unwrap(),
            compressed
        );
        assert_eq!(
            scratch.path().join(format!("{filename}.{suffix}")).exists(),
            format == "zstd"
        );
    }
}

#[test]
fn bzip2_unknown_suffix_and_xz_tar_suffix_map_to_canonical_destinations() {
    for (applet, format, source, destination) in [
        ("bzip2", "bzip2", "unknown.bin", "unknown.bin.out"),
        ("bunzip2", "bzip2", "unknown.bin", "unknown.bin.out"),
        ("xz", "xz", "archive.txz", "archive.tar"),
        ("unxz", "xz", "archive.txz", "archive.tar"),
    ] {
        let scratch = Scratch::new(applet);
        fs::write(scratch.path().join(source), fixture(format, PAYLOAD)).unwrap();
        stamp(&scratch.path().join(source));
        success(&run(scratch.path(), applet, &["-d", source], b""));
        assert_eq!(fs::read(scratch.path().join(destination)).unwrap(), PAYLOAD);
        private_metadata(&scratch.path().join(destination));
        assert!(!scratch.path().join(source).exists());
    }
}

#[test]
fn common_long_forms_and_gzip_levels_are_effective_not_ignored() {
    for (format, _) in FORMATS {
        let scratch = Scratch::new(format);
        let output = run(
            scratch.path(),
            format,
            &["--decompress", "--stdout"],
            &fixture(format, PAYLOAD),
        );
        success(&output);
        assert_eq!(output.stdout, PAYLOAD);
        let output = run(
            scratch.path(),
            format,
            &["--keep", "--force", "--stdout"],
            PAYLOAD,
        );
        success(&output);
        assert_eq!(decode(format, &output.stdout), PAYLOAD);
        let verbose = run(scratch.path(), format, &["-vc"], PAYLOAD);
        success(&verbose);
        assert!(!verbose.stderr.is_empty(), "{format}: {verbose:?}");
        let quiet = run(scratch.path(), format, &["-qc"], PAYLOAD);
        success(&quiet);
        assert!(quiet.stderr.is_empty(), "{format}: {quiet:?}");
    }
    let scratch = Scratch::new("gzip-levels");
    for (level, flags) in [("-1", 4), ("--fast", 4), ("-9", 2), ("--best", 2)] {
        let output = run(scratch.path(), "gzip", &[level, "-c"], PAYLOAD);
        success(&output);
        assert_eq!(decode("gzip", &output.stdout), PAYLOAD);
        assert_eq!(output.stdout[8], flags, "{level}");
    }
}

#[test]
fn xz_listing_reports_real_stream_block_check_counts_and_canonical_units() {
    let scratch = Scratch::new("xz-list");
    let payload = vec![b'x'; 12_000];
    let mut compressed = xz_fixture(&payload, CheckType::Crc32, true);
    compressed.extend_from_slice(&[0; 4]);
    compressed.extend_from_slice(&xz_fixture(PAYLOAD, CheckType::Crc64, false));
    compressed.extend_from_slice(&[0; 8]);
    fs::write(scratch.path().join("data.xz"), &compressed).unwrap();
    let output = run(scratch.path(), "xz", &["-l", "data.xz"], b"");
    success(&output);
    assert!(output.stderr.is_empty(), "{output:?}");
    let plain = payload.len() + PAYLOAD.len();
    let expected = format!(
        "Strms  Blocks   Compressed Uncompressed  Ratio  Check   Filename\n{:>5} {:>7}  {:>11}  {:>11}  {:>5}  {:<7} data.xz\n",
        2,
        4,
        format!("{} B", compressed.len()),
        "11.7 KiB",
        format!("{:.3}", compressed.len() as f64 / plain as f64),
        "CRC32,CRC64"
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    for (check, name) in [
        (CheckType::None, "None"),
        (CheckType::Crc32, "CRC32"),
        (CheckType::Sha256, "SHA-256"),
    ] {
        let compressed = xz_fixture(PAYLOAD, check, false);
        fs::write(scratch.path().join("single.xz"), &compressed).unwrap();
        let output = run(scratch.path(), "xz", &["-l", "single.xz"], b"");
        success(&output);
        let text = String::from_utf8(output.stdout).unwrap();
        let fields: Vec<_> = text.lines().nth(1).unwrap().split_whitespace().collect();
        assert_eq!(&fields[..2], ["1", "1"]);
        assert_eq!(fields[7], name, "{text}");
    }
    compressed.pop();
    fs::write(scratch.path().join("invalid-padding.xz"), compressed).unwrap();
    assert_eq!(
        run(scratch.path(), "xz", &["-l", "invalid-padding.xz"], b"")
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn unreadable_output_directory_cannot_replace_an_existing_destination() {
    let scratch = Scratch::new("directory-publication");
    let owner = fs::metadata(scratch.path()).unwrap();
    let drop_root = owner.uid() == 0;
    let uid = if drop_root { 65_534 } else { owner.uid() };
    let gid = if drop_root { 65_534 } else { owner.gid() };
    let executable = if drop_root {
        std::os::unix::fs::chown(scratch.path(), Some(uid), Some(gid)).unwrap();
        let executable = scratch.path().join("axe");
        let original = Path::new(env!("CARGO_BIN_EXE_axe"));
        let shared_executable = fs::metadata(original).unwrap().mode() & 0o005 == 0o005
            && fs::hard_link(original, &executable).is_ok();
        if !shared_executable {
            fs::copy(original, &executable).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        executable
    } else {
        PathBuf::from(env!("CARGO_BIN_EXE_axe"))
    };
    for (format, suffix) in FORMATS {
        let directory = scratch.path().join(format);
        fs::create_dir(&directory).unwrap();
        let source = directory.join("payload");
        let destination = directory.join(format!("payload.{suffix}"));
        fs::write(&source, PAYLOAD).unwrap();
        fs::write(&destination, SENTINEL).unwrap();
        stamp(&source);
        let inode = fs::metadata(&destination).unwrap().ino();
        if drop_root {
            for path in [&directory, &source, &destination] {
                std::os::unix::fs::chown(path, Some(uid), Some(gid)).unwrap();
            }
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o300)).unwrap();
        let mut command = Command::new(&executable);
        command
            .args(["--applet", format, "--", "-f", &format!("{format}/payload")])
            .current_dir(scratch.path())
            .env_clear()
            .env("HOME", scratch.path())
            .env("LC_ALL", "C")
            .env("PATH", "/nonexistent")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if drop_root {
            command.gid(gid).uid(uid);
        }
        let output = command
            .output()
            .expect("run without directory-read privilege");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(output.status.code(), Some(1), "{format}: {output:?}");
        assert_eq!(
            fs::read(&destination).unwrap(),
            SENTINEL,
            "{format}: {output:?}"
        );
        assert_eq!(fs::metadata(&destination).unwrap().ino(), inode, "{format}");
        assert_eq!(fs::read(&source).unwrap(), PAYLOAD, "{format}");
        no_staging_files(&directory);
    }
}

#[test]
fn integrity_test_cannot_be_redirected_to_an_encoder_by_a_later_compress_flag() {
    for (applet, format, _, _) in DECODERS {
        if format == "gzip" {
            continue;
        }
        let scratch = Scratch::new(applet);
        for stdin in [false, true] {
            for valid in [false, true] {
                let input = if valid {
                    fixture(format, PAYLOAD)
                } else {
                    PAYLOAD.to_vec()
                };
                for flags in [&["-tzv"][..], &["--test", "--compress", "--verbose"][..]] {
                    let mut args = flags.to_vec();
                    if !stdin {
                        fs::write(scratch.path().join("input"), &input).unwrap();
                        args.push("input");
                    }
                    let output = run(
                        scratch.path(),
                        applet,
                        &args,
                        if stdin { &input } else { b"" },
                    );
                    assert_eq!(
                        output.status.code(),
                        Some(if valid {
                            0
                        } else if format == "bzip2" {
                            2
                        } else {
                            1
                        }),
                        "{applet}, stdin={stdin}, valid={valid}, {flags:?}: {output:?}"
                    );
                    assert!(output.stdout.is_empty(), "{applet}: {output:?}");
                    if !stdin {
                        assert_eq!(fs::read(scratch.path().join("input")).unwrap(), input);
                    }
                    no_staging_files(scratch.path());
                }
            }
        }
    }
}

#[test]
fn a_closed_stdout_consumer_does_not_erase_earlier_operand_errors() {
    for (applet, format, suffix, _) in DECODERS {
        let scratch = Scratch::new(applet);
        let valid = format!("valid.{suffix}");
        fs::write(scratch.path().join(&valid), fixture(format, PAYLOAD)).unwrap();
        for corrupt in [false, true] {
            let first = if corrupt {
                let bad = format!("bad.{suffix}");
                fs::write(scratch.path().join(&bad), &fixture(format, PAYLOAD)[..3]).unwrap();
                bad
            } else {
                "missing".to_owned()
            };
            let (reader, writer) = nix::unistd::pipe().unwrap();
            drop(reader);
            let output = Command::new(env!("CARGO_BIN_EXE_axe"))
                .args([
                    "--applet",
                    applet,
                    "--",
                    "-dc",
                    &first,
                    &valid,
                    "not-reached",
                ])
                .current_dir(scratch.path())
                .env_clear()
                .env("HOME", scratch.path())
                .env("LC_ALL", "C")
                .env("PATH", "/nonexistent")
                .stdin(Stdio::null())
                .stdout(Stdio::from(writer))
                .stderr(Stdio::piped())
                .output()
                .expect("run with an already-closed stdout consumer");
            let expected = if corrupt && format == "bzip2" { 2 } else { 1 };
            assert_eq!(output.status.code(), Some(expected), "{applet}: {output:?}");
            assert!(
                !String::from_utf8_lossy(&output.stderr).contains("not-reached"),
                "{output:?}"
            );
            assert!(scratch.path().join(&valid).exists());
        }
    }
}

fn xz_index_fixture(records: &[(u64, u64)]) -> Vec<u8> {
    fn vli(output: &mut Vec<u8>, mut value: u64) {
        while value >= 0x80 {
            output.push((value as u8 & 0x7f) | 0x80);
            value >>= 7;
        }
        output.push(value as u8);
    }
    fn crc(bytes: &[u8]) -> [u8; 4] {
        let mut crc = flate2::Crc::new();
        crc.update(bytes);
        crc.sum().to_le_bytes()
    }
    let mut stream = b"\xfd7zXZ\0\0\0".to_vec();
    stream.extend_from_slice(&crc(&[0, 0]));
    let mut index = vec![0];
    vli(&mut index, records.len() as u64);
    for &(unpadded, plain) in records {
        stream.resize(stream.len() + unpadded.div_ceil(4) as usize * 4, 0);
        vli(&mut index, unpadded);
        vli(&mut index, plain);
    }
    while index.len() % 4 != 0 {
        index.push(0);
    }
    let checksum = crc(&index);
    index.extend_from_slice(&checksum);
    let backward = (index.len() as u32 / 4 - 1).to_le_bytes();
    stream.extend_from_slice(&index);
    let mut footer = backward.to_vec();
    footer.extend_from_slice(&[0, 0]);
    stream.extend_from_slice(&crc(&footer));
    stream.extend_from_slice(&footer);
    stream.extend_from_slice(b"YZ");
    stream
}

#[test]
fn xz_listing_rejects_crc_valid_but_structurally_impossible_indexes() {
    let scratch = Scratch::new("xz-impossible-index");
    for unpadded in 1..5 {
        let compressed = xz_index_fixture(&[(unpadded, 0)]);
        assert_eq!(compressed.len(), 36);
        fs::write(scratch.path().join("impossible.xz"), &compressed).unwrap();
        let output = run(scratch.path(), "xz", &["-l", "impossible.xz"], b"");
        assert_eq!(output.status.code(), Some(1), "{unpadded}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    let compressed = xz_index_fixture(&[(5, (1_u64 << 63) - 1), (5, 1)]);
    fs::write(scratch.path().join("oversized.xz"), compressed).unwrap();
    let output = run(scratch.path(), "xz", &["-l", "oversized.xz"], b"");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}
