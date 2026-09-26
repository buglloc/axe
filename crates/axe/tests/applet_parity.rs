#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::LazyLock;

const REFERENCE_PATH_ENV: &str = "AXE_REFERENCE_PATH";
static REFERENCE_PATH: LazyLock<OsString> = LazyLock::new(|| {
    std::env::var_os(REFERENCE_PATH_ENV).unwrap_or_else(|| {
        panic!(
            "{REFERENCE_PATH_ENV} is not set; enter the project shell with `nix develop .#default`"
        )
    })
});

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-parity-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create parity scratch directory");
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

#[derive(Clone, Copy)]
struct Case<'a> {
    label: &'a str,
    args: &'a [&'a str],
    input: &'a [u8],
}

#[derive(Debug, Eq, PartialEq)]
enum Entry {
    Directory(PathBuf),
    File(PathBuf, Vec<u8>),
    Symlink(PathBuf, PathBuf),
}

fn reference_path() -> &'static OsStr {
    REFERENCE_PATH.as_os_str()
}

fn reference_program(applet: &str) -> PathBuf {
    std::env::split_paths(reference_path())
        .map(|directory| directory.join(applet))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!(
                "reference executable `{applet}` is absent from {REFERENCE_PATH_ENV}={}",
                reference_path().to_string_lossy()
            )
        })
}

fn configure(command: &mut Command, current_dir: &Path) {
    command
        .current_dir(current_dir)
        .env_clear()
        .env("HOME", current_dir)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", reference_path())
        .env("TZ", "UTC");
}

fn run_command(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn parity command");
    child
        .stdin
        .take()
        .expect("parity command stdin is piped")
        .write_all(input)
        .expect("write parity command input");
    child.wait_with_output().expect("wait for parity command")
}

fn run_reference(applet: &str, args: &[&str], input: &[u8], current_dir: &Path) -> Output {
    let mut command = Command::new(reference_program(applet));
    command.args(args);
    configure(&mut command, current_dir);
    run_command(&mut command, input)
}

fn run_applet(applet: &str, args: &[&str], input: &[u8], current_dir: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command.args(["--applet", applet, "--"]).args(args);
    configure(&mut command, current_dir);
    run_command(&mut command, input)
}

fn assert_parity(applet: &str, case: Case<'_>, current_dir: &Path) {
    let reference = run_reference(applet, case.args, case.input, current_dir);
    let actual = run_applet(applet, case.args, case.input, current_dir);
    let context = || {
        format!(
            "{} ({applet} {})\nreference: {}\naxe: {}",
            case.label,
            case.args.join(" "),
            describe(&reference),
            describe(&actual)
        )
    };

    assert_eq!(
        (actual.status.code(), actual.status.signal()),
        (reference.status.code(), reference.status.signal()),
        "exit status mismatch: {}",
        context()
    );
    assert_eq!(
        actual.stdout,
        reference.stdout,
        "stdout mismatch: {}",
        context()
    );
    assert_eq!(
        actual.stderr,
        reference.stderr,
        "stderr mismatch: {}",
        context()
    );
}

fn assert_cases(applet: &str, cases: &[Case<'_>]) {
    let scratch = Scratch::new(applet);
    for case in cases {
        assert_parity(applet, *case, scratch.path());
    }
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed: {}",
        describe(output)
    );
}

