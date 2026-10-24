#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tar::{Builder, EntryType, Header};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-tar-{}-{}",
            std::process::id(),
            NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create tar scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn extraction_root(&self) -> PathBuf {
        let root = self.path().join("extract");
        fs::create_dir(&root).expect("create extraction root");
        root
    }

    fn outside(&self) -> PathBuf {
        let outside = self.path().join("outside");
        fs::create_dir(&outside).expect("create outside directory");
        fs::write(outside.join("sentinel"), b"outside data").expect("write outside sentinel");
        outside
    }

    fn archive(&self, builder: Builder<Vec<u8>>) -> PathBuf {
        let path = self.path().join("input.tar");
        fs::write(&path, builder.into_inner().expect("finish archive")).expect("write archive");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy, Debug)]
enum Encoding {
    Header,
    Pax,
    Gnu,
}

fn header(kind: EntryType) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header
}

fn gnu_extension(builder: &mut Builder<Vec<u8>>, kind: EntryType, value: &str) {
    let mut header = header(kind);
    header
        .set_path("././@LongLink")
        .expect("set extension path");
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    builder
        .append(&header, bytes.as_slice())
        .expect("append GNU extension");
}

fn append(
    builder: &mut Builder<Vec<u8>>,
    encoding: Encoding,
    path: &str,
    target: Option<&str>,
    mut header: Header,
    data: &[u8],
) {
    header
        .set_path("placeholder")
        .expect("set placeholder path");
    if target.is_some() {
        header
            .set_link_name("placeholder")
            .expect("set placeholder link");
    }
    match encoding {
        Encoding::Header => {
            assert!(path.len() < 100);
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            if let Some(target) = target {
                assert!(target.len() < 100);
                header.as_mut_bytes()[157..257].fill(0);
                header.as_mut_bytes()[157..157 + target.len()].copy_from_slice(target.as_bytes());
            }
        }
        Encoding::Pax => {
            let mut extensions = vec![("path", path.as_bytes())];
            if let Some(target) = target {
                extensions.push(("linkpath", target.as_bytes()));
            }
            builder
                .append_pax_extensions(extensions)
                .expect("append PAX paths");
        }
        Encoding::Gnu => {
            gnu_extension(builder, EntryType::GNULongName, path);
            if let Some(target) = target {
                gnu_extension(builder, EntryType::GNULongLink, target);
            }
        }
    }
    header.set_size(data.len() as u64);
    header.set_cksum();
    builder
        .append(&header, data)
        .expect("append archive member");
}

fn member(
    builder: &mut Builder<Vec<u8>>,
    encoding: Encoding,
    path: &str,
    kind: EntryType,
    target: Option<&str>,
    data: &[u8],
) {
    append(builder, encoding, path, target, header(kind), data);
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    command
}

fn run_tar(root: &Path, args: &[&str]) -> Output {
    command(root)
        .args(["--applet", "tar", "--"])
        .args(args)
        .output()
        .expect("run bundled tar")
}

fn extract(root: &Path, archive: &Path, strip: usize) -> Output {
    command(root)
        .args(["--applet", "tar", "--", "-xf"])
        .arg(archive)
        .arg(format!("--strip-components={strip}"))
        .output()
        .expect("extract archive")
}

