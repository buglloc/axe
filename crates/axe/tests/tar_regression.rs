#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tar::{Builder, EntryType, Header};

fn accessible_temp_dir() -> PathBuf {
    let candidate =
        std::fs::canonicalize(std::env::temp_dir()).unwrap_or_else(|_| std::env::temp_dir());
    let mut current = candidate.as_path();
    loop {
        if fs::metadata(current)
            .map(|metadata| metadata.mode() & 0o111 == 0)
            .unwrap_or(true)
        {
            return PathBuf::from("/dev/shm");
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent;
    }
    candidate
}

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = accessible_temp_dir().join(format!(
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

fn command_with_executable(root: &Path, executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("PATH", "/nonexistent")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    command
}

struct PublicExecutable {
    _scratch: Scratch,
    path: PathBuf,
}

impl PublicExecutable {
    fn new() -> Self {
        let scratch = Scratch::new();
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755))
            .expect("public scratch directory mode");
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).expect("create public binary directory");
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755))
            .expect("public directory mode");
        let path = bin.join("axe");
        fs::copy(env!("CARGO_BIN_EXE_axe"), &path).expect("copy bundled binary");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("public binary mode");
        Self {
            _scratch: scratch,
            path,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn probe(&self) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            let mut command = Command::new(self.path());
            command
                .args(["--applet", "tar", "--", "--version"])
                .current_dir("/")
                .env_clear()
                .env("HOME", "/dev/shm")
                .env("PATH", "/nonexistent")
                .env("LC_ALL", "C")
                .env("TZ", "UTC")
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command.gid(65534).uid(65534);
            match command.output() {
                Ok(output) if output.status.success() => return true,
                Ok(output) if output.status.code() == Some(127) => {
                    eprintln!(
                        "SKIP: dropped uid cannot load copied AXE binary (loader exited 127)"
                    );
                    return false;
                }
                Ok(output) => panic!("probe copied AXE binary exited with {}", output.status),
                Err(error)
                    if error.raw_os_error() == Some(libc::ETXTBSY)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    eprintln!("SKIP: dropped uid cannot execute copied AXE binary: {error}");
                    return false;
                }
                Err(error) => panic!("probe copied AXE binary: {error}"),
            }
        }
    }
}

fn command(root: &Path) -> Command {
    command_with_executable(root, Path::new(env!("CARGO_BIN_EXE_axe")))
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
            directory.set_mtime(1_700_000_000);
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
            let mtime = fs::metadata(root.join("real")).map(|metadata| metadata.mtime());
            // Restore write access even when an assertion fails, so Scratch can clean up.
            let _ = fs::set_permissions(root.join("real"), fs::Permissions::from_mode(0o755));
            assert_status(&output, 0);
            assert_eq!(mode.unwrap(), 0o555);
            assert_eq!(mtime.unwrap(), 1_700_000_000);
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
    assert!(!output.stderr.is_empty());

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
fn files_from_supports_c_directives_null_names_and_positional_members() {
    let scratch = Scratch::new();
    let a = scratch.path().join("a");
    let b = scratch.path().join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    fs::write(a.join("with space"), b"space").unwrap();
    let odd = std::ffi::OsString::from_vec(b"odd\xff".to_vec());
    fs::write(b.join(&odd), b"odd").unwrap();
    fs::write(scratch.path().join("root"), b"root").unwrap();

    let list = scratch.path().join("list");
    let mut list_bytes =
        format!("-C {}\nwith space\n-C {}\n", a.display(), b.display()).into_bytes();
    list_bytes.extend_from_slice(b"odd\xff\n");
    fs::write(&list, list_bytes).unwrap();
    let output = run_tar(scratch.path(), &["-cf", "bundle.tar", "root", "-T", "list"]);
    assert_status(&output, 0);
    let mut archive = tar::Archive::new(fs::File::open(scratch.path().join("bundle.tar")).unwrap());
    let names: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path_bytes().to_vec())
        .collect();
    assert_eq!(
        names,
        vec![
            b"root".to_vec(),
            b"with space".to_vec(),
            b"odd\xff".to_vec()
        ]
    );

    let null_list = scratch.path().join("null-list");
    fs::write(&null_list, b"a/with space\0b/odd\xff\0").unwrap();
    let output = run_tar(
        scratch.path(),
        &["--null", "-cf", "null.tar", "-T", "null-list"],
    );
    assert_status(&output, 0);
    let mut archive = tar::Archive::new(fs::File::open(scratch.path().join("null.tar")).unwrap());
    let names: Vec<_> = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path_bytes().to_vec())
        .collect();
    assert_eq!(names, vec![b"a/with space".to_vec(), b"b/odd\xff".to_vec()]);
}

