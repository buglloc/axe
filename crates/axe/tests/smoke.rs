use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "axe-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch directory");
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

fn run_with_input(command: &mut Command, input: &[u8]) -> std::process::Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn command");
    child
        .stdin
        .take()
        .expect("command stdin is piped")
        .write_all(input)
        .expect("write command input");
    child.wait_with_output().expect("wait for command")
}

fn run_with_input_before_deadline(
    command: &mut Command,
    input: &[u8],
    timeout: std::time::Duration,
) -> std::process::Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn command");
    child
        .stdin
        .take()
        .expect("command stdin is piped")
        .write_all(input)
        .expect("write command input");

    let deadline = std::time::Instant::now() + timeout;
    let timed_out = loop {
        if child.try_wait().expect("poll command").is_some() {
            break false;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            break true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let output = child.wait_with_output().expect("wait for command");
    assert!(
        !timed_out,
        "command exceeded {timeout:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn offline_axe(scratch: &Scratch) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .env("AXE_STORE_URL", "http://127.0.0.1:9")
        .env("AXE_STORE_ADDRESSES", "127.0.0.1");
    command
}

#[test]
fn positional_applet_dispatch_accepts_optional_separator() {
    for args in [
        &["printf", "--", "%s\\n", "positional"][..],
        &["printf", "%s\\n", "positional"][..],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(args)
            .output()
            .expect("run positional applet");

        assert!(output.status.success());
        assert_eq!(output.stdout, b"positional\n");
    }
}

#[test]
fn bundled_direct_dispatch_does_not_initialize_store() {
    use std::io;
    use std::net::TcpListener;

    let scratch = Scratch::new("bundled-without-store");
    let store = scratch.path().join("store");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind Store probe");
    listener
        .set_nonblocking(true)
        .expect("make Store probe nonblocking");
    let address = listener.local_addr().expect("read Store probe address");

    for args in [&["--applet", "true"][..], &["true"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_axe"))
            .env_clear()
            .env("HOME", scratch.path())
            .env("PATH", "/nonexistent")
            .env("AXE_STORE_DIR", &store)
            .env("AXE_STORE_URL", format!("http://{address}"))
            .env("AXE_STORE_ADDRESSES", "127.0.0.1")
            .args(args)
            .output()
            .expect("run bundled applet");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(!store.exists(), "bundled applet initialized Store storage");
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
}

#[cfg(feature = "on-demand")]
#[test]
fn clean_tools_removes_only_current_metadata_namespace() {
    let scratch = Scratch::new("clean-tools");
    let store = scratch.path().join("store");
    let metadata = store.join("metadata");

    // Failed refreshes persist backoff under the actual namespace for each Store URL.
    offline_axe(&scratch)
        .arg("refresh-tools")
        .output()
        .expect("initialize current Store metadata");
    let namespace = fs::read_dir(&metadata)
        .expect("read current metadata namespace")
        .next()
        .expect("current namespace exists")
        .expect("read current namespace")
        .path();
    fs::create_dir_all(namespace.join("index")).expect("create current Index cache");
    fs::write(namespace.join("index/content"), b"index").expect("write current Index cache");

    offline_axe(&scratch)
        .env("AXE_STORE_URL", "http://127.0.0.1:9/other-edition")
        .arg("refresh-tools")
        .output()
        .expect("initialize other Store metadata");
    let namespaces: Vec<_> = fs::read_dir(&metadata)
        .expect("read Store metadata namespaces")
        .map(|entry| entry.expect("read metadata namespace").path())
        .collect();
    assert_eq!(namespaces.len(), 2, "expected two Store namespaces");
    let other_namespace = namespaces
        .into_iter()
        .find(|path| path != &namespace)
        .expect("other Store namespace exists");
    fs::create_dir_all(other_namespace.join("index")).expect("create other Index cache");
    fs::write(other_namespace.join("index/content"), b"other index")
        .expect("write other Index cache");

    for (directory, name, content) in [
        ("objects", "object", b"object".as_slice()),
        ("unpacked", "artifact", b"artifact".as_slice()),
        ("locks", "lock", b"lock".as_slice()),
    ] {
        fs::create_dir_all(store.join(directory)).expect("create shared cache");
        fs::write(store.join(directory).join(name), content).expect("write shared cache");
    }

    let output = offline_axe(&scratch)
        .arg("clean-tools")
        .output()
        .expect("run clean-tools");

    assert!(
        output.status.success(),
        "clean-tools stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("clean-tools output is UTF-8");
    assert_eq!(
        stdout,
        format!("{}\nclean-tools: cleaned 1 cache root\n", store.display())
    );
    assert!(!namespace.exists());
    assert_eq!(
        fs::read(other_namespace.join("index/content")).expect("other edition survives"),
        b"other index"
    );
    for (directory, name, content) in [
        ("objects", "object", b"object".as_slice()),
        ("unpacked", "artifact", b"artifact".as_slice()),
        ("locks", "lock", b"lock".as_slice()),
    ] {
        assert_eq!(
            fs::read(store.join(directory).join(name)).expect("shared cache survives"),
            content
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn unresolved_current_identity_uses_numeric_fallbacks() {
    const CHILD: &str = "AXE_TEST_UNRESOLVED_IDENTITY_CHILD";
    const AXE_EXECUTABLE: &str = "AXE_TEST_UNRESOLVED_IDENTITY_EXECUTABLE";
    const TEST_NAME: &str = "unresolved_current_identity_uses_numeric_fallbacks";

    fn hide_identity_file(target: &[u8]) {
        let source = b"/dev/null\0";
        // SAFETY: source and target are NUL-terminated and live through the
        // call. The test child owns its mount namespace.
        let result = unsafe {
            libc::mount(
                source.as_ptr().cast(),
                target.as_ptr().cast(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null::<libc::c_void>(),
            )
        };
        assert_eq!(
            result,
            0,
            "hide {}: {}",
            String::from_utf8_lossy(&target[..target.len() - 1]),
            std::io::Error::last_os_error()
        );
    }

    fn isolated_axe(executable: &Path) -> Command {
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env("HOME", "/tmp")
            .env("PATH", "/nonexistent")
            .env("AXE_STORE_MODE", "off")
            .env("AXE_STORE_DIR", "/tmp/axe-unresolved-identity")
            .env("AXE_STORE_URL", "http://127.0.0.1:9")
            .env("AXE_STORE_ADDRESSES", "127.0.0.1");
        command
    }

    if std::env::var_os(CHILD).is_some() {
        hide_identity_file(b"/etc/passwd\0");
        hide_identity_file(b"/etc/group\0");

        for (args, expected) in [
            (&["id"][..], "uid=0(0) gid=0(0) groups=0(0)\n"),
            (&["id", "-un"][..], "0\n"),
            (&["id", "-Gn"][..], "0\n"),
            (&["groups"][..], "0\n"),
        ] {
            let output = isolated_axe(Path::new(
                &std::env::var_os(AXE_EXECUTABLE).expect("AXE path is inherited"),
            ))
            .args(args)
            .output()
            .expect("run identity applet");
            assert!(
                output.status.success(),
                "{args:?} stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
            assert_eq!(output.stderr, b"", "{args:?} emitted a diagnostic");
        }

        let output = isolated_axe(Path::new(
            &std::env::var_os(AXE_EXECUTABLE).expect("AXE path is inherited"),
        ))
        .env("PS1", r"\u")
        .args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            r#"printf "%s\n%s\n" "${PS1@P}" "${GROUPS[*]}""#,
        ])
        .output()
        .expect("expand prompt without a resolvable username");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"0\n0\n");
        assert_eq!(output.stderr, b"");
        return;
    }

    let test_binary = std::env::current_exe().expect("locate smoke test binary");
    let axe = PathBuf::from(env!("CARGO_BIN_EXE_axe"))
        .canonicalize()
        .expect("canonicalize AXE executable");
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "--fork", "--"])
        .arg(test_binary)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD, "1")
        .env(AXE_EXECUTABLE, axe)
        .output()
        .expect("start isolated identity test");
    if !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("Operation not permitted")
    {
        eprintln!(
            "unresolved identity proof skipped: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return;
    }
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn store_mode_off_omits_store_commands_without_network_or_storage() {
    use std::io;
    use std::net::TcpListener;

    let scratch = Scratch::new("store-off");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind Store probe");
    listener
        .set_nonblocking(true)
        .expect("make Store probe nonblocking");
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_MODE", "off")
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .env(
            "AXE_STORE_URL",
            format!(
                "http://{}",
                listener.local_addr().expect("read probe address")
            ),
        )
        .env("AXE_STORE_ADDRESSES", "127.0.0.1")
        .arg("commands")
        .output()
        .expect("list commands with Store disabled");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("commands output is JSON");
    assert!(
        inventory["commands"]
            .as_array()
            .expect("commands is an array")
            .iter()
            .all(|command| command["source"] != "store")
    );
    let lookup = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_MODE", "off")
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .env(
            "AXE_STORE_URL",
            format!(
                "http://{}",
                listener.local_addr().expect("read probe address")
            ),
        )
        .env("AXE_STORE_ADDRESSES", "127.0.0.1")
        .args(["commands", "yq"])
        .output()
        .expect("look up Store command with Store disabled");
    assert_eq!(lookup.status.code(), Some(127));
    let lookup: serde_json::Value =
        serde_json::from_slice(&lookup.stdout).expect("command lookup is JSON");
    assert_eq!(
        lookup["error"],
        serde_json::json!({
            "kind": "unknown_command",
            "name": "yq",
            "store_mode": "off",
            "store_registry": "disabled",
        })
    );
    assert!(
        !scratch.path().join("store").exists(),
        "off mode initialized Store storage"
    );
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
}

#[test]
fn http_applet_returns_structured_http_error_response() {
    use std::io::{BufRead, BufReader, Read as _};
    use std::net::TcpListener;

    let scratch = Scratch::new("http-applet");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP server");
    let address = listener.local_addr().expect("read HTTP server address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept HTTP request");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("set HTTP read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone HTTP stream"));
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .expect("read HTTP request line");
        assert_eq!(request_line, "POST /probe HTTP/1.1\r\n");

        let mut content_length = None;
        let mut saw_agent_header = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).expect("read HTTP header");
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.trim_end().split_once(':').expect("valid HTTP header");
            if name.eq_ignore_ascii_case("content-length") {
                content_length = Some(value.trim().parse::<usize>().expect("numeric body length"));
            }
            if name.eq_ignore_ascii_case("x-agent") {
                saw_agent_header = value.trim() == "probe";
            }
        }
        assert!(saw_agent_header, "custom request header was not sent");

        let mut body = vec![0; content_length.expect("request has Content-Length")];
        reader
            .read_exact(&mut body)
            .expect("read HTTP request body");
        assert_eq!(body, b"request");

        stream
            .write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Type: application/octet-stream\r\nContent-Length: 6\r\nX-Trace: local\r\nConnection: close\r\n\r\n\0\xffbody",
            )
            .expect("write HTTP response");
    });

    let request_url = format!("http://user:secret@{address}/probe");
    let output = offline_axe(&scratch)
        .args([
            "--applet",
            "http",
            "--",
            "--max-bytes",
            "6",
            "-H",
            "X-Agent: probe",
            "-d",
            "request",
            &request_url,
        ])
        .output()
        .expect("run HTTP applet");
    server.join().expect("HTTP server completed");

    assert!(output.status.success());
    assert_eq!(output.stderr, b"");
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("HTTP output is JSON");
    assert_eq!(document["schema"], "axe_http");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["request"]["method"], "POST");
    assert_eq!(
        document["request"]["url"],
        format!("http://user:[REDACTED]@{address}/probe")
    );
    assert_eq!(document["response"]["status"], 404);
    assert_eq!(
        document["response"]["url"],
        format!("http://user:[REDACTED]@{address}/probe")
    );
    assert_eq!(document["response"]["redirects"], serde_json::json!([]));
    assert_eq!(document["response"]["body"]["encoding"], "base64");
    assert_eq!(document["response"]["body"]["data"], "AP9ib2R5");
}