fn assert_status(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn parent_components_are_rejected_before_and_after_stripping() {
    for encoding in [Encoding::Header, Encoding::Pax, Encoding::Gnu] {
        for strip in [0, 1] {
            let scratch = Scratch::new();
            let root = scratch.extraction_root();
            let outside = scratch.outside();
            let path = if strip == 0 {
                "../outside/escaped".to_owned()
            } else {
                format!(
                    "top/../../{}/outside/escaped",
                    scratch
                        .path()
                        .file_name()
                        .expect("scratch basename")
                        .to_str()
                        .unwrap()
                )
            };
            let mut builder = Builder::new(Vec::new());
            member(
                &mut builder,
                encoding,
                &path,
                EntryType::Regular,
                None,
                b"escaped",
            );
            let archive = scratch.archive(builder);
            let output = extract(&root, &archive, strip);
            assert_status(&output, 2);
            assert!(
                !outside.join("escaped").exists(),
                "{encoding:?}, strip={strip}"
            );
            assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside data");
        }
    }
}

#[test]
fn absolute_member_paths_are_rooted_inside_the_destination() {
    for encoding in [Encoding::Pax, Encoding::Gnu] {
        for strip in [0, 1] {
            let scratch = Scratch::new();
            let root = scratch.extraction_root();
            let outside = scratch.outside();
            let absolute = outside.join("escaped");
            let mut builder = Builder::new(Vec::new());
            member(
                &mut builder,
                encoding,
                absolute.to_str().unwrap(),
                EntryType::Regular,
                None,
                b"inside data",
            );
            let archive = scratch.archive(builder);
            assert_status(&extract(&root, &archive, strip), 0);
            assert!(!absolute.exists(), "{encoding:?}, strip={strip}");
            let relative: PathBuf = absolute
                .components()
                .filter(|part| matches!(part, Component::Normal(_)))
                .skip(strip)
                .collect();
            assert_eq!(fs::read(root.join(relative)).unwrap(), b"inside data");
        }
    }
}

#[test]
fn archive_symlinks_cannot_redirect_files_or_deferred_directories_outside() {
    for encoding in [Encoding::Pax, Encoding::Gnu] {
        for strip in [0, 1] {
            for directory in [false, true] {
                for symlink_first in [false, true] {
                    let scratch = Scratch::new();
                    let root = scratch.extraction_root();
                    let outside = scratch.outside();
                    let prefix = if strip == 0 { "" } else { "top/" };
                    let mut builder = Builder::new(Vec::new());
                    for is_symlink in [symlink_first, !symlink_first] {
                        if is_symlink {
                            member(
                                &mut builder,
                                encoding,
                                &format!("{prefix}link"),
                                EntryType::Symlink,
                                Some(outside.to_str().unwrap()),
                                b"",
                            );
                        } else {
                            member(
                                &mut builder,
                                encoding,
                                &format!("{prefix}link/nested/escaped"),
                                if directory {
                                    EntryType::Directory
                                } else {
                                    EntryType::Regular
                                },
                                None,
                                if directory { b"" } else { b"escaped" },
                            );
                        }
                    }
                    let archive = scratch.archive(builder);
                    assert_status(&extract(&root, &archive, strip), 2);
                    assert!(
                        !outside.join("nested").exists(),
                        "{encoding:?}, strip={strip}, directory={directory}, symlink_first={symlink_first}"
                    );
                    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside data");
                }
            }
        }
    }
}

#[test]
fn preexisting_symlink_parents_are_checked_before_creating_children() {
    for encoding in [Encoding::Pax, Encoding::Gnu] {
        for strip in [0, 1] {
            for directory in [false, true] {
                let scratch = Scratch::new();
                let root = scratch.extraction_root();
                let outside = scratch.outside();
                symlink(&outside, root.join("link")).expect("create outside parent symlink");
                let prefix = if strip == 0 { "" } else { "top/" };
                let mut builder = Builder::new(Vec::new());
                member(
                    &mut builder,
                    encoding,
                    &format!("{prefix}link/new-parent/escaped"),
                    if directory {
                        EntryType::Directory
                    } else {
                        EntryType::Regular
                    },
                    None,
                    if directory { b"" } else { b"escaped" },
                );
                let archive = scratch.archive(builder);
                assert_status(&extract(&root, &archive, strip), 2);
                assert!(
                    !outside.join("new-parent").exists(),
                    "{encoding:?}, strip={strip}"
                );
                assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside data");
            }
        }
    }
}

#[test]
fn hardlink_targets_cannot_reach_host_files_through_absolute_parent_or_symlink_paths() {
    for encoding in [Encoding::Pax, Encoding::Gnu] {
        for strip in [0, 1] {
            for target_kind in ["absolute", "parent", "symlink"] {
                let scratch = Scratch::new();
                let root = scratch.extraction_root();
                let outside = scratch.outside();
                let sentinel = outside.join("sentinel");
                let initial_links = fs::metadata(&sentinel).unwrap().nlink();
                symlink(&outside, root.join("link")).expect("create hardlink source symlink");
                let prefix = if strip == 0 { "" } else { "top/" };
                let target = match target_kind {
                    "absolute" => sentinel.to_str().unwrap().to_owned(),
                    "parent" => "../outside/sentinel".to_owned(),
                    "symlink" => format!("{prefix}link/sentinel"),
                    _ => unreachable!(),
                };
                let mut builder = Builder::new(Vec::new());
                member(
                    &mut builder,
                    encoding,
                    &format!("{prefix}linked"),
                    EntryType::Link,
                    Some(&target),
                    b"",
                );
                let archive = scratch.archive(builder);
                assert_status(&extract(&root, &archive, strip), 2);
                assert!(
                    !root.join("linked").exists(),
                    "{encoding:?}, strip={strip}, {target_kind}"
                );
                assert_eq!(fs::metadata(&sentinel).unwrap().nlink(), initial_links);
                assert_eq!(fs::read(sentinel).unwrap(), b"outside data");
            }
        }
    }
}

#[test]
fn overwriting_a_final_symlink_replaces_it_without_modifying_its_target() {
    for strip in [0, 1] {
        let scratch = Scratch::new();
        let root = scratch.extraction_root();
        let outside = scratch.outside();
        symlink(outside.join("sentinel"), root.join("file")).expect("create final symlink");
        let mut builder = Builder::new(Vec::new());
        member(
            &mut builder,
            Encoding::Pax,
            if strip == 0 { "file" } else { "top/file" },
            EntryType::Regular,
            None,
            b"inside data",
        );
        let archive = scratch.archive(builder);
        assert_status(&extract(&root, &archive, strip), 0);
        assert!(
            !fs::symlink_metadata(root.join("file"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(fs::read(root.join("file")).unwrap(), b"inside data");
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside data");
    }
}

#[test]
fn internal_symlink_parents_and_hardlinks_survive_component_stripping() {
    for encoding in [Encoding::Pax, Encoding::Gnu] {
        for strip in [0, 1] {
            let scratch = Scratch::new();
            let root = scratch.extraction_root();
            let prefix = if strip == 0 { "" } else { "/./top/./" };
            let mut builder = Builder::new(Vec::new());
            let mut directory = header(EntryType::Directory);
            directory.set_mode(0o555);
            append(
                &mut builder,
                encoding,
                &format!("{prefix}real/"),
                None,
                directory,
                b"",
            );
            member(
                &mut builder,
                encoding,
                &format!("{prefix}alias"),
                EntryType::Symlink,
                Some("real"),
                b"",
            );
            member(
                &mut builder,
                encoding,
                &format!("{prefix}alias/data"),
                EntryType::Regular,
                None,
                b"payload",
            );
            member(
                &mut builder,
                encoding,
                &format!("{prefix}alias/subdir/"),
                EntryType::Directory,
                None,
                b"",
            );
            member(
                &mut builder,
                encoding,
                &format!("{prefix}hard"),
                EntryType::Link,
                Some(if strip == 0 {
                    "real/data"
                } else {
                    "top/real/data"
                }),
                b"",
            );
            let archive = scratch.archive(builder);
            let output = extract(&root, &archive, strip);
            let mode = fs::metadata(root.join("real")).map(|metadata| metadata.mode() & 0o7777);
            // Restore write access even when an assertion fails, so Scratch can clean up.
            let _ = fs::set_permissions(root.join("real"), fs::Permissions::from_mode(0o755));
            assert_status(&output, 0);
            assert_eq!(mode.unwrap(), 0o555);
            assert_eq!(
                fs::read_link(root.join("alias")).unwrap(),
                Path::new("real")
            );
            assert_eq!(fs::read(root.join("alias/data")).unwrap(), b"payload");
            assert!(root.join("real/subdir").is_dir());
            assert_eq!(
                fs::metadata(root.join("hard")).unwrap().ino(),
                fs::metadata(root.join("real/data")).unwrap().ino()
            );
        }
    }
}

#[test]
fn listing_ignores_strip_and_reports_every_unmatched_normalized_operand() {
    let scratch = Scratch::new();
    let mut builder = Builder::new(Vec::new());
    for (path, data) in [
        ("tree/", &b""[..]),
        ("tree/file", &b"payload"[..]),
        ("other", &b""[..]),
    ] {
        member(
            &mut builder,
            Encoding::Header,
            path,
            if path.ends_with('/') {
                EntryType::Directory
            } else {
                EntryType::Regular
            },
            None,
            data,
        );
    }
    let archive = scratch.archive(builder);
    let archive = archive.to_str().unwrap();
    let output = run_tar(scratch.path(), &["tf", archive, "--strip-components=999"]);
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"tree/\ntree/file\nother\n");

    let output = run_tar(scratch.path(), &["tf", archive, "/tree", "tree/file"]);
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"tree/\ntree/file\n");

    let output = run_tar(
        scratch.path(),
        &["tf", archive, "/tree", "/absent", "absent-too"],
    );
    assert_status(&output, 2);
    assert_eq!(output.stdout, b"tree/\ntree/file\n");
    assert_eq!(
        output.stderr,
        b"tar: absent: Not found in archive\ntar: absent-too: Not found in archive\ntar: Exiting with failure status due to previous errors\n"
    );

    let output = run_tar(scratch.path(), &["xOf", archive, "/tree/file"]);
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"payload");
    let output = run_tar(scratch.path(), &["xOf", archive, "absent"]);
    assert_status(&output, 2);
    assert!(
        output
            .stderr
            .starts_with(b"tar: absent: Not found in archive\n")
    );

    let output = run_tar(
        scratch.path(),
        &["tf", archive, "--exclude=tree/file", "/tree/file"],
    );
    assert_status(&output, 0);
    assert!(output.stdout.is_empty());
}

#[test]
fn fully_stripped_members_are_skipped_without_changing_the_root() {
    let scratch = Scratch::new();
    let root = scratch.extraction_root();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o750)).unwrap();
    let mut builder = Builder::new(Vec::new());
    let mut directory = header(EntryType::Directory);
    directory.set_mode(0o000);
    append(&mut builder, Encoding::Pax, "./top/", None, directory, b"");
    member(
        &mut builder,
        Encoding::Gnu,
        "top/file",
        EntryType::Regular,
        None,
        b"payload",
    );
    let archive = scratch.archive(builder);
    assert_status(&extract(&root, &archive, 1), 0);
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o750);
    assert_eq!(fs::read(root.join("file")).unwrap(), b"payload");
}