#[test]
fn option_errors_distinguish_unknown_options_and_missing_or_unexpected_arguments() {
    let scratch = Scratch::new();
    for (args, _message) in [
        (vec!["--format=pax"], "unrecognized option '--format=pax'"),
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
        assert!(!output.stderr.is_empty(), "{args:?}");
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

fn archive_input(path: &Path, compression: Option<&str>) -> Box<dyn Read> {
    let input = fs::File::open(path).unwrap();
    match compression {
        Some("-z") => Box::new(flate2::read::MultiGzDecoder::new(input)),
        Some("-j") => Box::new(bzip2::read::MultiBzDecoder::new(input)),
        Some("-J") => Box::new(lzma_rust2::XzReader::new(input, true)),
        Some("--zstd") => Box::new(zstd::stream::read::Decoder::new(input).unwrap()),
        None => Box::new(input),
        Some(option) => panic!("unexpected test compression {option}"),
    }
}

#[test]
fn creation_preserves_hardlink_groups_in_every_compression_mode() {
    for compression in [None, Some("-z"), Some("-j"), Some("-J"), Some("--zstd")] {
        let scratch = Scratch::new();
        fs::create_dir(scratch.path().join("tree")).unwrap();
        fs::write(scratch.path().join("tree/plain"), b"hardlink payload\n").unwrap();
        fs::hard_link(
            scratch.path().join("tree/plain"),
            scratch.path().join("tree/hard"),
        )
        .unwrap();
        let mut args = vec!["-cf", "bundle.tar"];
        args.extend(compression);
        args.extend(["tree/plain", "tree/hard"]);
        assert_status(&run_tar(scratch.path(), &args), 0);
        let mut archive = tar::Archive::new(archive_input(
            &scratch.path().join("bundle.tar"),
            compression,
        ));
        let mut entries = archive.entries().unwrap();
        let mut plain = entries.next().unwrap().unwrap();
        assert_eq!(plain.path().unwrap().as_ref(), Path::new("tree/plain"));
        assert!(plain.header().entry_type().is_file());
        let mut data = Vec::new();
        plain.read_to_end(&mut data).unwrap();
        assert_eq!(data, b"hardlink payload\n");
        let hard = entries.next().unwrap().unwrap();
        assert_eq!(hard.path().unwrap().as_ref(), Path::new("tree/hard"));
        assert!(hard.header().entry_type().is_hard_link());
        assert_eq!(hard.size(), 0);
        assert_eq!(
            hard.link_name().unwrap().unwrap().as_ref(),
            Path::new("tree/plain")
        );
        assert!(entries.next().is_none());
    }
}

#[test]
fn directory_changes_are_positional_for_creation_and_selected_extraction() {
    let scratch = Scratch::new();
    for directory in ["one", "two", "out-one", "out-two"] {
        fs::create_dir(scratch.path().join(directory)).unwrap();
    }
    fs::write(scratch.path().join("one/first"), b"one\n").unwrap();
    fs::write(scratch.path().join("two/second"), b"two\n").unwrap();
    fs::hard_link(
        scratch.path().join("one/first"),
        scratch.path().join("two/alias"),
    )
    .unwrap();
    let output = run_tar(
        scratch.path(),
        &[
            "-cf",
            "bundle.tar",
            "-C",
            "one",
            "first",
            "--directory=../two",
            "second",
            "alias",
        ],
    );
    assert_status(&output, 0);
    assert!(scratch.path().join("bundle.tar").is_file());
    let output = run_tar(
        scratch.path(),
        &[
            "-xf",
            "bundle.tar",
            "-C",
            "out-one",
            "first",
            "alias",
            "--directory",
            "../out-two",
            "second",
        ],
    );
    assert_status(&output, 0);
    assert_eq!(
        fs::read(scratch.path().join("out-one/first")).unwrap(),
        b"one\n"
    );
    assert_eq!(
        fs::read(scratch.path().join("out-two/second")).unwrap(),
        b"two\n"
    );
    assert_eq!(
        fs::metadata(scratch.path().join("out-one/first"))
            .unwrap()
            .ino(),
        fs::metadata(scratch.path().join("out-one/alias"))
            .unwrap()
            .ino(),
    );
    assert!(!scratch.path().join("out-one/second").exists());
    assert!(!scratch.path().join("out-two/first").exists());
}

#[test]
fn basename_and_glob_exclusions_apply_to_nested_members_and_directory_descendants() {
    for pattern in ["drop", "*/drop", "[d]rop", "d?op"] {
        let scratch = Scratch::new();
        fs::create_dir_all(scratch.path().join("tree/nested")).unwrap();
        fs::create_dir(scratch.path().join("tree/drop")).unwrap();
        for (path, data) in [
            ("tree/keep", &b"kept"[..]),
            ("tree/nested/drop", &b"excluded file"[..]),
            ("tree/drop/child", &b"excluded descendant"[..]),
        ] {
            fs::write(scratch.path().join(path), data).unwrap();
        }
        let exclude = format!("--exclude={pattern}");
        let output = run_tar(scratch.path(), &["-cf", "created.tar", &exclude, "tree"]);
        assert_status(&output, 0);
        let listing = run_tar(scratch.path(), &["-tf", "created.tar"]);
        assert_status(&listing, 0);
        let listing = String::from_utf8(listing.stdout).unwrap();
        assert!(listing.lines().any(|line| line == "tree/keep"));
        assert!(!listing.contains("drop"), "pattern {pattern}: {listing}");

        let mut builder = Builder::new(Vec::new());
        for path in [
            "tree/keep",
            "tree/nested/drop",
            "tree/drop/",
            "tree/drop/child",
        ] {
            let kind = if path.ends_with('/') {
                EntryType::Directory
            } else {
                EntryType::Regular
            };
            let mut metadata = header(kind);
            metadata.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
            append(
                &mut builder,
                Encoding::Pax,
                path,
                None,
                metadata,
                if kind.is_dir() { b"" } else { b"payload" },
            );
        }
        let archive = scratch.archive(builder);
        let root = scratch.extraction_root();
        let output = run_tar(&root, &["-xf", archive.to_str().unwrap(), &exclude]);
        assert_status(&output, 0);
        assert_eq!(fs::read(root.join("tree/keep")).unwrap(), b"payload");
        assert!(!root.join("tree/nested/drop").exists(), "pattern {pattern}");
        assert!(!root.join("tree/drop").exists(), "pattern {pattern}");
    }
}

#[test]
fn hardlinks_replace_existing_files_and_symlinks_unless_keep_old_is_requested() {
    for strip in [0, 1] {
        for keep_old in [false, true] {
            for existing_symlink in [false, true] {
                let scratch = Scratch::new();
                let root = scratch.extraction_root();
                let outside = scratch.outside();
                if existing_symlink {
                    symlink(outside.join("sentinel"), root.join("hard")).unwrap();
                } else {
                    fs::write(root.join("hard"), b"existing data").unwrap();
                }
                let prefix = if strip == 0 { "" } else { "top/" };
                let mut builder = Builder::new(Vec::new());
                member(
                    &mut builder,
                    Encoding::Pax,
                    &format!("{prefix}data"),
                    EntryType::Regular,
                    None,
                    b"new data",
                );
                member(
                    &mut builder,
                    Encoding::Pax,
                    &format!("{prefix}hard"),
                    EntryType::Link,
                    Some(&format!("{prefix}data")),
                    b"",
                );
                let archive = scratch.archive(builder);
                let mut args = vec!["-xf", archive.to_str().unwrap()];
                let strip_option = format!("--strip-components={strip}");
                args.push(&strip_option);
                if keep_old {
                    args.push("--keep-old-files");
                }
                let output = run_tar(&root, &args);
                assert_status(&output, if keep_old { 2 } else { 0 });
                assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside data");
                if keep_old {
                    assert_eq!(
                        fs::read(root.join("hard")).unwrap(),
                        if existing_symlink {
                            &b"outside data"[..]
                        } else {
                            &b"existing data"[..]
                        },
                    );
                    assert_ne!(
                        fs::metadata(root.join("hard")).unwrap().ino(),
                        fs::metadata(root.join("data")).unwrap().ino(),
                    );
                } else {
                    assert!(
                        !fs::symlink_metadata(root.join("hard"))
                            .unwrap()
                            .is_symlink()
                    );
                    assert_eq!(fs::read(root.join("hard")).unwrap(), b"new data");
                    assert_eq!(
                        fs::metadata(root.join("hard")).unwrap().ino(),
                        fs::metadata(root.join("data")).unwrap().ino(),
                    );
                }
            }
        }
    }
}

#[test]
fn keep_old_collisions_do_not_skip_later_entries_or_deferred_directory_metadata() {
    for strip in [0, 1] {
        let scratch = Scratch::new();
        let root = scratch.extraction_root();
        fs::create_dir(root.join("tree")).unwrap();
        fs::set_permissions(root.join("tree"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("tree/plain"), b"existing\n").unwrap();
        let prefix = if strip == 0 { "" } else { "top/" };
        let mut builder = Builder::new(Vec::new());
        let mut directory = header(EntryType::Directory);
        directory.set_mode(0o555);
        directory.set_mtime(1_700_000_000);
        append(
            &mut builder,
            Encoding::Pax,
            &format!("{prefix}tree/"),
            None,
            directory,
            b"",
        );
        for (path, kind, target, data) in [
            ("tree/plain", EntryType::Regular, None, &b"new\n"[..]),
            ("tree/empty", EntryType::Regular, None, &b""[..]),
            ("tree/hard", EntryType::Link, Some("tree/plain"), &b""[..]),
            ("tree/sym", EntryType::Symlink, Some("plain"), &b""[..]),
        ] {
            let target = target.map(|target| {
                if kind.is_hard_link() {
                    format!("{prefix}{target}")
                } else {
                    target.to_owned()
                }
            });
            member(
                &mut builder,
                Encoding::Pax,
                &format!("{prefix}{path}"),
                kind,
                target.as_deref(),
                data,
            );
        }
        let mut directory = header(EntryType::Directory);
        directory.set_mode(0o555);
        directory.set_mtime(1_700_000_123);
        append(
            &mut builder,
            Encoding::Pax,
            &format!("{prefix}fresh/"),
            None,
            directory,
            b"",
        );
        member(
            &mut builder,
            Encoding::Pax,
            &format!("{prefix}fresh/data"),
            EntryType::Regular,
            None,
            b"later payload",
        );
        let archive = scratch.archive(builder);
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let output = run_tar(
            &root,
            &[
                "--keep-old-files",
                "-xf",
                archive.to_str().unwrap(),
                &format!("--strip-components={strip}"),
            ],
        );
        let directory = fs::metadata(root.join("tree")).unwrap();
        let fresh = fs::metadata(root.join("fresh")).unwrap();
        fs::set_permissions(root.join("fresh"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_status(&output, 2);
        assert_eq!(output.stdout, b"");
        assert!(!output.stderr.is_empty());
        assert_eq!(fs::read(root.join("tree/plain")).unwrap(), b"existing\n");
        assert_eq!(fs::read(root.join("tree/empty")).unwrap(), b"");
        assert_eq!(fs::read(root.join("tree/hard")).unwrap(), b"existing\n");
        assert_eq!(
            fs::metadata(root.join("tree/plain")).unwrap().ino(),
            fs::metadata(root.join("tree/hard")).unwrap().ino(),
        );
        assert_eq!(
            fs::read_link(root.join("tree/sym")).unwrap(),
            Path::new("plain")
        );
        assert_eq!(directory.mode() & 0o7777, 0o700);
        assert!(directory.mtime() >= started);
        assert_ne!(directory.mtime(), 1_700_000_000);
        assert_eq!(fs::read(root.join("fresh/data")).unwrap(), b"later payload");
        assert_eq!(fresh.mode() & 0o7777, 0o555);
        assert_eq!(fresh.mtime(), 1_700_000_123);
    }
}

#[test]
fn extraction_permissions_follow_umask_unless_preserved_including_root_defaults() {
    // SAFETY: geteuid has no arguments or memory preconditions.
    let root_user = unsafe { libc::geteuid() == 0 };
    let dropped_executable = if root_user {
        let executable = PublicExecutable::new();
        if !executable.probe() {
            return;
        }
        Some(executable)
    } else {
        None
    };
    for nonroot_child in [false, true] {
        if nonroot_child && !root_user {
            continue;
        }
        for permission_option in [None, Some("--no-same-permissions"), Some("-p")] {
            let scratch = Scratch::new();
            let root = scratch.extraction_root();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let mut builder = Builder::new(Vec::new());
            for (path, kind, mode) in [
                ("tree/", EntryType::Directory, 0o1777),
                ("tree/open", EntryType::Regular, 0o777),
                ("tree/special", EntryType::Regular, 0o6751),
            ] {
                let mut metadata = header(kind);
                metadata.set_mode(mode);
                append(&mut builder, Encoding::Pax, path, None, metadata, b"");
            }
            let archive = scratch.archive(builder);
            fs::set_permissions(&archive, fs::Permissions::from_mode(0o644)).unwrap();
            if nonroot_child {
                fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755)).unwrap();
                chown(&root, Some(65534), Some(65534)).unwrap();
            }
            let executable: &Path = if nonroot_child {
                dropped_executable
                    .as_ref()
                    .expect("probed dropped executable")
                    .path()
            } else {
                Path::new(env!("CARGO_BIN_EXE_axe"))
            };
            let mut child = command_with_executable(&root, executable);
            child.args([
                "--no-config",
                "--norc",
                "--noprofile",
                "-c",
                "umask 022; tar \"$@\"",
                "tar-permission-test",
                "-xf",
            ]);
            child.arg(&archive);
            if let Some(option) = permission_option {
                child.arg(option);
            }
            if nonroot_child {
                child.gid(65534).uid(65534);
            }
            let output = child.output().expect("extract with child-local umask");
            assert_status(&output, 0);
            let preserve = permission_option == Some("-p")
                || (permission_option.is_none() && root_user && !nonroot_child);
            for (path, expected) in [
                ("tree", if preserve { 0o1777 } else { 0o755 }),
                ("tree/open", if preserve { 0o777 } else { 0o755 }),
                ("tree/special", if preserve { 0o6751 } else { 0o751 }),
            ] {
                assert_eq!(
                    fs::metadata(root.join(path)).unwrap().mode() & 0o7777,
                    expected,
                    "nonroot={nonroot_child} option={permission_option:?} path={path}",
                );
            }
        }
    }
}

#[test]
fn m_and_touch_skip_mtime_for_files_and_deferred_directories() {
    for option in [None, Some("-m"), Some("--touch"), Some("--no-mtime")] {
        let scratch = Scratch::new();
        let root = scratch.extraction_root();
        let mut builder = Builder::new(Vec::new());
        for (path, kind) in [
            ("tree/", EntryType::Directory),
            ("tree/data", EntryType::Regular),
        ] {
            let mut metadata = header(kind);
            if kind.is_dir() {
                metadata.set_mode(0o755);
            }
            metadata.set_mtime(1_700_000_000);
            append(&mut builder, Encoding::Pax, path, None, metadata, b"");
        }
        let archive = scratch.archive(builder);
        let mut args = vec!["-xf", archive.to_str().unwrap()];
        args.extend(option);
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let output = run_tar(&root, &args);
        assert_status(&output, 0);
        for path in ["tree", "tree/data"] {
            let mtime = fs::metadata(root.join(path)).unwrap().mtime();
            if option.is_some() {
                assert!(mtime >= started, "{option:?} {path}: {mtime} < {started}");
            } else {
                assert_eq!(mtime, 1_700_000_000);
            }
        }
    }
}

#[test]
fn auto_compression_and_zstd_detection_preserve_archive_contents() {
    for (name, magic) in [
        ("bundle.tar", None),
        ("bundle.tar.gz", Some(&b"\x1f\x8b"[..])),
        ("bundle.tgz", Some(&b"\x1f\x8b"[..])),
        ("bundle.tar.bz2", Some(&b"BZh"[..])),
        ("bundle.tbz2", Some(&b"BZh"[..])),
        ("bundle.txz", Some(&b"\xfd7zXZ\0"[..])),
        ("bundle.tar.zst", Some(&b"\x28\xb5\x2f\xfd"[..])),
        ("bundle.tzst", Some(&b"\x28\xb5\x2f\xfd"[..])),
    ] {
        let scratch = Scratch::new();
        fs::write(scratch.path().join("data"), b"auto-compressed\n").unwrap();
        assert_status(&run_tar(scratch.path(), &["-acf", name, "data"]), 0);
        let bytes = fs::read(scratch.path().join(name)).unwrap();
        if let Some(magic) = magic {
            assert!(bytes.starts_with(magic), "{name}");
        }
        let output = run_tar(scratch.path(), &["-xOf", name, "data"]);
        assert_status(&output, 0);
        assert_eq!(output.stdout, b"auto-compressed\n");
    }
}

#[test]
fn dereference_creates_regular_files_and_directory_contents_but_rejects_cycles() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.path().join("real")).unwrap();
    fs::write(scratch.path().join("real/data"), b"dereferenced\n").unwrap();
    symlink("real/data", scratch.path().join("file-link")).unwrap();
    symlink("real", scratch.path().join("dir-link")).unwrap();
    assert_status(
        &run_tar(
            scratch.path(),
            &["-hcf", "bundle.tar", "file-link", "dir-link"],
        ),
        0,
    );
    let mut archive = tar::Archive::new(fs::File::open(scratch.path().join("bundle.tar")).unwrap());
    let mut files = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        assert!(!entry.header().entry_type().is_symlink());
        if entry.header().entry_type().is_file() {
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            files.push((entry.path().unwrap().into_owned(), data));
        }
    }
    assert_eq!(
        files,
        [
            (PathBuf::from("file-link"), b"dereferenced\n".to_vec()),
            (PathBuf::from("dir-link/data"), b"dereferenced\n".to_vec()),
        ],
    );
    symlink(".", scratch.path().join("real/loop")).unwrap();
    assert_status(&run_tar(scratch.path(), &["-hcf", "cycle.tar", "real"]), 2);
}