#[test]
fn http_applet_preserves_metadata_when_response_exceeds_limit() {
    use std::net::TcpListener;

    let scratch = Scratch::new("http-applet-limit");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP server");
    let address = listener.local_addr().expect("read HTTP server address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept HTTP request");
        stream
            .write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Type: application/octet-stream\r\nContent-Length: 6\r\nX-Trace: preserved\r\nConnection: close\r\n\r\n\0\xffbody",
            )
            .expect("write oversized HTTP response");
    });

    let output = offline_axe(&scratch)
        .args([
            "--applet",
            "http",
            "--",
            "--max-bytes",
            "5",
            &format!("http://{address}/oversized"),
        ])
        .output()
        .expect("run bounded HTTP applet");
    server.join().expect("HTTP server completed");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"");
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("HTTP error output is JSON");
    assert_eq!(document["error"]["kind"], "response_too_large");
    assert_eq!(document["error"]["limit_bytes"], 5);
    assert_eq!(document["error"]["received_at_least_bytes"], 6);
    assert_eq!(document["response"]["status"], 404);
    assert_eq!(
        document["response"]["url"],
        format!("http://{address}/oversized")
    );
    assert!(
        document["response"]["headers"]
            .as_array()
            .expect("headers are an array")
            .iter()
            .any(|header| header["name"] == "x-trace" && header["data"] == "preserved")
    );
    assert!(document["response"].get("body").is_none());
}

#[test]
fn http_applet_reports_unsupported_extension_method_before_network() {
    let scratch = Scratch::new("http-applet-method");
    let output = offline_axe(&scratch)
        .args([
            "--applet",
            "http",
            "--",
            "-X",
            "PROPFIND",
            "http://127.0.0.1:9/",
        ])
        .output()
        .expect("run HTTP applet with extension method");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"");
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("HTTP error output is JSON");
    assert_eq!(document["request"]["method"], "PROPFIND");
    assert_eq!(document["error"]["kind"], "unsupported_method");
}

#[test]
fn http_applet_resolves_location_and_explains_unreplayable_redirect() {
    use std::io::{BufRead, BufReader, Read as _};
    use std::net::TcpListener;

    let scratch = Scratch::new("http-applet-redirect");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP server");
    let address = listener.local_addr().expect("read HTTP server address");
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept HTTP request");
            let mut reader = BufReader::new(stream.try_clone().expect("clone HTTP stream"));
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read HTTP request line");
            assert_eq!(request_line, "POST /nested/start HTTP/1.1\r\n");

            let mut content_length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).expect("read HTTP header");
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.trim_end().split_once(':').expect("valid HTTP header");
                if name.eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().expect("numeric body length");
                }
            }
            let mut body = vec![0; content_length];
            reader
                .read_exact(&mut body)
                .expect("read HTTP request body");
            assert_eq!(body, b"payload");

            stream
                .write_all(
                    b"HTTP/1.1 307 Temporary Redirect\r\nLocation: ../final?step=1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("write redirect response");
        }
    });

    let request_url = format!("http://{address}/nested/start");
    let first = offline_axe(&scratch)
        .args(["--applet", "http", "--", "-d", "payload", &request_url])
        .output()
        .expect("inspect redirect without following");
    assert!(first.status.success());
    let first_document: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("HTTP response is JSON");
    assert_eq!(first_document["response"]["status"], 307);
    assert_eq!(
        first_document["response"]["resolved_location"],
        format!("http://{address}/final?step=1")
    );

    let followed = offline_axe(&scratch)
        .args([
            "--applet",
            "http",
            "--",
            "-L",
            "-d",
            "payload",
            &request_url,
        ])
        .output()
        .expect("run redirecting HTTP applet");
    server.join().expect("HTTP server completed");

    assert_eq!(followed.status.code(), Some(1));
    assert_eq!(followed.stderr, b"");
    let error: serde_json::Value =
        serde_json::from_slice(&followed.stdout).expect("HTTP error output is JSON");
    assert_eq!(error["error"]["kind"], "redirect_replay_unsupported");
}

#[cfg(unix)]
#[test]
fn store_mode_cache_only_uses_path_without_network() {
    use std::io;
    use std::net::TcpListener;

    let scratch = Scratch::new("store-cache-only");
    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).expect("create PATH directory");
    let source = scratch.path().join("fallback.c");
    fs::write(
        &source,
        b"#include <stdio.h>\nint main(int argc, char **argv) { if (argc != 2) return 2; printf(\"path:%s\\n\", argv[1]); return 0; }\n",
    )
    .expect("write PATH fallback source");
    let external = bin.join("curl");
    let compiled = Command::new("cc")
        .arg("-Os")
        .arg(&source)
        .arg("-o")
        .arg(&external)
        .status()
        .expect("compile PATH fallback");
    assert!(compiled.success(), "compile PATH fallback");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind Store probe");
    listener
        .set_nonblocking(true)
        .expect("make Store probe nonblocking");

    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", &bin)
        .env("AXE_STORE_MODE", "cache-only")
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .env(
            "AXE_STORE_URL",
            format!(
                "http://{}",
                listener.local_addr().expect("read probe address")
            ),
        )
        .env("AXE_STORE_ADDRESSES", "127.0.0.1")
        .args(["--norc", "--noprofile", "-c", "curl sentinel"])
        .output()
        .expect("run cache-only Store command");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"path:sentinel\n");
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
}

#[cfg(unix)]
#[test]
fn applet_basename_dispatches_busybox_style() {
    let scratch = Scratch::new("argv0");
    let applet = scratch.path().join("printf");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_axe"), &applet).expect("create applet symlink");

    let output = Command::new(applet)
        .args(["%s\\n", "argv0"])
        .output()
        .expect("run applet symlink");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"argv0\n");
}

#[test]
fn bundled_pipeline_and_recursive_xargs_work_without_path() {
    let scratch = Scratch::new("empty-path");
    fs::create_dir(scratch.path().join("directory")).expect("create fixture directory");
    fs::write(
        scratch.path().join("directory/data.txt"),
        b"pattern\nother\n",
    )
    .expect("write grep fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "printf 'b\\na\\n' | sort; find . -type f | xargs grep -nH pattern; tar -czf archive.tar.gz directory",
        ])
        .output()
        .expect("run shell smoke scenario");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "a\nb\n./directory/data.txt:1:pattern\n"
    );
    assert!(scratch.path().join("archive.tar.gz").is_file());

    let archive = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .args(["--applet", "tar", "--", "-tzf", "archive.tar.gz"])
        .output()
        .expect("list generated archive");
    assert!(
        archive.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&archive.stderr)
    );
    assert!(
        String::from_utf8_lossy(&archive.stdout)
            .lines()
            .any(|path| path == "directory/data.txt"),
        "archive listing: {}",
        String::from_utf8_lossy(&archive.stdout)
    );
}

#[cfg(feature = "applet-tar")]
#[test]
fn tar_extracts_relative_archive_after_changing_directory() {
    let scratch = Scratch::new("tar-directory");
    let source = scratch.path().join("source");
    let destination = scratch.path().join("destination");
    fs::create_dir(&source).expect("create tar source");
    fs::create_dir(&destination).expect("create tar destination");
    fs::write(source.join("data.txt"), b"payload").expect("write tar fixture");

    let create = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .args(["--applet", "tar", "--", "-cf", "archive.tar", "source"])
        .output()
        .expect("create archive");
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let extract = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .args([
            "--applet",
            "tar",
            "--",
            "-C",
            "destination",
            "-xf",
            "archive.tar",
        ])
        .output()
        .expect("extract archive");
    assert!(
        extract.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&extract.stderr)
    );
    assert_eq!(
        fs::read(destination.join("source/data.txt")).expect("read extracted file"),
        b"payload"
    );
}

#[cfg(all(
    feature = "applet-admin-coreutils",
    feature = "applet-findutils",
    feature = "bundled-coreutils"
))]
#[test]
fn child_spawning_applets_resolve_bundled_commands_without_path() {
    let scratch = Scratch::new("child-resolver");
    fs::write(scratch.path().join("data"), b"").expect("write find fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "timeout 0.01 sleep 1; printf 'timeout=%s\\n' \"$?\"; find . -name data -exec printf 'found:%s\\n' {} \\;",
        ])
        .output()
        .expect("run child resolver scenario");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"timeout=124\nfound:./data\n");
}

#[test]
fn shell_reads_complete_multiline_program_from_piped_stdin() {
    let scratch = Scratch::new("multiline-shell-input");
    let output = run_with_input(
        offline_axe(&scratch).args(["--no-config", "--norc", "--noprofile"]),
        b"if true\nthen\n  printf 'complete\\n'\nfi\n",
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"complete\n");
    assert_eq!(output.stderr, b"");
}

#[test]
fn shell_preserves_utf8_reads_and_empty_ifs_fields() {
    let scratch = Scratch::new("utf8-shell-read");
    let output = run_with_input(
        offline_axe(&scratch).args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "read value; IFS=:; fields=a::b; set -- $fields; printf 'value=<%s>\\n' \"$value\"; printf 'field=<%s>\\n' \"$@\"",
        ]),
        "é\n".as_bytes(),
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        "value=<é>\nfield=<a>\nfield=<>\nfield=<b>\n".as_bytes()
    );
    assert_eq!(output.stderr, b"");
}

#[test]
fn shell_preserves_literal_heredoc_content_and_padded_braces() {
    let scratch = Scratch::new("shell-parser-regressions");
    let output = offline_axe(&scratch)
        .args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "printf '%s\\n' \"$(cat <<'EOF'\na (b's c) `echo SHOULD_NOT_EXECUTE >&2` d's\nEOF\n)\"; printf '<%s>\\n' {01..03}",
        ])
        .output()
        .expect("run shell parser scenario");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"a (b's c) `echo SHOULD_NOT_EXECUTE >&2` d's\n<01>\n<02>\n<03>\n"
    );
    assert_eq!(output.stderr, b"");
}

#[test]
fn shell_printf_and_large_redirection_prefix_terminate() {
    let scratch = Scratch::new("shell-termination-regressions");
    let output = run_with_input_before_deadline(
        offline_axe(&scratch).args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "printf 'foo' ignored extra; printf '\\n'; echo 999999999999999999999999999999999999999999999999999999>&1",
        ]),
        b"",
        std::time::Duration::from_secs(5),
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"foo\n999999999999999999999999999999999999999999999999999999\n"
    );
    assert_eq!(output.stderr, b"");
}

