#![cfg(target_os = "linux")]

use std::fs::File;
use std::net::Ipv6Addr;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
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
fn bundled_route_accepts_read_only_queries_and_rejects_mutations() {
    for (args, output_required) in [
        (vec![], true),
        (vec!["-n"], true),
        (vec!["-A", "inet"], true),
        (vec!["-A", "inet6"], false),
        (vec!["show", "table", "main"], true),
        (vec!["-n", "show", "dev", "lo"], false),
    ] {
        let output = axe("route", &args).output().unwrap();
        assert!(
            output.status.success(),
            "route {args:?}: {}",
            describe(&output)
        );
        if output_required {
            assert!(
                !output.stdout.is_empty(),
                "route {args:?}: {}",
                describe(&output)
            );
        }
    }
    for (args, code) in [
        (vec!["show", "dev", "axe-no-device"], 1),
        (vec!["show", "table", "not-a-table"], 255),
        (vec!["add", "10.0.0.0/8", "dev", "lo"], 1),
        (vec!["delete", "10.0.0.0/8"], 1),
    ] {
        let output = axe("route", &args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "route {args:?}: {}",
            describe(&output)
        );
        assert!(
            output.stdout.is_empty(),
            "route {args:?}: {}",
            describe(&output)
        );
        assert!(
            !output.stderr.is_empty(),
            "route {args:?}: {}",
            describe(&output)
        );
    }
    let output = axe("route", &["--help"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Read-only"));
}

#[test]
fn actual_bundled_network_applets_reject_invalid_arguments() {
    let output = axe("ifconfig", &["-s", "axe-no-device"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.starts_with(b"Iface "));
    assert!(!output.stderr.is_empty());
    let output = axe("arp", &["-nx"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn actual_route_output_handles_full_devices_and_closed_consumers() {
    let output = axe("route", &["-n"])
        .stdout(Stdio::from(
            File::options().write(true).open("/dev/full").unwrap(),
        ))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(!output.stderr.is_empty());
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader);
    let fd: OwnedFd = writer.into();
    let output = axe("route", &["-n"])
        .stdout(Stdio::from(fd))
        .output()
        .unwrap();
    assert_eq!(output.status.signal(), Some(13), "{}", describe(&output));
    assert!(output.stderr.is_empty());
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
    }
    for args in [
        vec!["192.0.2.1/24", "255.255.0.0", "garbage"],
        vec!["192.0.2.1/33"],
        vec!["192.0.2.1", "255.0.255.0"],
        vec!["-46", "192.0.2.1"],
    ] {
        let output = axe("ipcalc", &args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
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