#[test]
fn wildcard_selection_supports_ranges_and_can_be_disabled_for_literal_names() {
    let scratch = Scratch::new();
    let mut builder = Builder::new(Vec::new());
    for path in [
        "tree/nested/script",
        "tree/nested/drop",
        "star*name",
        "elsewhere",
    ] {
        member(
            &mut builder,
            Encoding::Pax,
            path,
            EntryType::Regular,
            None,
            b"",
        );
    }
    let archive = scratch.archive(builder);
    for pattern in [
        "tree/nested/[a-z]*",
        "tree/nested/[[:alpha:]]*",
        "tree/nested/[!0-9]*",
    ] {
        let output = run_tar(
            scratch.path(),
            &[
                "--wildcards",
                "-tf",
                archive.to_str().unwrap(),
                pattern,
                "--no-wildcards",
                "star*name",
            ],
        );
        assert_status(&output, 0);
        assert_eq!(
            output.stdout,
            b"tree/nested/script\ntree/nested/drop\nstar*name\n"
        );
    }
}

#[test]
fn verbose_creation_uses_stdout_for_files_and_stderr_for_archive_streams() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("line\nname"), b"verbose payload").unwrap();
    let output = run_tar(scratch.path(), &["-cvf", "bundle.tar", "line\nname"]);
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"line\\nname\n");
    assert_eq!(output.stderr, b"");
    let output = run_tar(scratch.path(), &["-xvOf", "bundle.tar", "line\nname"]);
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"verbose payload");
    assert_eq!(output.stderr, b"line\\nname\n");
    let output = run_tar(scratch.path(), &["-cvf", "-", "line\nname"]);
    assert_status(&output, 0);
    assert_eq!(output.stderr, b"line\\nname\n");
    let mut archive = tar::Archive::new(output.stdout.as_slice());
    let mut entries = archive.entries().unwrap();
    let mut entry = entries.next().unwrap().unwrap();
    assert_eq!(entry.path().unwrap().as_ref(), Path::new("line\nname"));
    let mut data = Vec::new();
    entry.read_to_end(&mut data).unwrap();
    assert_eq!(data, b"verbose payload");
}