#[test]
fn shell_accepts_options_between_c_and_command() {
    let scratch = Scratch::new("shell-c-options");
    let output = offline_axe(&scratch)
        .args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "-u",
            "case $- in *u*) printf 'nounset\\n';; *) exit 9;; esac",
        ])
        .output()
        .expect("run shell command with nounset option");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"nounset\n");
    assert_eq!(output.stderr, b"");
}

#[cfg(unix)]
#[test]
fn shell_distinguishes_non_executable_and_missing_path_commands() {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = Scratch::new("shell-non-executable");
    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).expect("create PATH directory");
    let candidate = bin.join("blocked");
    fs::write(&candidate, b"#!/bin/sh\nprintf 'unexpected\\n'\n").expect("write PATH candidate");
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644))
        .expect("make PATH candidate non-executable");

    let output = offline_axe(&scratch)
        .env("PATH", &bin)
        .args(["--no-config", "--norc", "--noprofile", "-c", "blocked"])
        .output()
        .expect("run non-executable PATH candidate");

    assert_eq!(output.status.code(), Some(126));
    assert_eq!(output.stdout, b"");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Permission denied"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let missing = offline_axe(&scratch)
        .env("PATH", &bin)
        .args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "axe_definitely_missing_command",
        ])
        .output()
        .expect("run missing PATH command");
    assert_eq!(missing.status.code(), Some(127));
    assert_eq!(missing.stdout, b"");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("command not found"),
        "stderr: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

#[cfg(all(unix, feature = "bundled-coreutils"))]
#[test]
fn shell_builtin_kill_accepts_numeric_signal_and_job_spec() {
    let scratch = Scratch::new("shell-job-signal");
    let output = offline_axe(&scratch)
        .args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "timeout 5 sleep 10 & pid=$!; printf 'pid=%s\\n' \"$pid\"; kill -0 %1; printf 'alive=%s\\n' \"$?\"; kill -9 %1; printf 'sent=%s\\n' \"$?\"; wait \"$pid\"; printf 'wait=%s\\n' \"$?\"; kill -0 \"$pid\" >/dev/null 2>&1; printf 'dead=%s\\n' \"$?\"",
        ])
        .output()
        .expect("signal background job");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("job signal output is UTF-8");
    let mut lines = stdout.lines();
    let pid = lines
        .next()
        .and_then(|line| line.strip_prefix("pid="))
        .expect("background PID output");
    assert!(pid.parse::<u32>().is_ok(), "invalid background PID: {pid}");
    assert_eq!(
        lines.collect::<Vec<_>>(),
        ["alive=0", "sent=0", "wait=137", "dead=1"]
    );
    assert_eq!(output.stderr, b"");
}

#[cfg(feature = "bundled-coreutils")]
#[test]
fn shell_tracks_bundled_background_process_lifecycle() {
    let scratch = Scratch::new("process-lifecycle");
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "timeout 0.2 sleep 1 & pid=$!; printf 'pid=%s\\n' \"$pid\"; kill -0 \"$pid\"; printf 'alive=%s\\n' \"$?\"; wait \"$pid\"; printf 'wait=%s\\n' \"$?\"; kill -0 \"$pid\" >/dev/null 2>&1; printf 'dead=%s\\n' \"$?\"",
        ])
        .output()
        .expect("run background process lifecycle scenario");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("lifecycle output is UTF-8");
    let mut lines = stdout.lines();
    let pid = lines
        .next()
        .and_then(|line| line.strip_prefix("pid="))
        .expect("background PID output");
    assert!(pid.parse::<u32>().is_ok(), "invalid background PID: {pid}");
    assert_eq!(lines.collect::<Vec<_>>(), ["alive=0", "wait=124", "dead=1"]);
}

#[test]
fn shell_exposes_internal_background_jobs_as_waitable_job_specs() {
    let scratch = Scratch::new("internal-background-job");
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "(exit 7) & ref=$!; printf 'ref=%s\\n' \"$ref\"; wait \"$ref\"; printf 'wait=%s\\n' \"$?\"",
        ])
        .output()
        .expect("run internal background job scenario");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"ref=%1\nwait=7\n");
}

#[test]
fn xargs_replace_processes_one_logical_line_per_command() {
    let output = run_with_input(
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", "xargs", "--", "-I{}", "printf", "<%s>\\n", "{}"]),
        b"a b\nc d\n",
    );

    assert!(output.status.success());
    assert_eq!(output.stdout, b"<a b>\n<c d>\n");
}

