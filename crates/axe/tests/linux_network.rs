#![cfg(target_os = "linux")]

use std::fs::File;
use std::net::Ipv6Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn axe(applet: &str, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_axe"));
    command
        .args(["--applet", applet, "--"])
        .args(args)
        .env("LC_ALL", "C");
    command
}

fn reference_program(name: &str) -> PathBuf {
    let path = std::env::var_os("AXE_REFERENCE_PATH").expect(
        "enter nix develop .#default; include net-tools in AXE_REFERENCE_PATH for namespace parity",
    );
    std::env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("reference {name} is absent from AXE_REFERENCE_PATH"))
}

fn reference(applet: &str, args: &[&str]) -> Output {
    Command::new(reference_program(applet))
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .expect("run reference applet")
}

fn describe(output: &Output) -> String {
    format!(
        "status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn ip_link_selectors_and_device_errors_use_actual_axe() {
    let output = axe("ip", &["-j", "link", "show", "dev", "lo"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    let links: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(links.as_array().unwrap().len(), 1);
    assert_eq!(links[0]["ifname"], "lo");
    for args in [
        vec!["link", "show", "dev", "axe-no-device"],
        vec!["route", "get", "127.0.0.1", "dev", "axe-no-device"],
    ] {
        let output = axe("ip", &args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.starts_with(b"ip:"));
    }
    let output = axe("ip", &["link", "s", "dev", "lo", "up"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("read-only"));
}

#[test]
fn actual_applets_reject_invalid_selectors_and_keep_net_tools_errors() {
    for args in [
        vec!["route", "show", "table", "not-a-table"],
        vec!["address", "show", "scope"],
        vec!["-d", "link", "show"],
    ] {
        let output = axe("ip", &args).output().unwrap();
        assert_eq!(output.status.code(), Some(255), "{}", describe(&output));
        assert!(output.stdout.is_empty());
    }
    let output = axe("ifconfig", &["-s", "axe-no-device"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.starts_with(b"Iface "));
    assert_eq!(
        output.stderr,
        b"axe-no-device: error fetching interface information: Device not found\n"
    );
    let output = axe("arp", &["-nx"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stderr, b"arp: invalid option -- 'x'\n");
}

#[test]
fn actual_ip_output_handles_full_devices_and_closed_consumers() {
    let output = axe("ip", &["-j", "link", "show", "dev", "lo"])
        .stdout(Stdio::from(
            File::options().write(true).open("/dev/full").unwrap(),
        ))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(String::from_utf8_lossy(&output.stderr).contains("No space left on device"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let fd: OwnedFd = writer.into();
    let output = axe("ip", &["-j", "link", "show", "dev", "lo"])
        .stdout(Stdio::from(fd))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    assert!(output.stderr.is_empty());
}

#[test]
fn bare_ip_and_rejected_selectors_keep_reference_exit_classes() {
    let output = axe("ip", &[]).output().unwrap();
    assert_eq!(output.status.code(), Some(255), "{}", describe(&output));
    assert!(output.stdout.is_empty());
    assert!(
        output
            .stderr
            .starts_with(b"Usage: ip [ OPTIONS ] OBJECT { COMMAND | help }\n")
    );
    for (applet, args, code, message) in [
        (
            "ip",
            vec!["audit-no-object"],
            1,
            "Object \"audit-no-object\" is unknown, try \"ip help\".\n",
        ),
        (
            "iprule",
            vec!["show", "audit-no-selector"],
            1,
            "Error: inet prefix is expected rather than \"audit-no-selector\".\n",
        ),
        (
            "ipaddr",
            vec!["show", "dev"],
            255,
            "Command line is not complete. Try option \"help\"\n",
        ),
        (
            "iproute",
            vec!["show", "table", "audit-no-table"],
            255,
            "Error: argument \"audit-no-table\" is wrong: table id value is invalid\n\n",
        ),
    ] {
        let output = axe(applet, &args).output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{}", describe(&output));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, message.as_bytes());
    }
}

#[test]
fn ipcalc_selected_fields_and_json_use_modern_ipcalc_contract() {
    let output = axe("ipcalc", &["-nmp", "192.168.7.42/24"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    assert_eq!(
        output.stdout,
        b"NETMASK=255.255.255.0\nPREFIX=24\nNETWORK=192.168.7.0\n"
    );
    assert!(output.stderr.is_empty());
    let output = axe("ipcalc", &["-j", "192.168.7.42/24"]).output().unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({
            "ADDRESS": "192.168.7.42", "NETWORK": "192.168.7.0",
            "NETMASK": "255.255.255.0", "PREFIX": "24", "BROADCAST": "192.168.7.255",
            "ADDRSPACE": "Private Use", "MINADDR": "192.168.7.1",
            "MAXADDR": "192.168.7.254", "ADDRESSES": "254"
        })
    );
    for (address, endpoint) in [
        ("192.0.2.1", "192.0.2.1"),
        ("192.0.2.1/32", "192.0.2.1"),
        ("2001:db8::1", "2001:db8::1"),
        ("2001:db8::1/128", "2001:db8::1"),
    ] {
        let output = axe("ipcalc", &["--minaddr", "--maxaddr", address])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", describe(&output));
        assert_eq!(
            output.stdout,
            format!("MINADDR={endpoint}\nMAXADDR={endpoint}\n").as_bytes()
        );
        assert!(output.stderr.is_empty());
    }
    for (args, diagnostic) in [
        (
            vec!["192.0.2.1/24", "255.255.0.0", "garbage"],
            "ipcalc: bad IPv4 address: 192.0.2.1/24\n",
        ),
        (vec!["192.0.2.1/33"], "ipcalc: bad IPv4 prefix: 33\n"),
        (
            vec!["192.0.2.1", "255.0.255.0"],
            "ipcalc: bad IPv4 prefix: 255.0.255.0\n",
        ),
        (
            vec!["-46", "192.0.2.1"],
            "ipcalc: you cannot specify both IPv4 and IPv6\n",
        ),
    ] {
        let output = axe("ipcalc", &args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, diagnostic.as_bytes());
    }
}

fn assert_owned_network_namespace() {
    let network = File::open("/proc/self/ns/net").unwrap();
    let user = std::fs::metadata("/proc/self/ns/user").unwrap();
    for (kind, inode) in [
        ("NET", network.metadata().unwrap().ino()),
        ("USER", user.ino()),
    ] {
        let parent = std::env::var(format!("AXE_NETWORK_NAMESPACE_PARENT_{kind}"))
            .expect("namespace child must have a recorded parent namespace")
            .parse::<u64>()
            .unwrap();
        assert_ne!(
            inode, parent,
            "refusing writes in the parent's {kind} namespace"
        );
    }
    // SAFETY: NS_GET_USERNS takes no pointer and returns a new owned descriptor.
    let owner = unsafe { libc::ioctl(network.as_raw_fd(), 0xb701) };
    assert!(
        owner >= 0,
        "query network namespace owner: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: a successful NS_GET_USERNS returned this fresh descriptor.
    let owner = File::from(unsafe { OwnedFd::from_raw_fd(owner) });
    let owner = owner.metadata().unwrap();
    assert_eq!(
        (owner.dev(), owner.ino()),
        (user.dev(), user.ino()),
        "network namespace must belong to the isolated user namespace"
    );
}

fn setup(args: &[&str]) {
    let output = reference("ip", args);
    assert!(
        output.status.success(),
        "ip {args:?}: {}",
        describe(&output)
    );
}

fn parity(applet: &str, args: &[&str], json: bool) {
    let expected = reference(applet, args);
    let actual = axe(applet, args).output().unwrap();
    assert_eq!(
        actual.status.code(),
        expected.status.code(),
        "{applet} {args:?}\nexpected={}\nactual={}",
        describe(&expected),
        describe(&actual)
    );
    assert_eq!(actual.stderr, expected.stderr, "{applet} {args:?}");
    if json && expected.status.success() {
        let mut expected: serde_json::Value =
            serde_json::from_slice(&expected.stdout).expect("reference JSON");
        // iproute2 can leave empty link/address objects after filtering.
        if let Some(records) = expected.as_array_mut() {
            records.retain(|record| !record.as_object().is_some_and(|object| object.is_empty()));
            if applet == "ip"
                && records.iter().any(|record| {
                    record.get("addr_info").is_some() && record.get("ifindex").is_some()
                })
            {
                // Family-filtered link dumps can omit attributes retained by the full address view.
                let links = reference("ip", &["-j", "link", "show"]);
                assert!(links.status.success(), "{}", describe(&links));
                let links: Vec<serde_json::Value> = serde_json::from_slice(&links.stdout).unwrap();
                for record in records.iter_mut() {
                    if record.get("addr_info").is_none() || record.get("ifindex").is_none() {
                        continue;
                    }
                    let link = links
                        .iter()
                        .find(|link| link["ifindex"] == record["ifindex"])
                        .expect("reference link metadata for address object");
                    let record = record.as_object_mut().unwrap();
                    for key in ["qdisc", "master", "group", "txqlen"] {
                        if !record.contains_key(key)
                            && let Some(value) = link.get(key)
                        {
                            record.insert(key.to_owned(), value.clone());
                        }
                    }
                }
            }
            for record in records {
                if let Some(addresses) = record
                    .get_mut("addr_info")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    addresses.retain(|address| {
                        !address.as_object().is_some_and(|object| object.is_empty())
                    });
                }
            }
        }
        let actual: serde_json::Value = serde_json::from_slice(&actual.stdout).expect("axe JSON");
        assert_eq!(actual, expected, "{applet} {args:?}");
    } else {
        assert_eq!(actual.stdout, expected.stdout, "{applet} {args:?}");
    }
}

#[test]
#[ignore = "creates an isolated user/network namespace; requires iproute2, net-tools and unshare in AXE_REFERENCE_PATH"]
fn namespace_readonly_reference_parity() {
    if std::env::var_os("AXE_NETWORK_NAMESPACE_CHILD").is_none() {
        let output = Command::new(reference_program("unshare"))
            .args(["--user", "--map-root-user", "--net", "--"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "namespace_readonly_reference_parity",
                "--ignored",
                "--nocapture",
            ])
            .env("AXE_NETWORK_NAMESPACE_CHILD", "1")
            .env(
                "AXE_NETWORK_NAMESPACE_PARENT_NET",
                std::fs::metadata("/proc/self/ns/net")
                    .unwrap()
                    .ino()
                    .to_string(),
            )
            .env(
                "AXE_NETWORK_NAMESPACE_PARENT_USER",
                std::fs::metadata("/proc/self/ns/user")
                    .unwrap()
                    .ino()
                    .to_string(),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "namespace parity: {}",
            describe(&output)
        );
        return;
    }
    assert_owned_network_namespace();
    setup(&["link", "set", "lo", "up"]);
    for name in ["d0", "d1"] {
        setup(&["link", "add", name, "type", "dummy"]);
        setup(&["link", "set", name, "up"]);
    }
    setup(&["link", "add", "empty0", "type", "dummy"]);
    setup(&["link", "set", "d0", "group", "5"]);
    setup(&["link", "add", "blue", "type", "vrf", "table", "100"]);
    setup(&["link", "set", "d1", "master", "blue"]);
    setup(&["link", "add", "v0", "type", "veth", "peer", "name", "v1"]);
    setup(&["address", "add", "10.1.0.1/24", "dev", "d0"]);
    setup(&[
        "address",
        "add",
        "10.1.0.9/24",
        "dev",
        "d0",
        "label",
        "d0:alias",
    ]);
    setup(&[
        "-6",
        "address",
        "add",
        "2001:db8:1::1/64",
        "dev",
        "d0",
        "nodad",
    ]);
    setup(&[
        "route",
        "add",
        "10.5.0.0/16",
        "table",
        "100",
        "proto",
        "99",
        "metric",
        "100",
        "mtu",
        "1400",
        "window",
        "2000",
        "rtt",
        "10ms",
        "advmss",
        "1360",
        "nexthop",
        "dev",
        "d0",
        "weight",
        "1",
        "nexthop",
        "dev",
        "d1",
        "weight",
        "2",
    ]);
    setup(&[
        "route",
        "add",
        "blackhole",
        "10.6.0.0/16",
        "table",
        "100",
        "proto",
        "18",
    ]);
    setup(&[
        "route",
        "add",
        "throw",
        "10.7.0.0/16",
        "table",
        "100",
        "proto",
        "80",
    ]);
    setup(&[
        "route",
        "add",
        "10.8.0.0/16",
        "table",
        "100",
        "proto",
        "1",
        "dev",
        "d0",
    ]);
    setup(&[
        "-6",
        "route",
        "add",
        "2001:db8:2::/64",
        "table",
        "100",
        "dev",
        "d0",
        "metric",
        "0",
        "pref",
        "high",
    ]);
    setup(&[
        "neighbour",
        "add",
        "10.1.0.5",
        "lladdr",
        "ff:ff:ff:ff:ff:ff",
        "nud",
        "permanent",
        "dev",
        "d0",
    ]);
    setup(&["neighbour", "add", "10.1.0.6", "nud", "failed", "dev", "d0"]);
    setup(&[
        "neighbour",
        "add",
        "10.1.0.7",
        "lladdr",
        "02:00:00:00:00:07",
        "nud",
        "permanent",
        "dev",
        "d1",
    ]);
    setup(&["link", "set", "d1", "down"]);
    setup(&["rule", "add", "priority", "1000", "table", "100"]);

    for scope in ["global", "0"] {
        parity("ip", &["address", "show", "scope", scope], false);
        let output = axe("ip", &["-j", "address", "show", "scope", scope])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", describe(&output));
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let links = json.as_array().unwrap();
        assert!(!links.iter().any(|link| link["ifname"] == "lo"));
        let empty = links
            .iter()
            .find(|link| link["ifname"] == "empty0")
            .expect("scope filter retains interfaces without addresses");
        assert_eq!(empty["addr_info"], serde_json::json!([]));
        let d0 = links.iter().find(|link| link["ifname"] == "d0").unwrap();
        let addresses = d0["addr_info"].as_array().unwrap();
        assert_eq!(addresses.len(), 3);
        assert!(addresses.iter().all(|address| {
            address["local"].is_string()
                && address["family"].is_string()
                && address["scope"] == "global"
        }));
        for local in ["10.1.0.1", "10.1.0.9", "2001:db8:1::1"] {
            assert!(addresses.iter().any(|address| address["local"] == local));
        }
    }

    for args in [
        vec!["route", "show", "table", "all"],
        vec!["route", "show", "table", "0"],
        vec!["route", "show", "table", "100"],
        vec!["route", "show", "table", "100", "dev", "d0"],
        vec!["-o", "route", "show", "table", "100"],
        vec!["-N", "route", "show", "table", "100"],
        vec!["route", "show", "table", "100", "proto", "18"],
        vec!["route", "show", "table", "100", "proto", "80"],
        vec!["route", "show", "table", "100", "proto", "1"],
        vec!["route", "show", "match", "10.1.0.5"],
        vec!["route", "show", "root", "10.1.0.0/24"],
        vec!["route", "show", "10.1.0.0/24"],
        vec!["route", "get", "10.1.0.5", "dev", "d0"],
        vec!["route", "get", "10.1.0.5", "mark", "0x6", "dev", "d0"],
        vec!["route", "get", "127.0.0.1"],
        vec!["route", "get", "127.0.0.1", "uid", "1000"],
        vec!["rule", "show", "table", "local"],
        vec!["rule", "show", "table", "100"],
        vec!["-6", "route", "show", "table", "100"],
        vec!["route", "get", "203.0.113.1"],
        vec!["route", "get", "10.1.0.5", "dev", "axe-no-device"],
        vec!["route", "get", "10.1.0.5", "from", "::1"],
        vec!["neighbour", "show", "to", "10.1.0.0/24", "dev", "d0"],
        vec!["neighbour", "show", "nud", "all", "to", "10.1.0.0/24"],
        vec!["neighbour", "show", "to", "10.1.0.0/24", "vrf", "blue"],
        vec!["link", "show", "dev", "d1", "up"],
        vec!["-br", "link", "show", "dev", "d0"],
        vec!["-4", "address", "show"],
        vec!["-6", "address", "show"],
        vec!["-4", "address", "show", "scope", "global", "dev", "d0"],
        vec!["-br", "-4", "address", "show"],
        vec!["address", "show", "up", "dev", "d0"],
    ] {
        parity("ip", &args, false);
        let mut json_args = args.clone();
        json_args.insert(0, "-j");
        parity("ip", &json_args, true);
    }

    // The reference proto selector includes unrelated protocol-zero addresses.
    let addresses = reference("ip", &["-j", "-6", "address", "show", "dev", "d0"]);
    assert!(addresses.status.success(), "{}", describe(&addresses));
    let addresses: serde_json::Value = serde_json::from_slice(&addresses.stdout).unwrap();
    let expected: Vec<_> = addresses[0]["addr_info"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|address| address["protocol"] == "kernel_ll")
        .cloned()
        .collect();
    let expected = serde_json::Value::Array(expected);
    for protocol in ["3", "kernel_ll"] {
        let output = axe("ip", &["-j", "-6", "address", "show", "proto", protocol])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", describe(&output));
        let output: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            output
                .iter()
                .map(|link| (link["ifname"].as_str(), &link["addr_info"]))
                .eq([(Some("d0"), &expected)]),
            "{protocol}: {output:?}"
        );
    }

    for args in [
        vec!["-an"],
        vec!["-ne"],
        vec!["-nea"],
        vec!["-nae"],
        vec!["-nid0"],
        vec!["-n", "--device=d0"],
        vec!["-n", "--device="],
        vec!["-n", "10.2"],
        vec!["-an", "10.2"],
        vec!["-ni", "axe-no-device"],
        vec!["localhost"],
    ] {
        parity("arp", &args, false);
    }
    setup(&["address", "delete", "10.1.0.9/24", "dev", "d0"]);
    parity("ifconfig", &["-s", "axe-no-device"], false);
    for args in [vec!["-v", "lo"], vec!["lo"]] {
        parity("ifconfig", &args, false);
    }
    for args in [vec!["-s"], vec!["-a", "-s"]] {
        let expected = reference("ifconfig", &args);
        let actual = axe("ifconfig", &args).output().unwrap();
        assert!(expected.status.success(), "{}", describe(&expected));
        assert!(actual.status.success(), "{}", describe(&actual));
        assert_eq!(actual.stderr, expected.stderr);
        let summary = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .lines()
                .skip(1)
                .map(|line| {
                    let fields = line.split_whitespace().collect::<Vec<_>>();
                    (
                        fields[0].to_owned(),
                        fields[1].to_owned(),
                        fields.last().unwrap().to_string(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            summary(&actual.stdout),
            summary(&expected.stdout),
            "{args:?}"
        );
    }
    for (wrapper, object, args) in [
        ("iprule", "rule", vec!["show", "table", "local"]),
        ("iproute", "route", vec!["get", "127.0.0.1", "uid", "1000"]),
    ] {
        let mut reference_args = vec![object];
        reference_args.extend_from_slice(&args);
        let expected = reference("ip", &reference_args);
        let actual = axe(wrapper, &args).output().unwrap();
        assert_eq!(
            actual.status.code(),
            expected.status.code(),
            "{wrapper}: {}",
            describe(&actual)
        );
        assert_eq!(actual.stdout, expected.stdout, "{wrapper}");
        assert_eq!(actual.stderr, expected.stderr, "{wrapper}");
    }
}

#[test]
#[ignore = "requires modern ipcalc-ng 1.0.3 in AXE_REFERENCE_PATH, not the Perl ipcalc"]
fn modern_ipcalc_reference_parity() {
    for args in [
        vec!["192.168.7.42/24"],
        vec!["192.168.7.0/24"],
        vec!["192.168.7.42", "255.255.255.0"],
        vec!["10.1.2.3"],
        vec!["192.0.2.1/31"],
        vec!["192.0.2.1/32"],
        vec!["2001:db8::42/64"],
        vec!["2001:db8::1/128"],
        vec!["-n", "-m", "-p", "192.168.7.42/24"],
        vec!["-nm", "192.168.7.42/24"],
        vec!["-nbmp", "2001:db8::42/64"],
        vec!["192.0.2.1/33"],
        vec!["192.0.2.1", "255.0.255.0"],
        vec!["192.0.2.1/24", "255.255.0.0", "garbage"],
        vec!["-46", "192.0.2.1"],
        vec!["-cn", "192.0.2.1"],
        vec!["-vn", "192.0.2.1"],
    ] {
        parity("ipcalc", &args, false);
    }
    // The reference leaves 2^128 blank; derive that boundary without overflowing u128.
    let expected_count = format!("{}{}", u128::MAX / 10, u128::MAX % 10 + 1);
    let output = axe("ipcalc", &["::/0"]).output().unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    assert!(output.stderr.is_empty());
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let fields = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        u128::from(fields["HostMin:"].parse::<Ipv6Addr>().unwrap()),
        0
    );
    assert_eq!(
        u128::from(fields["HostMax:"].parse::<Ipv6Addr>().unwrap()),
        u128::MAX
    );
    let (power, count) = fields["Hosts/Net:"].split_once(" = ").unwrap();
    assert_eq!(power, "2^(128)");
    assert_eq!(count, expected_count);
    let output = axe("ipcalc", &["--addresses", "::/0"]).output().unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        format!("ADDRESSES={expected_count}\n").as_bytes()
    );
    let output = axe("ipcalc", &["-j", "::/0"]).output().unwrap();
    assert!(output.status.success(), "{}", describe(&output));
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({
            "FULLNETWORK":"0000:0000:0000:0000:0000:0000:0000:0000",
            "NETWORK":"::", "NETMASK":"::", "PREFIX":"0", "ADDRSPACE":"Reserved",
            "MINADDR":"::", "MAXADDR":"ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "ADDRESSES":expected_count
        })
    );
    for address in [
        "192.168.7.42/24",
        "192.0.2.1/31",
        "10.1.2.3",
        "2001:db8::42/64",
    ] {
        parity("ipcalc", &["-j", address], true);
    }
}