#[test]
fn empty_creation_and_unsupported_format_never_truncate_an_existing_archive() {
    let scratch = Scratch::new();
    fs::write(scratch.path().join("bundle.tar"), b"existing archive").unwrap();
    let output = run_tar(scratch.path(), &["-cf", "bundle.tar"]);
    assert_status(&output, 2);
    assert!(!output.stderr.is_empty());
    fs::write(scratch.path().join("data"), b"payload").unwrap();
    assert_status(
        &run_tar(
            scratch.path(),
            &["--format=pax", "-cf", "bundle.tar", "data"],
        ),
        2,
    );
    assert_eq!(
        fs::read(scratch.path().join("bundle.tar")).unwrap(),
        b"existing archive"
    );
}

#[test]
fn malformed_later_headers_still_fail_after_completed_directory_metadata_is_restored() {
    let scratch = Scratch::new();
    let root = scratch.extraction_root();
    let mut builder = Builder::new(Vec::new());
    let mut directory = header(EntryType::Directory);
    directory.set_mode(0o555);
    directory.set_mtime(1_700_000_000);
    append(
        &mut builder,
        Encoding::Header,
        "tree/",
        None,
        directory,
        b"",
    );
    member(
        &mut builder,
        Encoding::Header,
        "tree/data",
        EntryType::Regular,
        None,
        b"complete",
    );
    let mut bytes = builder.into_inner().unwrap();
    bytes.truncate(1536);
    bytes.extend_from_slice(&[b'x'; 256]);
    let archive = scratch.path().join("truncated.tar");
    fs::write(&archive, bytes).unwrap();
    let output = run_tar(&root, &["-xf", archive.to_str().unwrap()]);
    let directory = fs::metadata(root.join("tree")).unwrap();
    fs::set_permissions(root.join("tree"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_status(&output, 2);
    assert_eq!(fs::read(root.join("tree/data")).unwrap(), b"complete");
    assert_eq!(directory.mode() & 0o7777, 0o555);
    assert_eq!(directory.mtime(), 1_700_000_000);
}

#[test]
fn verbose_stripped_extraction_reports_original_member_names_including_directory_slashes() {
    let scratch = Scratch::new();
    let root = scratch.extraction_root();
    let mut builder = Builder::new(Vec::new());
    let mut directory = header(EntryType::Directory);
    directory.set_mode(0o755);
    append(
        &mut builder,
        Encoding::Pax,
        "top/tree/",
        None,
        directory,
        b"",
    );
    member(
        &mut builder,
        Encoding::Pax,
        "top/tree/data",
        EntryType::Regular,
        None,
        b"payload",
    );
    let archive = scratch.archive(builder);
    let output = run_tar(
        &root,
        &["-xvf", archive.to_str().unwrap(), "--strip-components=1"],
    );
    assert_status(&output, 0);
    assert_eq!(output.stdout, b"top/tree/\ntop/tree/data\n");
    assert_eq!(fs::read(root.join("tree/data")).unwrap(), b"payload");
}

#[test]
fn wildcard_mode_is_captured_per_exclusion_in_creation_and_extraction() {
    let scratch = Scratch::new();
    fs::create_dir(scratch.path().join("tree")).unwrap();
    let mut builder = Builder::new(Vec::new());
    for name in ["star*name", "starXname", "other1", "keep"] {
        let path = format!("tree/{name}");
        fs::write(scratch.path().join(&path), b"payload").unwrap();
        member(
            &mut builder,
            Encoding::Pax,
            &path,
            EntryType::Regular,
            None,
            b"payload",
        );
    }
    let output = run_tar(
        scratch.path(),
        &[
            "--no-wildcards",
            "--exclude=star*name",
            "--wildcards",
            "--exclude=other?",
            "-cf",
            "created.tar",
            "tree",
        ],
    );
    assert_status(&output, 0);
    let output = run_tar(scratch.path(), &["-tf", "created.tar"]);
    assert_status(&output, 0);
    let listing = String::from_utf8(output.stdout).unwrap();
    assert!(listing.lines().any(|line| line == "tree/starXname"));
    assert!(listing.lines().any(|line| line == "tree/keep"));
    assert!(!listing.contains("star*name"));
    assert!(!listing.contains("other1"));

    let archive = scratch.archive(builder);
    let root = scratch.extraction_root();
    let output = run_tar(
        &root,
        &[
            "--no-wildcards",
            "--exclude=star*name",
            "--wildcards",
            "--exclude=other?",
            "-xf",
            archive.to_str().unwrap(),
        ],
    );
    assert_status(&output, 0);
    assert_eq!(fs::read(root.join("tree/starXname")).unwrap(), b"payload");
    assert_eq!(fs::read(root.join("tree/keep")).unwrap(), b"payload");
    assert!(!root.join("tree/star*name").exists());
    assert!(!root.join("tree/other1").exists());
}

#[test]
fn replacing_an_archive_name_invalidates_its_cached_hardlink_target() {
    for first_name in ["a", "./a"] {
        let scratch = Scratch::new();
        fs::create_dir(scratch.path().join("one")).unwrap();
        fs::create_dir(scratch.path().join("two")).unwrap();
        fs::write(scratch.path().join("one/a"), b"old\n").unwrap();
        fs::hard_link(scratch.path().join("one/a"), scratch.path().join("one/b")).unwrap();
        fs::write(scratch.path().join("two/a"), b"new\n").unwrap();
        let output = run_tar(
            scratch.path(),
            &[
                "-cf",
                "bundle.tar",
                "-C",
                "one",
                first_name,
                "-C",
                "../two",
                "a",
                "-C",
                "../one",
                "b",
            ],
        );
        assert_status(&output, 0);
        let root = scratch.extraction_root();
        tar::Archive::new(fs::File::open(scratch.path().join("bundle.tar")).unwrap())
            .unpack(&root)
            .unwrap();
        assert_eq!(fs::read(root.join("a")).unwrap(), b"new\n");
        assert_eq!(fs::read(root.join("b")).unwrap(), b"old\n");
        assert_ne!(
            fs::metadata(root.join("a")).unwrap().ino(),
            fs::metadata(root.join("b")).unwrap().ino(),
        );
    }
}

#[test]
fn fifo_creation_uses_the_archive_name_after_directory_changes() {
    let scratch = Scratch::new();
    let source = scratch.path().join("source");
    fs::create_dir(&source).unwrap();
    let fifo = source.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o640)).unwrap();
    fs::set_permissions(&fifo, fs::Permissions::from_mode(0o640)).unwrap();
    let output = run_tar(
        scratch.path(),
        &["-cf", "bundle.tar", "-C", "source", "fifo"],
    );
    assert_status(&output, 0);
    let mut archive = tar::Archive::new(fs::File::open(scratch.path().join("bundle.tar")).unwrap());
    let mut entries = archive.entries().unwrap();
    let fifo = entries.next().unwrap().unwrap();
    assert_eq!(fifo.path().unwrap().as_ref(), Path::new("fifo"));
    assert!(fifo.header().entry_type().is_fifo());
    assert_eq!(fifo.header().mode().unwrap() & 0o7777, 0o640);
    assert_eq!(fifo.size(), 0);
    assert!(entries.next().is_none());
}