#[test]
fn xargs_splits_input_at_the_system_argument_limit() {
    let input = b"argument1234\n".repeat(20_000);
    let output = run_with_input(
        Command::new(env!("CARGO_BIN_EXE_axe")).args(["--applet", "xargs", "--", "true"]),
        &input,
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn xargs_children_cannot_consume_the_input_stream() {
    let input = b"item\n".repeat(3_000);
    let output = run_with_input(
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", "xargs", "--", "-n100", "sh", "-c", "cat"]),
        &input,
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn applet_path_bridge_is_inherited_and_repairs_stale_state() {
    use std::os::unix::fs::{MetadataExt as _, symlink};

    const CHILD: &str = "AXE_TEST_PATH_BRIDGE_CHILD";
    const TEST_BINARY: &str = "AXE_TEST_BINARY";
    if std::env::var_os(CHILD).is_some() {
        let shell = PathBuf::from(std::env::var_os("SHELL").expect("SHELL is published"));
        let axe_shell =
            PathBuf::from(std::env::var_os("AXE_SHELL").expect("AXE_SHELL is published"));
        assert_eq!(shell, axe_shell);
        let shell_metadata = fs::metadata(&shell).expect("SHELL resolves to live Axe");
        let axe_metadata = fs::metadata(axe_shell).expect("AXE_SHELL resolves on filesystem");
        assert_eq!(
            (shell_metadata.dev(), shell_metadata.ino()),
            (axe_metadata.dev(), axe_metadata.ino())
        );
        let applet_dir =
            PathBuf::from(std::env::var_os("AXE_APPLET_DIR").expect("AXE_APPLET_DIR is published"));
        let target =
            fs::read_link(applet_dir.join("commands")).expect("read live applet bridge target");
        assert_eq!(target, shell);
        let target_metadata = fs::metadata(target).expect("applet bridge target resolves");
        assert_eq!(
            (target_metadata.dev(), target_metadata.ino()),
            (shell_metadata.dev(), shell_metadata.ino())
        );

        let output = Command::new("commands")
            .arg("printf")
            .output()
            .expect("resolve Axe applet through inherited PATH");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let scratch = Scratch::new("path-bridge");
    let test_binary = std::env::current_exe().expect("locate smoke test binary");
    let run_child = || {
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .env_clear()
            .env("HOME", scratch.path())
            .env("AXE_WORK_DIR", scratch.path())
            .env("PATH", "/nonexistent")
            .env(CHILD, "1")
            .env(TEST_BINARY, &test_binary)
            .args([
                "--norc",
                "--noprofile",
                "-c",
                "\"$AXE_TEST_BINARY\" --exact applet_path_bridge_is_inherited_and_repairs_stale_state --nocapture",
            ])
            .output()
            .expect("run arbitrary child through Axe shell")
    };

    let first = run_child();
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let bridge = fs::read_dir(scratch.path())
        .expect("read work directory")
        .map(|entry| entry.expect("read work directory entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".axe-"))
                && path.is_dir()
        })
        .expect("find generated applet bridge");
    let generation = fs::read_link(bridge.join("current")).expect("read current generation");
    let applet = bridge.join(generation).join("bin/commands");
    fs::remove_file(&applet).expect("remove generated applet link");
    symlink("/nonexistent", &applet).expect("corrupt generated applet link");

    let second = run_child();
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let generation = fs::read_link(bridge.join("current")).expect("read repaired generation");
    let target = fs::read_link(bridge.join(generation).join("bin/commands"))
        .expect("read repaired applet link");
    assert_eq!(
        target,
        PathBuf::from(env!("CARGO_BIN_EXE_axe"))
            .canonicalize()
            .expect("canonicalize AXE executable")
    );
}

#[cfg(target_os = "linux")]
fn install_hostile_exec_policy() -> std::io::Result<()> {
    const PR_SET_MDWE: libc::c_int = 65;
    const PR_MDWE_REFUSE_EXEC_GAIN: libc::c_ulong = 1;

    // Older kernels do not implement MDWE. Any other failure is a real policy
    // setup failure; a supported kernel must carry the restriction across exec.
    let mdwe = unsafe {
        libc::prctl(
            PR_SET_MDWE,
            PR_MDWE_REFUSE_EXEC_GAIN,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if mdwe == -1 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error);
        }
    }

    const fn statement(code: u16, value: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k: value,
        }
    }

    const fn jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: yes,
            jf: no,
            k: value,
        }
    }

    let mut filter = [
        statement((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 0),
        jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            libc::SYS_execveat as u32,
            0,
            1,
        ),
        statement(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
        ),
        jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            libc::SYS_memfd_create as u32,
            0,
            1,
        ),
        statement(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        ),
        jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            libc::SYS_flock as u32,
            0,
            1,
        ),
        statement(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        ),
        statement(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ALLOW,
        ),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };

    // SAFETY: prctl copies the bounded filter before returning. no_new_privs
    // permits an unprivileged process to install this monotonic seccomp policy.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: program points to the live filter array for the duration of the
    // call; the kernel validates and copies every instruction.
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &program as *const libc::sock_fprog,
        )
    } == -1
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_spawn_denial_policy() -> std::io::Result<()> {
    const fn statement(code: u16, value: u32) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: 0,
            jf: 0,
            k: value,
        }
    }

    const fn jump(code: u16, value: u32, yes: u8, no: u8) -> libc::sock_filter {
        libc::sock_filter {
            code,
            jt: yes,
            jf: no,
            k: value,
        }
    }

    let mut filter = [
        statement((libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16, 0),
        jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            libc::SYS_clone as u32,
            2,
            0,
        ),
        jump(
            (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            libc::SYS_clone3 as u32,
            1,
            0,
        ),
        statement(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ALLOW,
        ),
        statement(
            (libc::BPF_RET | libc::BPF_K) as u16,
            libc::SECCOMP_RET_ERRNO | libc::EAGAIN as u32,
        ),
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };

    // SAFETY: prctl copies the bounded filter before returning. no_new_privs
    // permits an unprivileged process to install this monotonic seccomp policy.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: program points to the live filter array for the duration of the
    // call; the kernel validates and copies every instruction.
    if unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &program as *const libc::sock_fprog,
        )
    } == -1
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn no_proc_uses_filesystem_reexec_and_cleans_unusable_bridge() {
    const NAMESPACE_CHILD: &str = "AXE_TEST_NO_PROC_NAMESPACE";
    const ASSERT_CHILD: &str = "AXE_TEST_NO_PROC_ASSERT";
    const EXPECT_NO_BRIDGE: &str = "AXE_TEST_NO_PROC_EXPECT_NO_BRIDGE";
    const AXE_EXECUTABLE: &str = "AXE_TEST_NO_PROC_EXECUTABLE";
    const TEST_BINARY: &str = "AXE_TEST_NO_PROC_TEST_BINARY";
    const ROOT: &str = "AXE_TEST_NO_PROC_ROOT";
    const STALE_BRIDGE: &str = "AXE_TEST_NO_PROC_STALE_BRIDGE";
    const POLICY_CHILD: &str = "AXE_TEST_HOSTILE_EXEC_POLICY";
    const POLICY_SCRIPT: &str = "AXE_TEST_HOSTILE_EXEC_SCRIPT";
    const TEST_NAME: &str = "no_proc_uses_filesystem_reexec_and_cleans_unusable_bridge";

    if std::env::var_os(POLICY_CHILD).is_some() {
        use std::os::unix::process::CommandExt as _;

        install_hostile_exec_policy().expect("install hostile execution policy");
        let axe =
            PathBuf::from(std::env::var_os(AXE_EXECUTABLE).expect("policy AXE path is inherited"));
        let script = std::env::var_os(POLICY_SCRIPT).expect("policy script is inherited");
        let error = Command::new(axe)
            .env_remove(POLICY_CHILD)
            .args(["--no-config", "--norc", "--noprofile", "-c"])
            .arg(script)
            .exec();
        panic!("exec policy-constrained AXE: {error}");
    }

    if std::env::var_os(ASSERT_CHILD).is_some() {
        let paths: Vec<PathBuf> =
            std::env::split_paths(&std::env::var_os("PATH").expect("PATH is inherited")).collect();

        assert_eq!(
            std::env::var_os("AXE").as_deref(),
            Some(std::ffi::OsStr::new("true"))
        );

        if std::env::var_os(EXPECT_NO_BRIDGE).is_some() {
            let stale = PathBuf::from(
                std::env::var_os(STALE_BRIDGE).expect("stale bridge path is inherited"),
            );
            assert_eq!(std::env::var_os("AXE_APPLET_DIR"), None);
            assert!(!paths.contains(&stale));
        } else {
            let shell = PathBuf::from(std::env::var_os("SHELL").expect("SHELL is published"));
            let axe_shell =
                PathBuf::from(std::env::var_os("AXE_SHELL").expect("AXE_SHELL is published"));
            let bridge = PathBuf::from(
                std::env::var_os("AXE_APPLET_DIR").expect("AXE_APPLET_DIR is published"),
            );

            assert_eq!(shell, axe_shell);
            assert!(shell.exists(), "published shell path must exist");
            assert_eq!(paths.first(), Some(&bridge));
            assert_eq!(paths.iter().filter(|path| *path == &bridge).count(), 1);
            for name in ["axe", "sort", "xargs"] {
                let target =
                    fs::read_link(bridge.join(name)).expect("read filesystem bridge symlink");
                assert_eq!(target, shell);
                assert!(!target.starts_with("/proc"));
            }
        }
        return;
    }

    if std::env::var_os(NAMESPACE_CHILD).is_some() {
        let source = b"tmpfs\0";
        let target = b"/proc\0";
        let filesystem = b"tmpfs\0";
        // SAFETY: all strings are NUL-terminated and live through the call;
        // tmpfs accepts a null data pointer, and the child owns its mount namespace.
        let mounted = unsafe {
            libc::mount(
                source.as_ptr().cast(),
                target.as_ptr().cast(),
                filesystem.as_ptr().cast(),
                0,
                std::ptr::null::<libc::c_void>(),
            )
        };
        assert_eq!(
            mounted,
            0,
            "mount empty /proc: {}",
            std::io::Error::last_os_error()
        );

        let axe = PathBuf::from(std::env::var_os(AXE_EXECUTABLE).expect("AXE path is inherited"));
        let test_binary =
            PathBuf::from(std::env::var_os(TEST_BINARY).expect("test binary path is inherited"));
        let root = PathBuf::from(std::env::var_os(ROOT).expect("scratch path is inherited"));
        let valid_work = root.join("valid");
        fs::create_dir_all(&valid_work).expect("create valid AXE work directory");
        let assertion = format!(
            "\"{}\" --exact {TEST_NAME} --nocapture >/dev/null 2>&1 || exit $?; \\
             printf 'b\\na\\na\\n' | sort | uniq; \\
             printf 'x\\n' | xargs -n1 printf '<%s>\\n'",
            test_binary.display()
        );
        let valid = Command::new(&axe)
            .env_clear()
            .env("HOME", &root)
            .env("PATH", "/nonexistent")
            .env("AXE_WORK_DIR", &valid_work)
            .env("AXE_STORE_DIR", root.join("store"))
            .env("AXE_STORE_URL", "http://127.0.0.1:9")
            .env("AXE_STORE_ADDRESSES", "127.0.0.1")
            .env(ASSERT_CHILD, "1")
            .env(AXE_EXECUTABLE, &axe)
            .env(TEST_BINARY, &test_binary)
            .args(["--no-config", "--norc", "--noprofile", "-c", &assertion])
            .output()
            .expect("run proc-free filesystem fallback");
        assert!(
            valid.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&valid.stderr)
        );
        assert_eq!(valid.stdout, b"a\nb\n<x>\n");
        let direct_blocker = root.join("direct-blocker");
        fs::write(&direct_blocker, b"not a directory").expect("create direct relay blocker");
        let direct_axe = root.join("direct-axe");
        fs::copy(&axe, &direct_axe).expect("copy AXE for direct descriptor execution");
        let direct = Command::new(&direct_axe)
            .env_clear()
            .env("HOME", &root)
            .env("PATH", "/nonexistent")
            .env("AXE_WORK_DIR", direct_blocker.join("work"))
            .env("AXE_STORE_DIR", root.join("store"))
            .env("AXE_STORE_URL", "http://127.0.0.1:9")
            .env("AXE_STORE_ADDRESSES", "127.0.0.1")
            .args([
                "--no-config",
                "--norc",
                "--noprofile",
                "-c",
                "rm \"$AXE_SHELL\"; printf 'b\\na\\n' | sort",
            ])
            .output()
            .expect("run unlinked AXE through direct descriptor");
        assert!(
            direct.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&direct.stderr)
        );
        assert_eq!(direct.stdout, b"a\nb\n");
        assert!(!direct_axe.exists());

        let policy_work = root.join("policy");
        fs::create_dir(&policy_work).expect("create policy work directory");
        let policy_axe = root.join("policy-axe");
        fs::copy(&axe, &policy_axe).expect("copy AXE for policy fallback");
        let policy = Command::new(&test_binary)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(POLICY_CHILD, "1")
            .env(
                POLICY_SCRIPT,
                "printf 'b\\na\\na\\n' | sort | uniq; printf 'x\\n' | xargs -n1 printf '<%s>\\n'",
            )
            .env(AXE_EXECUTABLE, &policy_axe)
            .env("HOME", &root)
            .env("PATH", "/nonexistent")
            .env("AXE_WORK_DIR", &policy_work)
            .env("AXE_STORE_DIR", root.join("store"))
            .env("AXE_STORE_URL", "http://127.0.0.1:9")
            .env("AXE_STORE_ADDRESSES", "127.0.0.1")
            .output()
            .expect("run AXE with descriptor syscalls denied");
        assert!(
            policy.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&policy.stderr)
        );
        assert!(policy.stdout.ends_with(b"a\nb\n<x>\n"));
        assert!(
            policy_work.join(".axe-self").exists(),
            "unknown mount policy without procfs must prefer a checked relay"
        );

        let degraded_blocker = root.join("degraded-blocker");
        fs::write(&degraded_blocker, b"not a directory").expect("create degraded relay blocker");
        let degraded_axe = root.join("degraded-axe");
        fs::copy(&axe, &degraded_axe).expect("copy AXE for controlled degradation");
        let degraded = Command::new(&test_binary)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(POLICY_CHILD, "1")
            .env(
                POLICY_SCRIPT,
                "rm \"$AXE_SHELL\"; sort </dev/null; printf 'sort=%s\\nalive\\n' \"$?\"",
            )
            .env(AXE_EXECUTABLE, &degraded_axe)
            .env("HOME", &root)
            .env("PATH", "/nonexistent")
            .env("AXE_WORK_DIR", degraded_blocker.join("work"))
            .env("AXE_STORE_DIR", root.join("store"))
            .env("AXE_STORE_URL", "http://127.0.0.1:9")
            .env("AXE_STORE_ADDRESSES", "127.0.0.1")
            .output()
            .expect("run AXE without descriptor or relay execution");
        assert!(
            degraded.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&degraded.stderr)
        );
        assert!(degraded.stdout.ends_with(b"sort=126\nalive\n"));
        assert!(!degraded_axe.exists());

        let probe = Command::new(&axe)
            .env_clear()
            .env("HOME", &root)
            .env("PATH", "/nonexistent")
            .env("AXE_WORK_DIR", &valid_work)
            .args([
                "--applet",
                "doctor",
                "--",
                "--json",
                "--path",
                valid_work.to_str().expect("UTF-8 test path"),
            ])
            .output()
            .expect("run proc-free doctor");
        assert!(
            probe.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        let probe: serde_json::Value =
            serde_json::from_slice(&probe.stdout).expect("parse doctor report");
        assert_eq!(probe["schema"], "axe_doctor");
        assert_eq!(probe["schema_version"], 3);
        assert_eq!(probe["launch"]["bridge"]["state"], "not_attempted");
        assert_eq!(
            probe["capabilities"]["kernel"]["procfs"]["status"],
            "available"
        );
        assert_eq!(probe["capabilities"]["kernel"]["procfs"]["value"], false);
        assert_eq!(
            probe["capabilities"]["process"]["self_exec"]["observation"]["value"]["success"],
            true
        );
        assert_eq!(probe["launch"]["relay"]["status"], "available");
        assert!(
            probe["launch"]["candidates"]
                .as_array()
                .expect("launch candidates")
                .iter()
                .any(|candidate| {
                    candidate["kind"] == "lazy_relay" && candidate["usable"]["value"] == true
                }),
            "final launch snapshot must include the relay materialized by active probes"
        );
        assert_eq!(
            probe["filesystem"]["mount_execution_policy"]["status"],
            "unknown"
        );
        assert!(
            probe["filesystem"].get("chmod_file").is_none()
                && probe["filesystem"].get("execute_file").is_none(),
            "removed executable-file probe leaked into the schema: {}",
            probe["filesystem"]
        );
        assert_eq!(probe["filesystem"]["cleanup"]["value"], true);

        let blocker = root.join("blocker");
        fs::write(&blocker, b"not a directory").expect("create workdir blocker");
        let unusable_work = blocker.join("work");
        let stale = root.join("stale/bin");
        let stale_path =
            std::env::join_paths([stale.as_path(), Path::new("/nonexistent")]).expect("join PATH");
        let unusable = Command::new(&axe)
            .env_clear()
            .env("HOME", &root)
            .env("PATH", stale_path)
            .env("AXE_APPLET_DIR", &stale)
            .env("AXE_WORK_DIR", &unusable_work)
            .env("AXE_STORE_DIR", root.join("store"))
            .env("AXE_STORE_URL", "http://127.0.0.1:9")
            .env("AXE_STORE_ADDRESSES", "127.0.0.1")
            .env(ASSERT_CHILD, "1")
            .env(EXPECT_NO_BRIDGE, "1")
            .env(STALE_BRIDGE, &stale)
            .env(AXE_EXECUTABLE, &axe)
            .env(TEST_BINARY, &test_binary)
            .args(["--no-config", "--norc", "--noprofile", "-c", &assertion])
            .output()
            .expect("run with unusable bridge root");
        assert!(
            unusable.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&unusable.stderr)
        );
        assert_eq!(unusable.stdout, b"a\nb\n<x>\n");

        let ssh_work = root.join("ssh");
        fs::create_dir(&ssh_work).expect("create SSH work directory");
        let reservation =
            std::net::TcpListener::bind("127.0.0.1:0").expect("reserve SSH listen address");
        let address = reservation.local_addr().expect("read SSH listen address");
        drop(reservation);
        let child = Command::new(&axe)
            .env_clear()
            .env("HOME", &root)
            .env("PATH", "/nonexistent")
            .arg("--applet")
            .arg("sshd")
            .arg("--")
            .arg("--listen")
            .arg(address.to_string())
            .arg("--workdir")
            .arg(&ssh_work)
            .args(["--store-mode", "off", "--no-relay"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start proc-free sshd");
        let sshd = stop_process_after_ready(child, address);
        assert!(
            sshd.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&sshd.stderr)
        );
        assert!(
            String::from_utf8_lossy(&sshd.stdout).contains("sshd: serving on"),
            "stdout: {}",
            String::from_utf8_lossy(&sshd.stdout)
        );
        let bridge = fs::read_dir(&ssh_work)
            .expect("read SSH work directory")
            .map(|entry| entry.expect("read SSH bridge entry").path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".axe-"))
                    && path.is_dir()
                    && fs::symlink_metadata(path.join("current/bin/axe")).is_ok()
            })
            .expect("find SSH applet bridge");
        let target = fs::read_link(bridge.join("current/bin/axe"))
            .expect("read SSH filesystem bridge target");
        assert!(target.exists(), "SSH bridge target must exist");
        assert_ne!(target, axe);
        assert!(
            target
                .components()
                .any(|component| component.as_os_str() == ".axe-self")
        );
        assert!(!target.starts_with("/proc"));
        return;
    }

    let scratch = Scratch::new("no-proc");
    let test_binary = std::env::current_exe()
        .expect("locate smoke test binary")
        .canonicalize()
        .expect("canonicalize smoke test binary");
    let axe = std::env::var_os(AXE_EXECUTABLE)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_axe")))
        .canonicalize()
        .expect("canonicalize AXE executable");
    let output = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--pid",
            "--fork",
            "--",
        ])
        .arg(&test_binary)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(NAMESPACE_CHILD, "1")
        .env(AXE_EXECUTABLE, &axe)
        .env(TEST_BINARY, &test_binary)
        .env(ROOT, scratch.path())
        .output()
        .expect("start proc-free user namespace");
    if !output.status.success()
        && String::from_utf8_lossy(&output.stderr).contains("Operation not permitted")
    {
        eprintln!(
            "proc-free namespace proof skipped: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return;
    }
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn inherited_runtime_root_stabilizes_child_selection() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;

    let scratch = Scratch::new("inherited-runtime-root");
    let inherited_root = scratch.path().join("inherited");
    fs::create_dir(&inherited_root).expect("create inherited runtime root");
    fs::set_permissions(&inherited_root, fs::Permissions::from_mode(0o700))
        .expect("make inherited runtime root private");
    let cache = scratch.path().join("different-cache");
    let executable = fs::File::open(env!("CARGO_BIN_EXE_axe")).expect("open AXE executable");
    let descriptor = executable.as_raw_fd();
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .env_clear()
        .env("HOME", scratch.path().join("different-home"))
        .env("XDG_CACHE_HOME", &cache)
        .env("PATH", "/nonexistent")
        .env("AXE_STORE_MODE", "off")
        .env("__AXE_EXECUTABLE_FD", descriptor.to_string())
        .env("__AXE_RUNTIME_ROOT", &inherited_root)
        .args(["--norc", "--noprofile", "-c", "printf 'b\\na\\n' | sort"]);
    // SAFETY: the callback only queries and updates flags on the live executable
    // descriptor retained by this scope.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(descriptor, libc::F_GETFD);
            if flags == -1
                || libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let output = command
        .output()
        .expect("run AXE with inherited runtime root");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"a\nb\n");
    assert!(
        fs::read_dir(&inherited_root)
            .expect("read inherited runtime root")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".axe-"))
    );
    assert!(
        !cache.join("axe").exists(),
        "child recomputed a different automatic runtime root"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn descriptor_denial_refreshes_original_fallback_to_relay() {
    use std::os::unix::process::CommandExt as _;

    const CHILD: &str = "AXE_TEST_DESCRIPTOR_DENIAL_CHILD";
    const AXE_EXECUTABLE: &str = "AXE_TEST_DESCRIPTOR_DENIAL_EXECUTABLE";
    const POLICY_SCRIPT: &str = "AXE_TEST_DESCRIPTOR_DENIAL_SCRIPT";
    const TEST_NAME: &str = "descriptor_denial_refreshes_original_fallback_to_relay";

    if std::env::var_os(CHILD).is_some() {
        install_hostile_exec_policy().expect("install hostile execution policy");
        let axe =
            PathBuf::from(std::env::var_os(AXE_EXECUTABLE).expect("policy AXE path is inherited"));
        let script = std::env::var_os(POLICY_SCRIPT).expect("policy script is inherited");
        let error = Command::new(axe)
            .env_remove(CHILD)
            .args(["--no-config", "--norc", "--noprofile", "-c"])
            .arg(script)
            .exec();
        panic!("exec policy-constrained AXE: {error}");
    }

    let scratch = Scratch::new("descriptor-denial-refresh");
    let test_binary = std::env::current_exe().expect("locate smoke test binary");
    let run_case = |label: &str, script: &str| {
        let work = scratch.path().join(format!("{label}-work"));
        fs::create_dir(&work).expect("create policy work directory");
        let axe = scratch.path().join(format!("{label}-axe"));
        fs::copy(env!("CARGO_BIN_EXE_axe"), &axe).expect("copy AXE executable");
        let output = Command::new(&test_binary)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD, "1")
            .env(AXE_EXECUTABLE, &axe)
            .env(POLICY_SCRIPT, script)
            .env("HOME", scratch.path())
            .env("PATH", "/nonexistent")
            .env("AXE_WORK_DIR", &work)
            .env("AXE_STORE_DIR", scratch.path().join("store"))
            .env("AXE_STORE_URL", "http://127.0.0.1:9")
            .env("AXE_STORE_ADDRESSES", "127.0.0.1")
            .output()
            .expect("run AXE with descriptor execution denied");
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (work, axe, output)
    };

    let (original_work, original_axe, original) =
        run_case("original", "sort </dev/null; printf 'alive\\n'");
    assert!(original.stdout.ends_with(b"alive\n"));
    assert!(original_axe.exists());
    assert!(
        !original_work.join(".axe-self").exists(),
        "supported original path must not eagerly materialize a relay"
    );

    let (relay_work, relay_axe, relay) = run_case(
        "relay",
        "rm \"$AXE_SHELL\"; sort </dev/null; printf 'alive\\n'",
    );
    assert!(relay.stdout.ends_with(b"alive\n"));
    assert!(!relay_axe.exists());
    assert!(
        relay_work.join(".axe-self").exists(),
        "provider must materialize a relay after the original path disappears"
    );

    let relays = fs::read_dir(relay_work.join(".axe-self"))
        .expect("read relay directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.strip_prefix("nolock-").is_some_and(|build_id| {
                    build_id.len() == 40 && build_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
        })
        .count();
    assert_eq!(relays, 1, "one build-ID-addressed relay must be retained");
}

