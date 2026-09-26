use crate::{error, trace_categories};
use std::path::PathBuf;

use uzers::os::unix::UserExt;

pub(crate) fn is_root() -> bool {
    uzers::get_current_uid() == 0
}

pub(crate) fn get_user_home_dir(username: &str) -> Option<PathBuf> {
    if let Some(user_info) = uzers::get_user_by_name(username) {
        return Some(user_info.home_dir().to_path_buf());
    }

    None
}

pub(crate) fn get_current_user_home_dir() -> Option<PathBuf> {
    if let Some(username) = uzers::get_current_username()
        && let Some(user_info) = uzers::get_user_by_name(&username)
    {
        return Some(user_info.home_dir().to_path_buf());
    }

    None
}

pub(crate) fn get_current_user_default_shell() -> Option<PathBuf> {
    if let Some(username) = uzers::get_current_username()
        && let Some(user_info) = uzers::get_user_by_name(&username)
    {
        return Some(user_info.shell().to_path_buf());
    }

    None
}

#[expect(clippy::unnecessary_wraps)]
pub(crate) fn get_current_uid() -> Result<u32, error::Error> {
    Ok(uzers::get_current_uid())
}

#[expect(clippy::unnecessary_wraps)]
pub(crate) fn get_current_gid() -> Result<u32, error::Error> {
    Ok(uzers::get_current_gid())
}

#[expect(clippy::unnecessary_wraps)]
pub(crate) fn get_effective_uid() -> Result<u32, error::Error> {
    Ok(uzers::get_effective_uid())
}

#[expect(clippy::unnecessary_wraps)]
pub(crate) fn get_effective_gid() -> Result<u32, error::Error> {
    Ok(uzers::get_effective_gid())
}

pub(crate) fn get_current_username() -> Result<String, error::Error> {
    if let Some(username) = uzers::get_current_username() {
        return Ok(username.to_string_lossy().into_owned());
    }

    for variable in ["USER", "LOGNAME"] {
        if let Some(username) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
            return Ok(username.to_string_lossy().into_owned());
        }
    }

    Ok(uzers::get_current_uid().to_string())
}

pub(crate) fn get_user_group_ids() -> Result<Vec<u32>, error::Error> {
    let mut groups = Vec::<libc::gid_t>::new();
    loop {
        // SAFETY: a zero size instructs getgroups to return the required
        // element count without dereferencing the null pointer.
        let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
        if count < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if count == 0 {
            break;
        }

        groups.resize(count as usize, 0);
        // SAFETY: groups has capacity for count initialized gid_t values and
        // remains exclusively borrowed for the duration of the call.
        let result = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
        if result >= 0 {
            groups.truncate(result as usize);
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINVAL) {
            return Err(error.into());
        }
    }

    groups.push(uzers::get_effective_gid());
    groups.sort_unstable();
    groups.dedup();
    Ok(groups)
}

pub(crate) fn get_all_users() -> Result<Vec<String>, error::Error> {
    // TODO(#475): uzers::all_users() is available but unsafe; for now we just return the current
    // user. That's better than nothing.
    let user = get_current_username()?;
    Ok(vec![user])
}

pub(crate) fn get_all_groups() -> Result<Vec<String>, error::Error> {
    // TODO(#475): uzers::all_groups() is available but unsafe; for now we just return the current
    // user's groups. That's better than nothing.
    let groups = get_current_user_groups()?;
    let group_names = groups
        .into_iter()
        .map(|g| g.name().to_string_lossy().to_string());
    Ok(group_names.collect())
}

fn get_current_user_groups() -> Result<Vec<uzers::Group>, error::Error> {
    let username = uzers::get_current_username().ok_or_else(|| error::ErrorKind::NoCurrentUser)?;
    let gid = uzers::get_current_gid();
    let groups = uzers::get_user_groups(&username, gid).unwrap_or_default();
    Ok(groups)
}
