#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("axe-tree-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir(&root).unwrap();
    for name in [
        OsString::from_vec(b"back\\slash".to_vec()),
        OsString::from_vec(b"control\x01".to_vec()),
        OsString::from_vec(b"invalid\xff".to_vec()),
        OsString::from_vec(b"line\nname".to_vec()),
        OsString::from("space name"),
        OsString::from("é"),
    ] {
        fs::write(root.join(name), b"x").unwrap();
    }
    root
}

fn run_tree(locale: &str, flags: &[&str], root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_axe"))
        .args(["--applet", "tree", "--", "-n", "-i", "--noreport"])
        .args(flags)
        .arg(root)
        .env_clear()
        .env("AXE_STORE_MODE", "off")
        .env("PATH", "/nonexistent")
        .env("HOME", "/tmp")
        .env("TZ", "UTC")
        .env("LC_ALL", locale)
        .output()
        .unwrap()
}

fn expected(root: &Path, locale: &str, raw: bool, quote: bool) -> Vec<u8> {
    let mut output = root.as_os_str().as_bytes().to_vec();
    output.push(b'\n');
    let utf8 = locale == "C.UTF-8";
    let names: &[&[u8]] = if raw {
        &[
            b"back\\slash\n",
            b"control\x01\n",
            b"invalid\xff\n",
            b"line\nname\n",
            b"space name\n",
            "é\n".as_bytes(),
        ]
    } else if utf8 && quote {
        &[
            b"back\\slash\n",
            b"control?\n",
            b"invalid?\n",
            b"line?name\n",
            b"space name\n",
            "é\n".as_bytes(),
        ]
    } else if utf8 {
        &[
            b"back\\slash\n",
            b"control\\001\n",
            b"invalid\\377\n",
            b"line\\012name\n",
            b"space name\n",
            "é\n".as_bytes(),
        ]
    } else {
        &[
            b"back\\\\slash\n",
            b"control\\001\n",
            b"invalid\\377\n",
            b"line\\nname\n",
            b"space\\ name\n",
            b"\\303\\251\n",
        ]
    };
    for name in names {
        output.extend_from_slice(name);
    }
    output
}

#[test]
fn tree_name_quoting_matches_locale_and_flags() {
    let root = fixture();
    for (locale, flags, raw, quote) in [
        ("C", &[][..], false, false),
        ("C", &["-N"][..], true, false),
        ("C", &["-q"][..], false, true),
        ("C.UTF-8", &[][..], false, false),
        ("C.UTF-8", &["-N"][..], true, false),
        ("C.UTF-8", &["-q"][..], false, true),
        ("C.UTF-8", &["-Nq"][..], true, true),
        ("zz_ZZ.UTF-8", &[][..], false, false),
        ("zz_ZZ.UTF-8", &["-N"][..], true, false),
        ("zz_ZZ.UTF-8", &["-q"][..], false, true),
        ("zz_ZZ.UTF-8", &["-Nq"][..], true, true),
    ] {
        let output = run_tree(locale, flags, &root);
        assert!(output.status.success(), "{locale} {flags:?}");
        assert_eq!(
            output.stdout,
            expected(&root, locale, raw, quote),
            "{locale} {flags:?}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}