#[cfg(target_os = "linux")]
#[test]
fn spawn_denial_preserves_brush_control_plane() {
    use std::os::unix::process::CommandExt as _;

    const CHILD: &str = "AXE_TEST_SPAWN_DENIAL_CHILD";
    const AXE_EXECUTABLE: &str = "AXE_TEST_SPAWN_DENIAL_EXECUTABLE";
    const TEST_NAME: &str = "spawn_denial_preserves_brush_control_plane";

    if std::env::var_os(CHILD).is_some() {
        install_spawn_denial_policy().expect("install spawn denial policy");
        let axe = PathBuf::from(std::env::var_os(AXE_EXECUTABLE).expect("AXE path is inherited"));
        let error = Command::new(axe)
            .env_remove(CHILD)
            .args([
                "--no-config",
                "--norc",
                "--noprofile",
                "-c",
                "sort </dev/null; doctor --json >\"$AXE_WORK_DIR/doctor.json\"; doctor >\"$AXE_WORK_DIR/doctor.txt\"; printf 'alive\\n'",
            ])
            .exec();
        panic!("exec spawn-constrained AXE: {error}");
    }

    let scratch = Scratch::new("spawn-denial");
    let test_binary = std::env::current_exe().expect("locate smoke test binary");
    let output = Command::new(test_binary)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD, "1")
        .env(AXE_EXECUTABLE, env!("CARGO_BIN_EXE_axe"))
        .env("HOME", scratch.path())
        .env("AXE_WORK_DIR", scratch.path())
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .env("AXE_STORE_URL", "http://127.0.0.1:9")
        .env("AXE_STORE_ADDRESSES", "127.0.0.1")
        .env("PATH", "/nonexistent")
        .output()
        .expect("run AXE with process creation denied");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.ends_with(b"alive\n"));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Resource temporarily unavailable"),
        "spawn failure must be reported: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = fs::read(scratch.path().join("doctor.json")).expect("read native doctor report");
    let report: serde_json::Value =
        serde_json::from_slice(&report).expect("parse native doctor report");
    assert_eq!(report["scope"], "shell");
    assert_eq!(
        report["capabilities"]["process"]["fork"]["observation"]["status"],
        "unavailable"
    );
    assert_eq!(
        report["capabilities"]["process"]["self_exec"]["observation"]["status"],
        "unavailable"
    );
    let degradations = report["degradations"]
        .as_array()
        .expect("doctor degradations are an array");
    let bundled_degradation = degradations
        .iter()
        .find(|entry| entry["component"] == "bundled_command")
        .unwrap_or_else(|| panic!("failed bundled launch was not preserved: {degradations:?}"));
    assert_eq!(bundled_degradation["command"], "sort");
    assert_eq!(bundled_degradation["operation"], "launch bundled child");
    assert_eq!(bundled_degradation["failure"]["code"], "would_block");
    assert_eq!(bundled_degradation["failure"]["errno"], libc::EAGAIN);
    assert_eq!(bundled_degradation["recovered"], false);
    assert!(
        degradations
            .iter()
            .any(|entry| entry["component"] == "process" && entry["operation"] == "fork"),
        "active fork failure was not preserved: {degradations:?}"
    );
    let human =
        fs::read_to_string(scratch.path().join("doctor.txt")).expect("read native doctor output");
    assert!(
        human.contains("Attention\n")
            && human.contains("Command: sort")
            && human.contains("Child AXE processes may not start.")
            && human.contains("External child processes may not start."),
        "negative active results must explain runtime impact: {human}"
    );
}

#[test]
fn gzip_removes_partial_output_after_invalid_input() {
    let scratch = Scratch::new("gzip-partial");
    fs::write(scratch.path().join("broken.gz"), b"not a gzip stream")
        .expect("write invalid gzip input");

    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .current_dir(scratch.path())
        .args(["--applet", "gzip", "--", "-d", "broken.gz"])
        .output()
        .expect("run gzip");

    assert!(!output.status.success());
    assert!(scratch.path().join("broken.gz").is_file());
    assert!(!scratch.path().join("broken").exists());
}