fn describe(output: &Output) -> String {
    format!(
        "status={:?}, signal={:?}, stdout={:?}, stderr={:?}",
        output.status.code(),
        output.status.signal(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn snapshot(root: &Path) -> Vec<Entry> {
    fn visit(root: &Path, relative: &Path, entries: &mut Vec<Entry>) {
        let directory = root.join(relative);
        let mut children = fs::read_dir(&directory)
            .expect("read extracted directory")
            .map(|entry| entry.expect("read extracted entry"))
            .collect::<Vec<_>>();
        children.sort_unstable_by_key(|entry| entry.file_name());

        for child in children {
            let child_relative = relative.join(child.file_name());
            let file_type = child.file_type().expect("read extracted entry type");
            if file_type.is_dir() {
                entries.push(Entry::Directory(child_relative.clone()));
                visit(root, &child_relative, entries);
            } else if file_type.is_symlink() {
                entries.push(Entry::Symlink(
                    child_relative,
                    fs::read_link(child.path()).expect("read extracted symlink"),
                ));
            } else {
                entries.push(Entry::File(
                    child_relative,
                    fs::read(child.path()).expect("read extracted file"),
                ));
            }
        }
    }

    let mut entries = Vec::new();
    visit(root, Path::new(""), &mut entries);
    entries
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_matches_reference_for_filters_bindings_and_exit_status() {
    assert_cases(
        "jq",
        &[
            Case {
                label: "compact selection",
                args: &["-c", ".items[] | select(.enabled)"],
                input: br#"{"items":[{"enabled":true,"id":1},{"enabled":false,"id":2}]}"#,
            },
            Case {
                label: "raw string output",
                args: &["-r", ".name"],
                input: br#"{"name":"axe"}"#,
            },
            Case {
                label: "JSON variable binding",
                args: &["-c", "--argjson", "value", "[1,2]", "{value: $value}"],
                input: b"null\n",
            },
            Case {
                label: "false exit status",
                args: &["-e", ".ok"],
                input: br#"{"ok":false}"#,
            },
            Case {
                label: "implicit identity filter",
                args: &[],
                input: br#"{"value":1}"#,
            },
            Case {
                label: "slurped inputs",
                args: &["-c", "-s", "."],
                input: b"1\n{\"value\":2}\n",
            },
            Case {
                label: "recursively sorted object keys",
                args: &["-cS", "."],
                input: br#"{"z":{"b":1,"a":2},"a":0}"#,
            },
            Case {
                label: "raw input lines",
                args: &["-Rc", "."],
                input: b"alpha\nbeta\n",
            },
            Case {
                label: "joined raw output",
                args: &["-Rj", "."],
                input: b"alpha\nbeta\n",
            },
        ],
    );
}

#[cfg(feature = "applet-strings")]
#[test]
fn strings_matches_gnu_for_offsets_encodings_and_whitespace() {
    assert_cases(
        "strings",
        &[
            Case {
                label: "hexadecimal offsets",
                args: &["-n", "5", "-t", "x"],
                input: b"\0alpha\0tiny\0longer string\0",
            },
            Case {
                label: "little-endian 16-bit input",
                args: &["-e", "l", "-n", "4"],
                input: b"J\0u\0n\0k\0\0\0",
            },
            Case {
                label: "embedded whitespace",
                args: &["-w", "-n", "4"],
                input: b"\0line\tvalue\0next\nline\0",
            },
        ],
    );
}

#[cfg(feature = "applet-file")]
#[test]
fn file_matches_reference_for_stable_media_types() {
    assert_cases(
        "file",
        &[
            Case {
                label: "GIF media type",
                args: &["-b", "--mime-type", "-"],
                input: b"GIF89a\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
            },
            Case {
                label: "PDF media type",
                args: &["-b", "--mime-type", "-"],
                input: b"%PDF-1.7\n",
            },
            Case {
                label: "ASCII text description",
                args: &["-b", "-"],
                input: b"plain ascii text\n",
            },
            Case {
                label: "JSON media type",
                args: &["-b", "--mime-type", "-"],
                input: br#"{"value":1}"#,
            },
            Case {
                label: "JSON description",
                args: &["-b", "-"],
                input: br#"{"value":1}"#,
            },
        ],
    );
}

#[cfg(feature = "applet-findutils")]
#[test]
fn xargs_matches_gnu_for_tokenization_and_replacement() {
    assert_cases(
        "xargs",
        &[
            Case {
                label: "quoted and escaped words",
                args: &["-n", "2", "printf", "<%s><%s>\\n"],
                input: b"'a b' c\\ d\n",
            },
            Case {
                label: "NUL-delimited records",
                args: &["-0", "-n", "2", "printf", "<%s><%s>\\n"],
                input: b"a b\0c\nd\0",
            },
            Case {
                label: "logical-line replacement",
                args: &["-I{}", "printf", "[%s]\\n", "pre-{}-post"],
                input: b"one two\nthree\n",
            },
            Case {
                label: "empty input suppression",
                args: &["-r", "printf", "unexpected"],
                input: b"",
            },
            Case {
                label: "custom byte delimiter",
                args: &["-d", ",", "-n", "2", "printf", "<%s><%s>\\n"],
                input: b"a,b,c",
            },
            Case {
                label: "logical line batches",
                args: &["-L", "2", "printf", "<%s><%s><%s>\\n"],
                input: b"a b\nc\nd e\n",
            },
        ],
    );
}

fn assert_compression_interoperability(applet: &str) {
    let scratch = Scratch::new(applet);
    let input = b"first\0member\nsecond member\xfffirst\0member\n";

    let reference_stream = run_reference(applet, &["-c"], input, scratch.path());
    assert_success("reference compression", &reference_stream);
    assert_parity(
        applet,
        Case {
            label: "reference stream decompression",
            args: &["-dc"],
            input: &reference_stream.stdout,
        },
        scratch.path(),
    );

    let axe_stream = run_applet(applet, &["-c"], input, scratch.path());
    assert_success("axe compression", &axe_stream);
    let decoded = run_reference(applet, &["-dc"], &axe_stream.stdout, scratch.path());
    assert_success("reference decompression of axe stream", &decoded);
    assert_eq!(
        decoded.stdout, input,
        "{applet} encoded a different payload"
    );
    assert!(
        decoded.stderr.is_empty(),
        "reference decoder emitted stderr for {applet}: {}",
        String::from_utf8_lossy(&decoded.stderr)
    );
}

#[cfg(feature = "applet-gzip")]
#[test]
fn gzip_streams_interoperate_with_gnu() {
    assert_compression_interoperability("gzip");

    let scratch = Scratch::new("gzip-test");
    let compressed = run_reference("gzip", &["-c"], b"integrity payload\n", scratch.path());
    assert_success("reference gzip compression", &compressed);
    assert_parity(
        "gzip",
        Case {
            label: "integrity test from standard input",
            args: &["-t"],
            input: &compressed.stdout,
        },
        scratch.path(),
    );

    fs::write(scratch.path().join("payload.gz"), &compressed.stdout)
        .expect("write gzip integrity fixture");
    let actual = run_applet("gzip", &["-t", "payload.gz"], b"", scratch.path());
    assert_success("axe gzip file integrity test", &actual);
    assert!(actual.stdout.is_empty(), "{}", describe(&actual));
    assert!(actual.stderr.is_empty(), "{}", describe(&actual));
    assert!(
        scratch.path().join("payload.gz").exists(),
        "gzip -t removed its input"
    );

    let mut concatenated = Vec::new();
    concatenated.extend_from_slice(&compressed.stdout);
    let second = run_reference("gzip", &["-c"], b"second member\n", scratch.path());
    assert_success("reference gzip second member compression", &second);
    concatenated.extend_from_slice(&second.stdout);
    let decoded = run_applet("gzip", &["-dc"], &concatenated, scratch.path());
    assert_success("axe gzip concatenated member decode", &decoded);
    assert_eq!(decoded.stdout, b"integrity payload\nsecond member\n");
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_binds_raw_and_slurped_files() {
    let scratch = Scratch::new("jq-file-bindings");
    fs::write(scratch.path().join("raw.txt"), b"raw\ntext\n").expect("write rawfile fixture");
    fs::write(scratch.path().join("values.json"), b"1\n{\"two\":2}\n")
        .expect("write slurpfile fixture");

    assert_parity(
        "jq",
        Case {
            label: "rawfile and slurpfile bindings",
            args: &[
                "-c",
                "--rawfile",
                "raw",
                "raw.txt",
                "--slurpfile",
                "values",
                "values.json",
                "{raw:$raw, values:$values}",
            ],
            input: b"null\n",
        },
        scratch.path(),
    );
}

#[cfg(feature = "applet-compression")]
#[test]
fn bzip2_and_xz_streams_interoperate_with_reference_tools() {
    for applet in ["bzip2", "xz"] {
        assert_compression_interoperability(applet);
    }
}

#[cfg(feature = "applet-tar")]
#[test]
fn tar_lists_and_extracts_gnu_archives() {
    let scratch = Scratch::new("tar");
    let fixture = scratch.path().join("fixture");
    fs::create_dir_all(fixture.join("nested")).expect("create tar fixture tree");
    fs::write(fixture.join("alpha.txt"), b"alpha\n").expect("write tar text fixture");
    fs::write(fixture.join("nested/data.bin"), b"payload\0").expect("write tar binary fixture");
    std::os::unix::fs::symlink("alpha.txt", fixture.join("alpha.link"))
        .expect("create tar symlink fixture");

    let archive = run_reference("tar", &["-cf", "-", "fixture"], b"", scratch.path());
    assert_success("reference tar creation", &archive);
    assert_parity(
        "tar",
        Case {
            label: "GNU archive listing",
            args: &["-tf", "-"],
            input: &archive.stdout,
        },
        scratch.path(),
    );
    assert_parity(
        "tar",
        Case {
            label: "GNU archive listing with long options",
            args: &["--list", "--file", "-"],
            input: &archive.stdout,
        },
        scratch.path(),
    );

    let reference_root = scratch.path().join("reference");
    let axe_root = scratch.path().join("axe");
    fs::create_dir(&reference_root).expect("create reference extraction directory");
    fs::create_dir(&axe_root).expect("create axe extraction directory");

    let reference = run_reference("tar", &["-xf", "-"], &archive.stdout, &reference_root);
    assert_success("reference tar extraction", &reference);
    let actual = run_applet("tar", &["-xf", "-"], &archive.stdout, &axe_root);
    assert_success("axe tar extraction", &actual);
    assert_eq!(
        actual.stdout, reference.stdout,
        "tar extraction stdout differs"
    );
    assert_eq!(
        actual.stderr, reference.stderr,
        "tar extraction stderr differs"
    );
    assert_eq!(
        snapshot(&axe_root.join("fixture")),
        snapshot(&reference_root.join("fixture"))
    );

    let long_root = scratch.path().join("long-options");
    fs::create_dir(&long_root).expect("create long-option extraction directory");
    let actual = run_applet(
        "tar",
        &["--extract", "--file", "-", "--directory", "long-options"],
        &archive.stdout,
        scratch.path(),
    );
    assert_success("axe tar long-option extraction", &actual);
    assert_eq!(
        snapshot(&long_root.join("fixture")),
        snapshot(&reference_root.join("fixture"))
    );

    let axe_archive = run_applet(
        "tar",
        &["--create", "--file", "-", "fixture"],
        b"",
        scratch.path(),
    );
    assert_success("axe tar long-option creation", &axe_archive);
    let from_axe_root = scratch.path().join("from-axe");
    fs::create_dir(&from_axe_root).expect("create reference extraction directory for axe archive");
    let extracted = run_reference(
        "tar",
        &["--extract", "--file", "-"],
        &axe_archive.stdout,
        &from_axe_root,
    );
    assert_success("reference tar extraction of axe archive", &extracted);
    assert_eq!(snapshot(&from_axe_root.join("fixture")), snapshot(&fixture));
}

#[cfg(all(target_os = "linux", feature = "applet-linux-network"))]
#[test]
fn ip_link_and_address_json_match_iproute2_schema() {
    const LINK_KEYS: &[&str] = &[
        "ifindex",
        "ifname",
        "flags",
        "mtu",
        "qdisc",
        "operstate",
        "linkmode",
        "group",
        "txqlen",
        "link_type",
        "address",
        "broadcast",
        "permaddr",
        "altnames",
    ];
    const ADDRESS_KEYS: &[&str] = &[
        "family",
        "local",
        "prefixlen",
        "scope",
        "label",
        "broadcast",
        "secondary",
        "temporary",
        "nodad",
        "optimistic",
        "dadfailed",
        "home",
        "deprecated",
        "tentative",
        "mngtmpaddr",
        "noprefixroute",
        "autojoin",
        "stable-privacy",
    ];

    let scratch = Scratch::new("ip-json");
    for object in ["link", "address"] {
        let args = &["-j", object, "show"];
        let reference = run_reference("ip", args, b"", scratch.path());
        assert_success("reference ip JSON", &reference);
        let actual = run_applet("ip", args, b"", scratch.path());
        assert_success("axe ip JSON", &actual);
        assert!(actual.stderr.is_empty(), "{}", describe(&actual));

        let reference: Vec<serde_json::Value> =
            serde_json::from_slice(&reference.stdout).expect("reference ip emits JSON array");
        let actual: Vec<serde_json::Value> =
            serde_json::from_slice(&actual.stdout).expect("axe ip emits JSON array");
        assert_eq!(
            actual.len(),
            reference.len(),
            "{object} interface count differs"
        );

        for expected_link in &reference {
            let ifindex = &expected_link["ifindex"];
            let actual_link = actual
                .iter()
                .find(|link| &link["ifindex"] == ifindex)
                .unwrap_or_else(|| panic!("{object} JSON omitted interface {ifindex}"));
            assert_json_projection(expected_link, actual_link, LINK_KEYS, object);

            if object == "address" {
                let expected_addresses = expected_link["addr_info"]
                    .as_array()
                    .expect("reference addr_info is an array");
                let actual_addresses = actual_link["addr_info"]
                    .as_array()
                    .expect("axe addr_info is an array");
                assert_eq!(
                    actual_addresses.len(),
                    expected_addresses.len(),
                    "address count differs for interface {ifindex}"
                );
                for expected_address in expected_addresses {
                    let actual_address = actual_addresses
                        .iter()
                        .find(|address| {
                            address["family"] == expected_address["family"]
                                && address["local"] == expected_address["local"]
                                && address["prefixlen"] == expected_address["prefixlen"]
                        })
                        .unwrap_or_else(|| {
                            panic!(
                                "address JSON omitted {}/{}/{}",
                                expected_address["family"],
                                expected_address["local"],
                                expected_address["prefixlen"]
                            )
                        });
                    assert_json_projection(
                        expected_address,
                        actual_address,
                        ADDRESS_KEYS,
                        "address",
                    );
                    for key in ["valid_life_time", "preferred_life_time"] {
                        if expected_address.get(key).is_some() {
                            assert!(
                                actual_address
                                    .get(key)
                                    .and_then(serde_json::Value::as_u64)
                                    .is_some(),
                                "address JSON field {key} is not an unsigned lifetime"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(all(target_os = "linux", feature = "applet-linux-network"))]
fn assert_json_projection(
    expected: &serde_json::Value,
    actual: &serde_json::Value,
    keys: &[&str],
    context: &str,
) {
    for key in keys {
        if let Some(expected) = expected.get(key) {
            assert_eq!(
                actual.get(key),
                Some(expected),
                "{context} JSON field {key} differs"
            );
        }
    }
}

#[cfg(all(target_os = "linux", feature = "applet-linux-storage"))]
#[test]
fn blkid_matches_util_linux_for_filtered_filesystem_type() {
    let scratch = Scratch::new("blkid");
    let image = scratch.path().join("ext4.img");
    let mut bytes = vec![0u8; 2048];
    bytes[1080..1082].copy_from_slice(&[0x53, 0xef]);
    fs::write(image, bytes).expect("write ext4 fixture");

    assert_parity(
        "blkid",
        Case {
            label: "ext4 type from a regular file",
            args: &["-s", "TYPE", "-o", "value", "ext4.img"],
            input: b"",
        },
        scratch.path(),
    );

    assert_parity(
        "blkid",
        Case {
            label: "ext4 export omits absent UUID and reports block size",
            args: &["-o", "export", "ext4.img"],
            input: b"",
        },
        scratch.path(),
    );
}