#[test]
fn old_style_argument_options_consume_words_independently_of_their_cluster_position() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.path().join("fixture")).unwrap();
    fs::write(scratch.path().join("fixture/data"), b"payload").unwrap();
    for args in [
        vec!["cfz", "bundle.tgz", "fixture"],
        vec!["cfCz", "with-C.tgz", "fixture", "data"],
        vec!["cCfz", "fixture", "C-first.tgz", "data"],
        vec!["-cfmodern.tar", "-Cfixture", "data"],
    ] {
        assert_status(&run_tar(scratch.path(), &args), 0);
    }
    assert!(!scratch.path().join("z").exists());
    let output = run_tar(scratch.path(), &["tfv", "bundle.tgz"]);
    assert_status(&output, 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains("fixture/data\n"));
    for archive in ["with-C.tgz", "C-first.tgz", "modern.tar"] {
        let output = run_tar(scratch.path(), &["tf", archive]);
        assert_status(&output, 0);
        assert_eq!(output.stdout, b"data\n");
    }
}

#[test]
fn option_errors_distinguish_unknown_options_and_missing_or_unexpected_arguments() {
    let scratch = Scratch::new();
    for (args, message) in [
        (vec!["--wildcards"], "unrecognized option '--wildcards'"),
        (vec!["-cq"], "invalid option -- 'q'"),
        (vec!["cq"], "invalid option -- 'q'"),
        (vec!["-cf"], "option requires an argument -- 'f'"),
        (vec!["cfz"], "Old option 'f' requires an argument."),
        (vec!["cC"], "Old option 'C' requires an argument."),
        (vec!["-cC"], "option requires an argument -- 'C'"),
        (
            vec!["--create", "--file"],
            "option '--file' requires an argument",
        ),
        (
            vec!["--list", "--strip-components"],
            "option '--strip-components' requires an argument",
        ),
        (
            vec!["--list=foo"],
            "option '--list' doesn't allow an argument",
        ),
        (
            vec!["--help=foo"],
            "option '--help' doesn't allow an argument",
        ),
        (
            vec!["--list", "--strip-components=-1"],
            "invalid strip-components count '-1'",
        ),
    ] {
        let output = run_tar(scratch.path(), &args);
        assert_status(&output, 2);
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            output.stderr,
            format!("tar: {message}\n").as_bytes(),
            "{args:?}"
        );
    }
}