#[cfg(unix)]
#[test]
fn shell_avoids_history_writes_and_survives_unwritable_storage() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("history");
    let store = Scratch::new("history-store");
    let history = scratch.path().join("forced-history");
    let output = run_with_input(
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .current_dir(scratch.path())
            .env_clear()
            .env("HOME", scratch.path())
            .env("HISTFILE", &history)
            .env("AXE_STORE_DIR", store.path())
            .env("PATH", "/nonexistent")
            .env("TERM", "dumb")
            .args(["--no-config", "--norc", "--noprofile", "-i"]),
        b"printf stable\nexit\n",
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"stable");
    assert!(!history.exists(), "shell persisted command history");

    let readonly = scratch.path().join("readonly");
    fs::create_dir(&readonly).expect("create read-only home");
    let unavailable_workdir = readonly.join("not-a-directory");
    fs::write(&unavailable_workdir, b"fixture").expect("create unusable work directory");
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o555)).expect("make home read-only");
    let output = run_with_input(
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .current_dir(&readonly)
            .env_clear()
            .env("HOME", &readonly)
            .env("AXE_WORK_DIR", &unavailable_workdir)
            .env("AXE_STORE_DIR", store.path())
            .env("HISTFILE", "/dev/full")
            .env("PATH", "/nonexistent")
            .env("TERM", "dumb")
            .args(["--no-config", "--norc", "--noprofile", "-i"]),
        b"printf resilient\nexit\n",
    );
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o755))
        .expect("restore home permissions");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"resilient");
    let entries = fs::read_dir(&readonly)
        .expect("read restored home")
        .map(|entry| entry.expect("read home entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        [unavailable_workdir
            .file_name()
            .expect("fixture has a file name")]
    );
}

#[cfg(unix)]
fn stop_process_after_ready(
    mut child: std::process::Child,
    address: std::net::SocketAddr,
) -> std::process::Output {
    use std::net::TcpStream;
    use std::thread;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect(address).is_ok() {
            break;
        }
        if let Some(status) = child.try_wait().expect("poll process") {
            panic!("process exited before listening: {status}");
        }
        assert!(Instant::now() < deadline, "process did not start");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        // SAFETY: child.id() identifies the live process started above.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0,
        "send SIGTERM"
    );

    let shutdown_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().expect("poll process shutdown").is_some() {
            break;
        }
        assert!(
            Instant::now() < shutdown_deadline,
            "process did not exit after SIGTERM"
        );
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().expect("collect sshd output")
}