#[test]
fn hardlinks_to_internal_symlinks_preserve_the_symlink_inode_after_stripping() {
    for strip in [0, 1] {
        let scratch = Scratch::new();
        let root = scratch.extraction_root();
        fs::write(root.join("hard"), b"replaced").unwrap();
        let prefix = if strip == 0 { "" } else { "top/" };
        let mut builder = Builder::new(Vec::new());
        member(
            &mut builder,
            Encoding::Pax,
            &format!("{prefix}data"),
            EntryType::Regular,
            None,
            b"payload",
        );
        member(
            &mut builder,
            Encoding::Pax,
            &format!("{prefix}sym"),
            EntryType::Symlink,
            Some("data"),
            b"",
        );
        member(
            &mut builder,
            Encoding::Pax,
            &format!("{prefix}hard"),
            EntryType::Link,
            Some(&format!("{prefix}sym")),
            b"",
        );
        let archive = scratch.archive(builder);
        assert_status(&extract(&root, &archive, strip), 0);
        let source = fs::symlink_metadata(root.join("sym")).unwrap();
        let linked = fs::symlink_metadata(root.join("hard")).unwrap();
        assert!(linked.is_symlink());
        assert_eq!(linked.ino(), source.ino());
        assert_ne!(linked.ino(), fs::metadata(root.join("data")).unwrap().ino());
        assert_eq!(fs::read_link(root.join("hard")).unwrap(), Path::new("data"));
        assert_eq!(fs::read(root.join("hard")).unwrap(), b"payload");
    }
}