#[test]
fn verbose_listing_grows_owner_size_width_and_formats_links_devices_and_escapes() {
    let scratch = Scratch::new();
    let mut builder = Builder::new(Vec::new());
    let mut first = header(EntryType::Regular);
    first.set_username("alice").unwrap();
    first.set_groupname("staff").unwrap();
    first.set_mode(0o4755);
    append(&mut builder, Encoding::Header, "first", None, first, b"x");
    let mut second = header(EntryType::Regular);
    second.set_username("abcdefghijklmnopqrst").unwrap();
    second.set_groupname("staff").unwrap();
    second.set_mode(0o2640);
    append(
        &mut builder,
        Encoding::Header,
        "second",
        None,
        second,
        b"xx",
    );
    member(
        &mut builder,
        Encoding::Header,
        "hard",
        EntryType::Link,
        Some("first"),
        b"",
    );
    let mut link = header(EntryType::Symlink);
    link.set_mode(0o777);
    append(
        &mut builder,
        Encoding::Gnu,
        "space name\n\\\t\x01",
        Some("first\n"),
        link,
        b"",
    );
    for (kind, name, major, minor, mode) in [
        (EntryType::Char, "char", 4, 64, 0o600),
        (EntryType::Block, "block", 8, 1, 0o660),
    ] {
        let mut device = header(kind);
        device.set_mode(mode);
        device.set_device_major(major).unwrap();
        device.set_device_minor(minor).unwrap();
        append(&mut builder, Encoding::Header, name, None, device, b"");
    }
    let archive = scratch.archive(builder);
    let output = run_tar(scratch.path(), &["tvf", archive.to_str().unwrap()]);
    assert_status(&output, 0);
    let expected = format!(
        "-rwsr-xr-x alice/staff       1 1970-01-01 00:00 first\n\
         -rw-r-S--- abcdefghijklmnopqrst/staff 2 1970-01-01 00:00 second\n\
         hrw-r--r-- 0/0 {:>24} 1970-01-01 00:00 hard link to first\n\
         lrwxrwxrwx 0/0 {:>24} 1970-01-01 00:00 space name\\n\\\\\\t\\001 -> first\\n\n\
         crw------- 0/0 {:>24} 1970-01-01 00:00 char\n\
         brw-rw---- 0/0 {:>24} 1970-01-01 00:00 block\n",
        "0", "0", "4,64", "8,1"
    );
    assert_eq!(output.stdout, expected.as_bytes());
    let output = run_tar(
        scratch.path(),
        &["tf", archive.to_str().unwrap(), "space name\n\\\t\x01"],
    );
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"space name\\n\\\\\\t\\001\n");
}