#[cfg(unix)]
#[test]
fn sshd_daemon_fails_when_worker_cannot_bind() {
    use std::net::TcpListener;

    let scratch = Scratch::new("sshd-daemon-bind-failure");
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve SSH port");
    let address = reservation.local_addr().expect("read reserved address");
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .args([
            "--applet",
            "sshd",
            "--",
            "--listen",
            &address.to_string(),
            "--workdir",
        ])
        .arg(scratch.path())
        .args(["--store-mode", "off", "--no-relay", "--daemon"])
        .output()
        .expect("start daemon with occupied listener");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("did not become ready"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn sshd_daemon_is_ready_on_return_and_stops_with_supervisor() {
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    struct DaemonGuard(libc::pid_t);

    impl Drop for DaemonGuard {
        fn drop(&mut self) {
            if self.0 != 0 {
                // SAFETY: the daemon creates a session whose process-group ID is
                // the reported supervisor PID.
                unsafe { libc::kill(-self.0, libc::SIGKILL) };
            }
        }
    }

    let scratch = Scratch::new("sshd-daemon-lifecycle");
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve SSH port");
    let address = reservation.local_addr().expect("read reserved address");
    drop(reservation);
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("AXE_STORE_DIR", scratch.path().join("store"))
        .args([
            "--applet",
            "sshd",
            "--",
            "--listen",
            &address.to_string(),
            "--workdir",
        ])
        .arg(scratch.path())
        .args(["--store-mode", "off", "--no-relay", "--daemon"])
        .output()
        .expect("start SSH daemon");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("daemon output is UTF-8");
    let pid = stdout
        .split_whitespace()
        .next_back()
        .and_then(|value| value.parse::<libc::pid_t>().ok())
        .expect("daemon output ends with supervisor PID");
    let mut guard = DaemonGuard(pid);

    TcpStream::connect(address).expect("daemon is accepting when launcher returns");
    // SAFETY: the parsed PID identifies the live supervisor.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect(address).is_err() {
            guard.0 = 0;
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon worker survived supervisor"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn sshd_starts_without_writable_storage() {
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("sshd-readonly");
    let store = Scratch::new("sshd-readonly-store");
    fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o555))
        .expect("make sshd workdir read-only");
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve SSH port");
    let address = reservation.local_addr().expect("read reserved address");
    drop(reservation);

    let child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("HISTFILE", "/dev/full")
        .env("AXE_STORE_DIR", store.path())
        .args([
            "--applet",
            "sshd",
            "--",
            "--listen",
            &address.to_string(),
            "--workdir",
        ])
        .arg(scratch.path())
        .args(["--store-mode", "off", "--no-relay"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start sshd");
    let output = stop_process_after_ready(child, address);
    fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o755))
        .expect("restore workdir permissions");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // sshd may publish its applet bridge when the filesystem still allows it
    // (permission enforcement varies across mount namespaces), but it must
    // not leave any other state behind in a read-only workdir.
    let unexpected: Vec<String> = fs::read_dir(scratch.path())
        .expect("read restored workdir")
        .filter_map(|entry| {
            let entry = entry.expect("read workdir entry");
            let valid_bridge = entry.file_name().to_string_lossy().starts_with(".axe-")
                && entry.path().join("current/bin").is_dir();
            (!valid_bridge).then(|| entry.path().display().to_string())
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "sshd left unexpected state in its workdir: {unexpected:?}"
    );
}

#[cfg(unix)]
#[test]
fn sshd_selects_workdir_and_store_once_when_unspecified() {
    use std::net::TcpListener;

    let scratch = Scratch::new("sshd-auto-workdir");
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve SSH port");
    let address = reservation.local_addr().expect("read reserved address");
    drop(reservation);

    let child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("AXE_STORE_URL", "http://127.0.0.1:9")
        .env("AXE_STORE_ADDRESSES", "127.0.0.1")
        .args([
            "--applet",
            "sshd",
            "--",
            "--listen",
            &address.to_string(),
            "--no-relay",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start sshd");
    let output = stop_process_after_ready(child, address);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        scratch.path().join(".cache/axe/.axe-store").is_dir(),
        "sshd did not prepare AXE Store inside its selected workdir"
    );
}

#[cfg(unix)]
#[test]
fn sshd_publishes_selected_path_bridge_reused_by_shells() {
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::{Duration, Instant};

    let scratch = Scratch::new("sshd-bridge");
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve SSH port");
    let address = reservation.local_addr().expect("read reserved address");
    drop(reservation);

    let child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .args([
            "--applet",
            "sshd",
            "--",
            "--listen",
            &address.to_string(),
            "--workdir",
        ])
        .arg(scratch.path())
        .args(["--store-mode", "off", "--no-relay"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start sshd");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if TcpStream::connect(address).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "sshd did not start");
        thread::sleep(Duration::from_millis(10));
    }

    let base = fs::read_dir(scratch.path())
        .expect("read sshd workdir")
        .filter_map(|entry| entry.ok())
        .find(|entry| {
            entry.file_name().to_string_lossy().starts_with(".axe-")
                && entry.file_name() != ".axe-store"
        })
        .expect("sshd published an applet bridge")
        .path();
    let bin = base.join("current/bin");
    assert_eq!(
        fs::read_link(bin.join("axe")).expect("read bridge axe symlink"),
        PathBuf::from(env!("CARGO_BIN_EXE_axe"))
            .canonicalize()
            .expect("canonicalize AXE executable")
    );
    assert!(
        !bin.join("curl").exists(),
        "disabled Store command was published into the SSH bridge"
    );
    assert!(
        !scratch.path().join(".axe-store").exists()
            && !scratch.path().join(".cache/axe/.axe-store").exists(),
        "disabled Store initialized SSH storage"
    );

    let count_generations = || {
        fs::read_dir(base.join("generations"))
            .expect("read bridge generations")
            .count()
    };
    assert_eq!(count_generations(), 1, "sshd published one generation");

    // A shell started with the bridge marker must reuse it: PATH is prepended
    // and no second generation is published.
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .env("AXE_APPLET_DIR", &bin)
        .args([
            "--norc",
            "--noprofile",
            "--no-config",
            "-c",
            "printf %s \"$PATH\"",
        ])
        .output()
        .expect("run shell with bridge marker");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = String::from_utf8(output.stdout).expect("PATH output is UTF-8");
    let bin_string = bin.to_string_lossy();
    assert!(
        path.starts_with(&*bin_string),
        "PATH {path} does not start with {bin_string}"
    );
    assert_eq!(
        count_generations(),
        1,
        "shell with marker must not republish the bridge"
    );

    // BusyBox-style dispatch with a bare argv[0] resolves through the bridge
    // symlinks, exercising the inode-based managed-applet check.
    {
        use std::os::unix::process::CommandExt as _;
        let output = Command::new(bin.join("ls"))
            .env_clear()
            .env("HOME", scratch.path())
            .env("PATH", "/nonexistent")
            .env("AXE_APPLET_DIR", &bin)
            .arg0("ls")
            .arg("--version")
            .output()
            .expect("run applet through bridge");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output = stop_process_after_ready(child, address);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("applet PATH bridge unavailable"),
        "sshd reported bridge errors: {stderr}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unlinked_binary_still_launches_embedded_shell() {
    use std::os::fd::AsRawFd;

    let scratch = Scratch::new("unlinked-shell");
    let copy = scratch.path().join("axe");
    fs::copy(env!("CARGO_BIN_EXE_axe"), &copy).expect("copy axe binary");
    let executable = fs::File::open(&copy).expect("retain axe executable");
    fs::remove_file(&copy).expect("unlink axe binary");

    let output = Command::new(format!("/proc/self/fd/{}", executable.as_raw_fd()))
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .args([
            "--no-config",
            "--norc",
            "--noprofile",
            "-c",
            "printf 'b\\na\\na\\n' | sort | uniq",
        ])
        .output()
        .expect("launch Brush through retained executable");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"a\nb\n");
}

#[cfg(all(feature = "applet-jq", feature = "on-demand"))]
#[test]
fn commands_is_queryable_with_bundled_jq_inside_shell() {
    let scratch = Scratch::new("commands");
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", scratch.path())
        .env("PATH", "/nonexistent")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "commands | jq -c '{schema, schema_version, keys: (keys | sort), has_legacy_name: any(.commands[]; .name == \"axe-info\"), missing_synopses: [.commands[] | select((.synopsis | type) != \"string\" or (.synopsis | length) == 0) | .name], invalid_availability: [.commands[] | select(.availability != \"local\" and .availability != \"on_demand\" and .availability != \"blocked\") | .name], examples: ([.commands[] | select(.name == \"doctor\" or .name == \"goblin\" or .name == \"jq\") | {name, source, category, synopsis, availability, local_path_type: (.local_path | type)}] | sort_by(.name))}'",
        ])
        .output()
        .expect("query commands from the bundled shell");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let actual: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("commands output is JSON");
    assert_eq!(
        actual,
        serde_json::json!({
            "schema": "axe_commands",
            "schema_version": 1,
            "keys": ["commands", "schema", "schema_version"],
            "has_legacy_name": false,
            "missing_synopses": [],
            "invalid_availability": [],
            "examples": [
                {
                    "name": "doctor",
                    "source": "bundled",
                    "category": "control",
                    "synopsis": "Diagnose the current AXE runtime, shell, isolation, and restrictions",
                    "availability": "local",
                    "local_path_type": "string"
                },
                {
                    "name": "goblin",
                    "source": "bundled",
                    "category": "binary",
                    "synopsis": "Inspect ELF, PE, Mach-O, and archive binaries",
                    "availability": "local",
                    "local_path_type": "string"
                },
                {
                    "name": "jq",
                    "source": "bundled",
                    "category": "data",
                    "synopsis": "Process and transform JSON data",
                    "availability": "local",
                    "local_path_type": "string"
                },
            ]
        })
    );
}

#[cfg(target_os = "linux")]
#[test]
fn doctor_reports_readable_shell_state_and_selected_environment() {
    let scratch = Scratch::new("doctor");
    let home = scratch.path().join("home");
    let work = scratch.path().join("work");
    let inner = work.join("inner");
    fs::create_dir(&home).expect("create isolated home");
    fs::create_dir_all(&inner).expect("create doctor work directory");
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env_clear()
        .env("HOME", &home)
        .env("AXE_WORK_DIR", &work)
        .env("AXE_DOCTOR_IGNORED", "not-part-of-doctor-output")
        .env("PATH", "/nonexistent")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "cd \"$AXE_WORK_DIR/inner\"; doctor --json >\"$AXE_WORK_DIR/report.json\"; doctor >\"$AXE_WORK_DIR/human.txt\"; doctor --verbose >\"$AXE_WORK_DIR/verbose.txt\"",
        ])
        .output()
        .expect("run doctor from the bundled shell");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report_bytes = fs::read(work.join("report.json")).expect("read doctor report");
    assert!(
        !report_bytes
            .windows(b"AXE_DOCTOR_IGNORED".len())
            .any(|window| window == b"AXE_DOCTOR_IGNORED")
    );
    assert!(
        !report_bytes
            .windows(b"not-part-of-doctor-output".len())
            .any(|window| window == b"not-part-of-doctor-output")
    );
    let report: serde_json::Value =
        serde_json::from_slice(&report_bytes).expect("doctor report is JSON");
    assert_eq!(report["schema"], "axe_doctor");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["scope"], "shell");
    assert_eq!(
        report["shell"]["current_cwd"]["value"],
        inner.to_string_lossy().as_ref()
    );
    assert_eq!(
        report["filesystem"]["path"]["value"],
        work.to_string_lossy().as_ref()
    );
    for name in [
        "inheritable",
        "permitted",
        "effective",
        "bounding",
        "ambient",
    ] {
        assert!(
            report["restrictions"]["capability_sets"][name]["status"].is_string(),
            "capability set {name} must carry an independent observation"
        );
    }
    for name in ["cgroup", "ipc", "mnt", "net", "pid", "time", "user", "uts"] {
        assert!(
            report["restrictions"]["namespaces"][name]["status"].is_string(),
            "namespace {name} must carry an independent observation"
        );
    }
    assert!(report["restrictions"]["cgroups"]["membership"]["status"].is_string());
    assert!(report["restrictions"]["cgroups"]["limits"]["memory.max"]["status"].is_string());
    assert!(
        report["capabilities"]["memory"]["mfd_exec_flag"].is_object()
            && report["capabilities"]["memory"]["rw_to_rx"].is_object()
            && report["capabilities"]["memory"]["map_jit_rx"].is_object()
            && report["capabilities"]["memory"].get("mfd_exec").is_none()
            && report["capabilities"]["memory"].get("map_jit").is_none(),
        "doctor v2 memory capability names must preserve probe semantics"
    );
    for layer in ["virtual_machine", "container", "sandbox"] {
        assert_eq!(
            report["isolation"][layer]["interpretation"],
            "heuristic_indicator"
        );
    }
    assert!(
        report["launch"]["argv0"]["value"].is_string(),
        "UTF-8 argv[0] must be a plain string: {}",
        report["launch"]["argv0"]
    );
    let variables = report["environment"]["variables"]
        .as_array()
        .expect("environment list");
    let names = variables
        .iter()
        .map(|entry| {
            entry["name"]
                .as_str()
                .expect("environment name is a string")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "AXE",
            "AXE_SHELL",
            "PATH",
            "AXE_WORK_DIR",
            "AXE_STORE_MODE",
            "AXE_APPLET_DIR",
            "XDG_RUNTIME_DIR",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "TERM",
        ]
    );
    let axe_variable = variables
        .iter()
        .find(|entry| entry["name"] == "AXE")
        .expect("AXE marker is reported");
    assert_eq!(axe_variable["value"]["status"], "available");
    assert_eq!(axe_variable["value"]["value"], "true");
    let home_variable = variables
        .iter()
        .find(|entry| entry["name"] == "HOME")
        .expect("HOME is reported");
    assert_eq!(home_variable["value"]["status"], "available");
    assert_eq!(
        home_variable["value"]["value"],
        home.to_string_lossy().as_ref()
    );
    assert_eq!(report["filesystem"]["cleanup"]["value"], true);
    assert_eq!(report["filesystem"]["create_directory"]["value"], true);
    assert_eq!(report["filesystem"]["create_file"]["value"], true);
    assert_eq!(report["filesystem"]["write_file"]["value"], true);
    assert_eq!(report["filesystem"]["sync_file"]["value"], true);
    assert!(
        report["filesystem"].get("chmod_file").is_none()
            && report["filesystem"].get("execute_file").is_none(),
        "removed executable-file probe leaked into the schema: {}",
        report["filesystem"]
    );

    let human = fs::read_to_string(work.join("human.txt")).expect("read human doctor report");
    let mut prior = 0;
    for heading in [
        "AXE Doctor\n",
        "Summary\n",
        "Attention\n",
        "Launch\n",
        "Shell\n",
        "Host\n",
        "Isolation indicators\n",
        "Observed restrictions\n",
        "Filesystem probe\n",
        "Environment\n",
    ] {
        let offset = human[prior..]
            .find(heading)
            .map(|offset| prior + offset)
            .unwrap_or_else(|| panic!("missing doctor heading {heading:?}: {human}"));
        assert!(offset >= prior, "doctor headings are out of order: {human}");
        prior = offset + heading.len();
    }
    assert!(
        human.contains(&format!("  HOME={}", home.display())),
        "selected values must be directly readable: {human}"
    );
    assert!(
        !human.contains("{\"utf8\""),
        "UTF-8 paths must not use tagged objects: {human}"
    );
    assert!(
        human.contains("  Filesystem: writable; noexec ")
            && human.contains("  Noexec: ")
            && !human.contains("  Noexec mount option:")
            && !human.contains("Filesystem execution")
            && !human.contains("Execute file")
            && !human.contains("/bin/sh"),
        "filesystem output must report writability and noexec only: {human}"
    );
    assert!(
        !human.contains("  Scope:")
            && human.contains("  Sandboxing:")
            && !human.contains("Sandbox indicator")
            && !human.contains("PATH bridge: published —")
            && !human.contains("unknown — no strong")
            && !human.contains("unknown — no matching sandbox indicators")
            && !human.contains("\nActive checks\n")
            && !human.contains("\nDegradations\n")
            && !human.contains("  USER=")
            && !human.contains("  LOGNAME=")
            && !human.contains("  XDG_RUNTIME_DIR="),
        "default human output must omit redundant and verbose details: {human}"
    );
    assert!(
        human.contains("  Self-exec: works") && !human.contains("Self-exec: works (exit status"),
        "successful self-exec must use the concise human status: {human}"
    );
    assert!(
        !human.contains("  Candidates\n")
            && !human.contains("  memfd_create syscall:")
            && !human.contains("  Create directory:"),
        "default human output must keep detailed probe stages collapsed: {human}"
    );
    assert!(
        human.contains("  Seccomp: ")
            && !human.contains("  Seccomp mode:")
            && (human.contains("  No new privileges: enabled")
                || human.contains("  No new privileges: disabled")),
        "observed restrictions must use human-readable states: {human}"
    );

    let verbose = fs::read_to_string(work.join("verbose.txt")).expect("read verbose doctor report");
    assert!(
        verbose.contains("  Candidates\n")
            && verbose.contains("\nActive checks\n")
            && verbose.contains("  memfd_create syscall:")
            && verbose.contains("  Create directory:")
            && verbose.contains("  USER=absent")
            && verbose.contains("\nDegradations\n")
            && verbose.contains("  None")
            && !verbose.contains("  Make executable:")
            && !verbose.contains("  Execute file:"),
        "--verbose must expose full observations and probe stages: {verbose}"
    );

    assert!(
        fs::read_dir(&work)
            .expect("read doctor work directory")
            .all(|entry| !entry
                .expect("read work entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".axe-doctor-")),
        "doctor left a probe artifact"
    );
}

#[cfg(all(
    target_os = "linux",
    feature = "bundled-coreutils",
    feature = "applet-procutils",
    feature = "applet-util-linux"
))]
#[test]
fn full_linux_uutils_sets_are_registered() {
    let scratch = Scratch::new("uutils-list");
    let output = offline_axe(&scratch)
        .arg("--list")
        .output()
        .expect("list applet registry");

    assert!(output.status.success());
    let actual = String::from_utf8(output.stdout)
        .expect("tool list is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    let expected = [
        "[",
        "chroot",
        "dmesg",
        "free",
        "hd",
        "hexdump",
        "hostid",
        "hugetop",
        "install",
        "last",
        "mountpoint",
        "nice",
        "pathchk",
        "pgrep",
        "pidof",
        "pidwait",
        "pinky",
        "pkill",
        "pmap",
        "ps",
        "pwdx",
        "skill",
        "slabtop",
        "snice",
        "sysctl",
        "tload",
        "top",
        "uptime",
        "users",
        "vmstat",
        "w",
        "watch",
        "who",
    ];

    let missing = expected
        .into_iter()
        .filter(|name| !actual.contains(*name))
        .collect::<Vec<_>>();
    assert!(missing.is_empty(), "missing uutils applets: {missing:?}");
}

#[cfg(all(target_os = "linux", feature = "applet-procutils"))]
#[test]
fn ps_accepts_conventional_auxww_options() {
    let scratch = Scratch::new("ps-auxww-options");
    let output = offline_axe(&scratch)
        .args(["ps", "auxww"])
        .output()
        .expect("run ps auxww");

    assert!(
        output.status.success(),
        "ps auxww failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("ps output is UTF-8");
    let header = stdout.lines().next().expect("ps prints a header");
    assert_eq!(
        header.split_whitespace().collect::<Vec<_>>(),
        [
            "USER", "PID", "%CPU", "%MEM", "VSZ", "RSS", "TTY", "STAT", "START", "TIME", "COMMAND"
        ]
    );
}

#[cfg(unix)]
#[test]
fn busybox_alias_prepends_configured_arguments() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new("busybox");
    let alias = scratch.path().join("egrep");
    symlink(env!("CARGO_BIN_EXE_axe"), &alias).expect("create applet symlink");
    fs::write(scratch.path().join("input"), b"needle\n").expect("write grep input");

    let output = Command::new(alias)
        .arg("n(ee|ex)dle")
        .arg(scratch.path().join("input"))
        .output()
        .expect("run busybox alias");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"needle\n");
}