#[test]
fn preserving_a_hardlink_source_symlink_does_not_allow_external_referents() {
    for strip in [0, 1] {
        let scratch = Scratch::new();
        let root = scratch.extraction_root();
        let outside = scratch.outside();
        symlink(outside.join("sentinel"), root.join("sym")).unwrap();
        let prefix = if strip == 0 { "" } else { "top/" };
        let mut builder = Builder::new(Vec::new());
        member(
            &mut builder,
            Encoding::Pax,
            &format!("{prefix}hard"),
            EntryType::Link,
            Some(&format!("{prefix}sym")),
            b"",
        );
        let archive = scratch.archive(builder);
        assert_status(&extract(&root, &archive, strip), 2);
        assert!(!root.join("hard").exists());
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside data");
        assert_eq!(fs::symlink_metadata(root.join("sym")).unwrap().nlink(), 1);
    }
}

#[test]
fn deferred_directory_modes_follow_resolved_children_across_aliases_and_extraction_roots() {
    // SAFETY: geteuid has no arguments or memory preconditions.
    let root_user = unsafe { libc::geteuid() == 0 };
    let dropped_executable = if root_user {
        let executable = PublicExecutable::new();
        if !executable.probe() {
            return;
        }
        Some(executable)
    } else {
        None
    };
    for strip in [0, 1] {
        for positional_roots in [false, true] {
            let scratch = Scratch::new();
            let root = scratch.extraction_root();
            fs::create_dir(root.join("a")).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(root.join("a"), fs::Permissions::from_mode(0o755)).unwrap();
            let prefix = if strip == 0 { "" } else { "top/" };
            let mut builder = Builder::new(Vec::new());
            let mut parent = header(EntryType::Directory);
            parent.set_mode(0o000);
            parent.set_mtime(1_700_000_000);
            append(
                &mut builder,
                Encoding::Pax,
                &format!("{prefix}a/b/"),
                None,
                parent,
                b"",
            );
            member(
                &mut builder,
                Encoding::Pax,
                &format!("{prefix}z"),
                EntryType::Symlink,
                Some(if positional_roots { "b" } else { "a/b" }),
                b"",
            );
            let mut child = header(EntryType::Directory);
            child.set_mode(0o700);
            child.set_mtime(1_700_000_123);
            append(
                &mut builder,
                Encoding::Pax,
                &format!("{prefix}z/child/"),
                None,
                child,
                b"",
            );
            let archive = scratch.archive(builder);
            fs::set_permissions(&archive, fs::Permissions::from_mode(0o644)).unwrap();
            let executable: &Path = if root_user {
                dropped_executable
                    .as_ref()
                    .expect("probed dropped executable")
                    .path()
            } else {
                Path::new(env!("CARGO_BIN_EXE_axe"))
            };
            let mut child = command_with_executable(scratch.path(), executable);
            child
                .args(["--applet", "tar", "--", "-xf"])
                .arg(&archive)
                .arg(format!("--strip-components={strip}"));
            if positional_roots {
                child
                    .args(["-C", "extract"])
                    .arg(format!("{prefix}a/b"))
                    .args(["-C", "a"])
                    .arg(format!("{prefix}z"))
                    .arg(format!("{prefix}z/child"));
            } else {
                child.current_dir(&root);
            }
            if root_user {
                fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755)).unwrap();
                chown(&root, Some(65534), Some(65534)).unwrap();
                chown(root.join("a"), Some(65534), Some(65534)).unwrap();
                child.gid(65534).uid(65534);
            }
            let output = child
                .output()
                .expect("extract aliases without root privileges");
            let parent = fs::metadata(root.join("a/b"));
            let _ = fs::set_permissions(root.join("a/b"), fs::Permissions::from_mode(0o755));
            assert_status(&output, 0);
            let parent = parent.unwrap();
            let child = fs::metadata(root.join("a/b/child")).unwrap();
            assert_eq!(parent.mode() & 0o7777, 0o000);
            assert_eq!(parent.mtime(), 1_700_000_000);
            assert_eq!(child.mode() & 0o7777, 0o700);
            assert_eq!(child.mtime(), 1_700_000_123);
        }
    }
}