#[test]
fn verbose_permissions_show_executing_and_nonexecuting_setid_and_sticky_bits() {
    let scratch = Scratch::new();
    let mut builder = Builder::new(Vec::new());
    let cases = [
        (0o4755, "-rwsr-xr-x"),
        (0o4644, "-rwSr--r--"),
        (0o2755, "-rwxr-sr-x"),
        (0o2644, "-rw-r-Sr--"),
        (0o1755, "-rwxr-xr-t"),
        (0o1644, "-rw-r--r-T"),
    ];
    for (index, (mode, _)) in cases.iter().enumerate() {
        let mut entry = header(EntryType::Regular);
        entry.set_mode(*mode);
        append(
            &mut builder,
            Encoding::Header,
            &format!("entry-{index}"),
            None,
            entry,
            b"",
        );
    }
    let archive = scratch.archive(builder);
    let output = run_tar(scratch.path(), &["tvf", archive.to_str().unwrap()]);
    assert_status(&output, 0);
    let output = String::from_utf8(output.stdout).unwrap();
    let modes: Vec<&str> = output
        .lines()
        .map(|line| line.split_once(' ').unwrap().0)
        .collect();
    assert_eq!(modes, cases.map(|(_, mode)| mode));
}

#[test]
fn listing_escapes_non_utf8_name_bytes_without_lossy_replacement() {
    let scratch = Scratch::new();
    let mut builder = Builder::new(Vec::new());
    let mut header = header(EntryType::Regular);
    header.as_mut_bytes()[..4].copy_from_slice(b"bad\xff");
    header.set_size(0);
    header.set_cksum();
    builder.append(&header, &b""[..]).unwrap();
    let archive = scratch.archive(builder);
    let output = run_tar(scratch.path(), &["tf", archive.to_str().unwrap()]);
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"bad\\377\n");
}