#[cfg(all(feature = "applet-vzik", target_os = "linux"))]
#[test]
fn vzik_collect_profile_is_an_ordered_protocol_v3_stream() {
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["vzik", "collect"])
        .output()
        .expect("run bundled vzik collect");

    let records = String::from_utf8(output.stdout)
        .expect("vzik output is UTF-8 JSONL")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid JSON record"))
        .collect::<Vec<_>>();
    assert!(records.iter().all(|record| record["schema_version"] == 3));
    let expected_status = match records.last().and_then(|record| record["outcome"].as_str()) {
        Some("complete") => 0,
        Some("degraded") => 3,
        other => panic!("unexpected terminal outcome: {other:?}"),
    };
    assert_eq!(
        output.status.code(),
        Some(expected_status),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        records.first().and_then(|record| record["type"].as_str()),
        Some("stream_start")
    );
    assert_eq!(records[0]["command_id"], "baseline-v3");
    assert_eq!(
        records.last().and_then(|record| record["type"].as_str()),
        Some("stream_end")
    );
    for (seq, record) in records.iter().enumerate() {
        assert_eq!(record["seq"], seq as u64);
    }
    let capabilities = records
        .iter()
        .filter(|record| record["type"] == "capability_start")
        .map(|record| record["capability"].as_str().expect("capability ID"))
        .collect::<Vec<_>>();
    assert_eq!(
        capabilities,
        [
            "host.info",
            "kernel.info",
            "kernel.modules",
            "kernel.sysctls",
            "security.posture",
            "process.list",
            "network.interfaces",
            "network.addresses",
            "network.resolvers",
            "network.routes",
            "network.neighbors",
            "network.sockets",
            "network.firewall",
            "mount.list",
            "cgroup.inspect",
            "user.list",
            "group.list",
            "auth.posture",
            "sudo.rules",
            "package.list",
            "service.list",
            "schedule.list",
            "systemctl.list",
            "dbus.list",
            "container.list",
            "container.inspect",
            "porto.list",
            "porto.inspect",
            "ssh.server_config",
            "ssh.authorized_keys",
        ]
    );
    let socket_request = records
        .iter()
        .find(|record| {
            record["type"] == "capability_start" && record["capability"] == "network.sockets"
        })
        .map(|record| &record["request"])
        .expect("network socket request");
    assert_eq!(
        socket_request["socket_selection"],
        "inet_all_unix_listeners"
    );
    let systemd_request = records
        .iter()
        .find(|record| {
            record["type"] == "capability_start" && record["capability"] == "systemctl.list"
        })
        .map(|record| &record["request"])
        .expect("systemd request");
    assert_eq!(
        systemd_request["excluded_unit_types"],
        serde_json::json!(["device"])
    );
}

#[cfg(all(feature = "applet-vzik", target_os = "linux"))]
#[test]
fn vzik_nested_help_is_generated_for_bundled_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["vzik", "portoctl", "inspect", "--help"])
        .output()
        .expect("run bundled vzik help");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(help.contains("Usage: vzik portoctl inspect"));
    assert!(help.contains("--socket <PATH>"));
    assert!(help.contains("[default: /run/portod.socket]"));
}

#[cfg(all(feature = "applet-vzik", target_os = "linux"))]
#[test]
fn vzik_targeted_file_read_is_chunked_and_bounded() {
    let scratch = Scratch::new("vzik-file-read");
    let path = scratch.path().join("evidence");
    fs::write(&path, b"abcdef").expect("write evidence");
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["vzik", "file", "read"])
        .arg(&path)
        .args(["--max-bytes", "5"])
        .output()
        .expect("run bundled vzik file read");

    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = String::from_utf8(output.stdout)
        .expect("vzik output is UTF-8 JSONL")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid JSON record"))
        .collect::<Vec<_>>();
    assert!(records.iter().all(|record| record["schema_version"] == 3));
    let data = records
        .iter()
        .find(|record| record["type"] == "data")
        .expect("file chunk");
    assert_eq!(data["data"]["content"], "abcde");
    assert_eq!(data["data"]["bytes"], 5);
    let end = records
        .iter()
        .find(|record| record["type"] == "capability_end")
        .expect("capability end");
    assert_eq!(end["outcome"], "partial");
    assert_eq!(end["limits_hit"], serde_json::json!(["max_bytes"]));
    let stream_end = records.last().expect("stream end");
    assert_eq!(stream_end["type"], "stream_end");
    assert_eq!(stream_end["outcome"], "degraded");
}

#[cfg(feature = "applet-jq")]
fn run_jq(args: &[&str], input: &[u8]) -> std::process::Output {
    run_with_input(
        Command::new(env!("CARGO_BIN_EXE_axe"))
            .args(["--applet", "jq", "--"])
            .args(args),
        input,
    )
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_raw_field_extraction_is_compatible() {
    let output = run_jq(&["-r", ".foo"], br#"{"foo":"bar"}"#);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"bar\n");
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_compact_item_iteration_is_compatible() {
    let output = run_jq(&["-c", ".items[]"], br#"{"items":[{"id":1},{"id":2}]}"#);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\"id\":1}\n{\"id\":2}\n");
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_map_transformation_is_compatible() {
    let output = run_jq(
        &[".items | map(.name)"],
        br#"{"items":[{"name":"one"},{"name":"two"}]}"#,
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"[\n  \"one\",\n  \"two\"\n]\n");
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_select_filtering_is_compatible() {
    let output = run_jq(
        &["select(.enabled)"],
        b"{\"enabled\":true}\n{\"enabled\":false}\n",
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\n  \"enabled\": true\n}\n");
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_string_argument_binding_is_compatible() {
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env("X", "value")
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "printf '%s\\n' '{\"foo\":null}' | jq --arg x \"$X\" '.foo = $x'",
        ])
        .output()
        .expect("run jq --arg through shell");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\n  \"foo\": \"value\"\n}\n");
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_json_argument_binding_is_compatible() {
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .env("X", r#"{"nested":true}"#)
        .args([
            "--norc",
            "--noprofile",
            "-c",
            "printf '%s\\n' '{\"foo\":null}' | jq --argjson x \"$X\" '.foo = $x'",
        ])
        .output()
        .expect("run jq --argjson through shell");
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        b"{\n  \"foo\": {\n    \"nested\": true\n  }\n}\n"
    );
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_exit_status_is_compatible() {
    let output = run_jq(&["-e", ".ok"], br#"{"ok":false}"#);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"false\n");
}

#[cfg(feature = "applet-jq")]
#[test]
fn jq_null_input_construction_is_compatible() {
    let output = run_jq(&["-n", r#"{foo: "bar"}"#], b"");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"{\n  \"foo\": \"bar\"\n}\n");
}

#[cfg(all(
    target_os = "linux",
    feature = "applet-linux-network",
    feature = "applet-linux-storage",
    feature = "applet-compression",
    feature = "applet-linux-inspect",
    feature = "applet-inotify",
    feature = "applet-util-linux"
))]
#[test]
fn diagnostic_applets_are_registered() {
    let scratch = Scratch::new("diagnostic-list");
    let output = offline_axe(&scratch)
        .arg("--list")
        .output()
        .expect("list applet registry");
    assert!(output.status.success());
    let actual = String::from_utf8(output.stdout)
        .expect("tool list is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    let expected = [
        "arp",
        "blkid",
        "blockdev",
        "bzip2",
        "host",
        "ifconfig",
        "inotifywait",
        "inotifywatch",
        "iostat",
        "ip",
        "ipaddr",
        "ipcalc",
        "ipcs",
        "iplink",
        "ipneigh",
        "iproute",
        "iprule",
        "last",
        "lsmod",
        "lsof",
        "lspci",
        "lsscsi",
        "lsusb",
        "modinfo",
        "mount",
        "mountpoint",
        "nslookup",
        "ping",
        "ping6",
        "traceroute",
        "traceroute6",
        "xz",
    ];
    let missing = expected
        .into_iter()
        .filter(|name| !actual.contains(*name))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "missing diagnostic applets: {missing:?}"
    );
}

#[cfg(feature = "applet-compression")]
#[test]
fn bzip2_and_xz_round_trip_concatenated_binary_input() {
    let input = b"first\0member\nsecond member\xff";
    for applet in ["bzip2", "xz"] {
        let compressed = run_with_input(
            Command::new(env!("CARGO_BIN_EXE_axe")).args(["--applet", applet, "--", "-c"]),
            input,
        );
        assert!(
            compressed.status.success(),
            "{applet} compression stderr: {}",
            String::from_utf8_lossy(&compressed.stderr)
        );
        assert_ne!(compressed.stdout, input);

        let restored = run_with_input(
            Command::new(env!("CARGO_BIN_EXE_axe")).args(["--applet", applet, "--", "-dc"]),
            &compressed.stdout,
        );
        assert!(
            restored.status.success(),
            "{applet} decompression stderr: {}",
            String::from_utf8_lossy(&restored.stderr)
        );
        assert_eq!(restored.stdout, input);
    }
}

#[cfg(all(target_os = "linux", feature = "applet-linux-storage"))]
#[test]
fn blkid_probes_ext4_metadata_from_a_regular_file() {
    let scratch = Scratch::new("blkid-ext4");
    let image = scratch.path().join("ext4.img");
    let mut bytes = vec![0u8; 2048];
    bytes[1080..1082].copy_from_slice(&[0x53, 0xef]);
    bytes[1120..1124].copy_from_slice(&0x40u32.to_le_bytes());
    bytes[1144..1150].copy_from_slice(b"volume");
    fs::write(&image, bytes).expect("write ext4 fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "blkid", "--", "-o", "export"])
        .arg(&image)
        .output()
        .expect("probe filesystem image");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("blkid output is UTF-8");
    assert!(stdout.contains("LABEL=volume\n"), "output: {stdout}");
    assert!(stdout.contains("TYPE=ext4\n"), "output: {stdout}");
}

#[cfg(all(target_os = "linux", feature = "applet-linux-network"))]
#[test]
fn ipcalc_reports_ipv4_network_boundaries() {
    let output = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "ipcalc", "--", "192.168.7.42/24"])
        .output()
        .expect("run ipcalc");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("ipcalc output is UTF-8");
    assert!(stdout.contains("Network:   192.168.7.0/24\n"));
    assert!(stdout.contains("HostMin:   192.168.7.1\n"));
    assert!(stdout.contains("HostMax:   192.168.7.254\n"));
    assert!(stdout.contains("Broadcast: 192.168.7.255\n"));
}

#[cfg(all(target_os = "linux", feature = "applet-inotify"))]
#[test]
fn inotifywait_reports_created_file() {
    let scratch = Scratch::new("inotifywait-create");
    let child = Command::new(env!("CARGO_BIN_EXE_axe"))
        .args([
            "--applet",
            "inotifywait",
            "--",
            "-q",
            "-t",
            "3",
            "-e",
            "create",
            "--format",
            "%e:%f",
        ])
        .arg(scratch.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start inotifywait");
    std::thread::sleep(std::time::Duration::from_millis(500));
    fs::write(scratch.path().join("created"), b"payload").expect("create watched file");
    let output = child.wait_with_output().expect("wait for inotifywait");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"CREATE:created\n");
}
