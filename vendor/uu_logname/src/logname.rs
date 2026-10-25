// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore (ToDO) getlogin userlogin loginuid

use clap::Command;
use core::ffi::CStr;
use std::io::{Write, stdout};
use uucore::translate;
use uucore::{error::UResult, show_error};

#[cfg(all(target_os = "linux", target_env = "musl"))]
fn get_userlogin() -> Option<Vec<u8>> {
    // musl's getlogin returns LOGNAME and its utmp interfaces are stubs.
    // The audit UID identifies the original login, not the effective user.
    let uid = std::fs::read_to_string("/proc/self/loginuid")
        .ok()?
        .trim()
        .parse::<libc::uid_t>()
        .ok()?;
    if uid == libc::uid_t::MAX {
        return None;
    }

    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: passwd and result are writable, correctly aligned output
        // storage. buffer remains live and writable for the supplied length.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE {
            buffer.resize(buffer.len().checked_mul(2)?, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        // SAFETY: a successful getpwuid_r with a non-null result initialized
        // passwd; its string pointers refer to our still-live buffer.
        let passwd = unsafe { passwd.assume_init() };
        if passwd.pw_name.is_null() {
            return None;
        }
        // SAFETY: getpwuid_r supplied this NUL-terminated name in buffer.
        let name = unsafe { CStr::from_ptr(passwd.pw_name) }.to_bytes();
        return (!name.is_empty()).then(|| name.to_vec());
    }
}

#[cfg(not(all(target_os = "linux", target_env = "musl")))]
fn get_userlogin() -> Option<Vec<u8>> {
    // SAFETY: getlogin has no arguments and returns null or a libc-owned
    // NUL-terminated name, which is copied before another login lookup.
    let login_ptr = unsafe { libc::getlogin() };
    if login_ptr.is_null() {
        return None;
    }
    // SAFETY: the non-null pointer returned by getlogin is a valid C string.
    let name = unsafe { CStr::from_ptr(login_ptr) }.to_bytes();
    (!name.is_empty()).then(|| name.to_vec())
}

#[uucore::main(no_signals)]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let _ = uucore::clap_localization::handle_clap_result(uu_app(), args)?;

    if let Some(userlogin) = get_userlogin() {
        let mut output = stdout().lock();
        output.write_all(&userlogin)?;
        output.write_all(b"\n")?;
        Ok(())
    } else {
        show_error!("{}", translate!("logname-error-no-login-name"));
        Err(1.into())
    }
}

pub fn uu_app() -> Command {
    Command::new("logname")
        .version(uucore::crate_version!())
        .help_template(uucore::localized_help_template("logname"))
        .override_usage(translate!("logname-usage"))
        .about(translate!("logname-about"))
        .infer_long_args(true)
}