#[test]
fn deferred_directory_metadata_does_not_reenter_a_restricted_symlink_navigation_parent() {
    let scratch = Scratch::new();
    let root = scratch.extraction_root();
    fs::create_dir(root.join("target")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(root.join("target"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut builder = Builder::new(Vec::new());
    let mut parent = header(EntryType::Directory);
    parent.set_mode(0o000);
    parent.set_mtime(1_700_000_000);
    append(
        &mut builder,
        Encoding::Pax,
        "deep/nested/",
        None,
        parent,
        b"",
    );
    member(
        &mut builder,
        Encoding::Pax,
        "deep/nested/z",
        EntryType::Symlink,
        Some("../../target"),
        b"",
    );
    let mut child = header(EntryType::Directory);
    child.set_mode(0o700);
    child.set_mtime(1_700_000_123);
    append(
        &mut builder,
        Encoding::Pax,
        "deep/nested/z/child/",
        None,
        child,
        b"",
    );
    let archive = scratch.archive(builder);
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o644)).unwrap();
    // SAFETY: geteuid has no arguments or memory preconditions.
    let root_user = unsafe { libc::geteuid() == 0 };
    let dropped_executable = if root_user {
        let executable = PublicExecutable::new();
        if !executable.probe() {
            return;
        }
        Some(executable)
    } else {
        None
    };
    let executable: &Path = if root_user {
        dropped_executable
            .as_ref()
            .expect("probed dropped executable")
            .path()
    } else {
        Path::new(env!("CARGO_BIN_EXE_axe"))
    };
    let mut child = command_with_executable(&root, executable);
    child.args(["--applet", "tar", "--", "-xf"]).arg(&archive);
    if root_user {
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755)).unwrap();
        chown(&root, Some(65534), Some(65534)).unwrap();
        chown(root.join("target"), Some(65534), Some(65534)).unwrap();
        child.gid(65534).uid(65534);
    }
    let output = child
        .output()
        .expect("finalize through resolved directory names");
    let parent = fs::metadata(root.join("deep/nested"));
    let _ = fs::set_permissions(root.join("deep/nested"), fs::Permissions::from_mode(0o755));
    assert_status(&output, 0);
    let parent = parent.unwrap();
    let child = fs::metadata(root.join("target/child")).unwrap();
    assert_eq!(parent.mode() & 0o7777, 0o000);
    assert_eq!(parent.mtime(), 1_700_000_000);
    assert_eq!(child.mode() & 0o7777, 0o700);
    assert_eq!(child.mtime(), 1_700_000_123);
}