#[test]
fn listing_preserves_completed_records_when_a_later_header_is_truncated() {
    let scratch = Scratch::new();
    let mut builder = Builder::new(Vec::new());
    member(
        &mut builder,
        Encoding::Header,
        "first",
        EntryType::Regular,
        None,
        b"",
    );
    let mut bytes = builder.into_inner().unwrap();
    bytes.truncate(512);
    bytes.extend_from_slice(&[b'x'; 256]);
    let archive = scratch.path().join("truncated.tar");
    fs::write(&archive, bytes).unwrap();
    let output = run_tar(scratch.path(), &["tf", archive.to_str().unwrap()]);
    assert_status(&output, 2);
    assert_eq!(output.stdout, b"first\n");
}

#[test]
fn listing_emits_the_first_record_before_waiting_for_the_rest_of_the_archive() {
    let scratch = Scratch::new();
    let mut builder = Builder::new(Vec::new());
    member(
        &mut builder,
        Encoding::Header,
        "first",
        EntryType::Regular,
        None,
        b"",
    );
    let bytes = builder.into_inner().unwrap();
    let mut child = command(scratch.path())
        .args(["--applet", "tar", "--", "-tf", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start streaming tar");
    let mut input = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut line = Vec::new();
        let result = BufReader::new(stdout)
            .read_until(b'\n', &mut line)
            .map(|_| line);
        let _ = sender.send(result);
    });
    input
        .write_all(&bytes[..512])
        .expect("write first archive header");
    let first = receiver.recv_timeout(Duration::from_secs(10));
    if first.is_err() {
        let _ = child.kill();
    } else {
        let _ = input.write_all(&[0; 1024]);
    }
    drop(input);
    let output = child.wait_with_output().expect("wait for streaming tar");
    reader.join().expect("join stdout reader");
    assert_eq!(
        first
            .expect("first listing record must not wait for EOF")
            .unwrap(),
        b"first\n"
    );
    assert_status(&output, 0);
}

#[test]
fn bundled_shell_can_create_list_and_strip_extract_without_a_host_path() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.path().join("source")).unwrap();
    fs::write(scratch.path().join("source/data"), b"payload\n").unwrap();
    let output = command(scratch.path())
        .args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "tar cfz bundle.tgz source && tar tfv bundle.tgz >/dev/null && mkdir result && tar xf bundle.tgz --strip-components=1 -C result && cat result/data",
        ])
        .output()
        .expect("run bundled tar shell scenario");
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"payload\n");
}
