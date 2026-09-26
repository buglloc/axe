use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{
    coverage_diagnostic, emit, finish, item_limit, output_limit, parse_u64, path_value,
    read_bounded, sorted_entries, source_failure, text, trim_ascii, unavailable,
};
use crate::cli::{CapabilityId, Invocation};
use crate::protocol::{
    CapabilityReport, Coverage, ExecutionContext, ProtocolError, RecordSink, check_deadline,
};

const ACCOUNT_SOURCE_LIMIT: u64 = 8 << 20;
const CONFIG_SOURCE_LIMIT: u64 = 1 << 20;
const TOTAL_CONFIG_LIMIT: u64 = 16 << 20;

#[derive(Clone)]
struct PasswdEntry {
    name: Vec<u8>,
    uid: u64,
    gid: u64,
    gecos: Vec<u8>,
    home: PathBuf,
    shell: Vec<u8>,
}

pub fn run<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    match invocation.capability {
        CapabilityId::UserList => users(invocation, sink, deadline),
        CapabilityId::GroupList => groups(invocation, sink, deadline),
        CapabilityId::SudoRules => sudo_rules(invocation, sink, deadline),
        CapabilityId::SshServerConfig => ssh_server_config(invocation, sink, deadline),
        CapabilityId::SshAuthorizedKeys => authorized_keys(invocation, sink, deadline),
        _ => unreachable!("account dispatcher received unrelated capability"),
    }
}

fn users<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::UserList;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let entries = match passwd_entries() {
        Ok(entries) => entries,
        Err(error) => {
            return unavailable(sink, capability, Path::new("/etc/passwd"), error, coverage);
        }
    };

    for entry in entries {
        check_deadline(deadline)?;
        coverage.scanned += 1;
        if coverage.observed as usize >= max_items {
            return Ok(finish(coverage, true, Some("max_items")));
        }
        let data = json!({
            "name":text(&entry.name),
            "uid":entry.uid,
            "gid":entry.gid,
            "gecos":text(&entry.gecos),
            "home":text(entry.home.as_os_str().as_bytes()),
            "shell":text(&entry.shell),
            "coverage":"local_files",
            "source":path_value(Path::new("/etc/passwd")),
        });
        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(coverage, false, None))
}

fn groups<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::GroupList;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let path = Path::new("/etc/group");
    let (bytes, truncated) = match read_bounded(path, ACCOUNT_SOURCE_LIMIT) {
        Ok(value) => value,
        Err(error) => return unavailable(sink, capability, path, error, coverage),
    };

    coverage.scanned = bytes.len() as u64;
    let mut partial = truncated;

    for line in bytes.split(|byte| *byte == b'\n') {
        check_deadline(deadline)?;
        if line.is_empty() {
            continue;
        }

        if coverage.observed as usize >= max_items {
            return Ok(finish(coverage, true, Some("max_items")));
        }

        let fields = line.split(|byte| *byte == b':').collect::<Vec<_>>();
        if fields.len() != 4 {
            partial = true;
            coverage.skipped += 1;
            continue;
        }

        let Ok(gid) = parse_u64(fields[2]) else {
            partial = true;
            coverage.skipped += 1;
            continue;
        };

        let members = fields[3]
            .split(|byte| *byte == b',')
            .filter(|value| !value.is_empty())
            .map(text)
            .collect::<Vec<_>>();

        let data = json!({
            "name":text(fields[0]),
            "gid":gid,
            "members":members,
            "coverage":"local_files",
            "source":path_value(path),
        });

        if !emit(sink, capability, data, &mut coverage)? {
            return Ok(output_limit(coverage));
        }
    }

    Ok(finish(
        coverage,
        partial,
        truncated.then_some("max_source_bytes"),
    ))
}

fn sudo_rules<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let mut files = vec![PathBuf::from("/etc/sudoers")];
    if let Ok(mut includes) = sorted_entries(Path::new("/etc/sudoers.d")) {
        files.append(&mut includes);
    }

    let files = discover_config_files(files, DirectiveMode::Sudo);

    collect_directives(
        invocation,
        sink,
        deadline,
        CapabilityId::SudoRules,
        files,
        DirectiveMode::Sudo,
    )
}

fn ssh_server_config<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let mut files = vec![PathBuf::from("/etc/ssh/sshd_config")];
    if let Ok(mut includes) = sorted_entries(Path::new("/etc/ssh/sshd_config.d")) {
        includes.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "conf")
        });
        files.append(&mut includes);
    }

    let files = discover_config_files(files, DirectiveMode::Ssh);

    collect_directives(
        invocation,
        sink,
        deadline,
        CapabilityId::SshServerConfig,
        files,
        DirectiveMode::Ssh,
    )
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DirectiveMode {
    Sudo,
    Ssh,
}

fn discover_config_files(initial: Vec<PathBuf>, mode: DirectiveMode) -> Vec<PathBuf> {
    const MAX_CONFIG_FILES: usize = 1_024;

    let mut queue = VecDeque::from(initial);
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();

    while let Some(path) = queue.pop_front() {
        let path = fs::canonicalize(&path).unwrap_or(path);
        if files.len() >= MAX_CONFIG_FILES || !seen.insert(path.clone()) {
            continue;
        }

        let Ok(metadata) = fs::metadata(&path) else {
            files.push(path);
            continue;
        };

        if metadata.is_dir() {
            if let Ok(entries) = sorted_entries(&path) {
                queue.extend(entries);
            }
            continue;
        }

        files.push(path.clone());
        let Ok((bytes, _)) = read_bounded(&path, CONFIG_SOURCE_LIMIT) else {
            continue;
        };

        for raw in bytes.split(|byte| *byte == b'\n') {
            let includes = match mode {
                DirectiveMode::Sudo => sudo_include(raw)
                    .map(|(directory, value)| vec![(directory, value.to_vec())])
                    .unwrap_or_default(),
                DirectiveMode::Ssh => {
                    let line = trim_ascii(strip_comment(raw));
                    let (key, value) = split_directive(line);
                    if key.eq_ignore_ascii_case(b"include") {
                        value
                            .split(|byte| byte.is_ascii_whitespace())
                            .filter(|value| !value.is_empty())
                            .map(|value| (false, value.to_vec()))
                            .collect()
                    } else {
                        Vec::new()
                    }
                }
            };

            for (directory, value) in includes {
                let value = trim_ascii(value.as_slice())
                    .strip_prefix(b"=")
                    .map_or(trim_ascii(value.as_slice()), trim_ascii);
                let value = value.strip_prefix(b"\"").unwrap_or(value);
                let value = value.strip_suffix(b"\"").unwrap_or(value);

                let mut include = PathBuf::from(std::ffi::OsStr::from_bytes(value));
                if include.is_relative() {
                    include = path
                        .parent()
                        .unwrap_or_else(|| Path::new("/"))
                        .join(include);
                }

                if directory {
                    queue.push_back(include);
                } else {
                    let pattern = include.as_os_str().as_bytes();
                    if pattern
                        .iter()
                        .any(|byte| matches!(byte, b'*' | b'?' | b'['))
                    {
                        if let Some(pattern) = include.to_str()
                            && let Ok(paths) = glob::glob(pattern)
                        {
                            let mut paths = paths.filter_map(Result::ok).collect::<Vec<_>>();
                            paths.sort_by(|left, right| {
                                left.as_os_str()
                                    .as_bytes()
                                    .cmp(right.as_os_str().as_bytes())
                            });
                            queue.extend(paths);
                        }
                    } else {
                        queue.push_back(include);
                    }
                }
            }
        }
    }

    files
}

fn sudo_include(line: &[u8]) -> Option<(bool, &[u8])> {
    let line = trim_ascii(line);
    for (directive, directory) in [
        (b"#includedir".as_slice(), true),
        (b"@includedir".as_slice(), true),
        (b"#include".as_slice(), false),
        (b"@include".as_slice(), false),
    ] {
        if line.starts_with(directive)
            && line
                .get(directive.len())
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'=')
        {
            return Some((directory, trim_ascii(&line[directive.len()..])));
        }
    }
    None
}

fn collect_directives<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
    capability: CapabilityId,
    files: Vec<PathBuf>,
    mode: DirectiveMode,
) -> Result<CapabilityReport, ProtocolError> {
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let mut partial = false;
    let mut total_bytes = 0_u64;
    let mut any_source = false;

    for path in files {
        let mut match_context: Option<Vec<u8>> = None;
        check_deadline(deadline)?;

        if total_bytes >= TOTAL_CONFIG_LIMIT {
            return Ok(finish(coverage, true, Some("max_source_bytes")));
        }

        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &path, &error, &mut coverage)?;
                continue;
            }
        };

        if metadata.len() > CONFIG_SOURCE_LIMIT {
            partial = true;
            coverage.skipped += 1;
            continue;
        }

        let (bytes, truncated) = match read_bounded(&path, CONFIG_SOURCE_LIMIT) {
            Ok(value) => value,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &path, &error, &mut coverage)?;
                continue;
            }
        };

        any_source = true;
        total_bytes += bytes.len() as u64;
        coverage.scanned += bytes.len() as u64;
        partial |= truncated;

        for (line_number, raw) in bytes.split(|byte| *byte == b'\n').enumerate() {
            check_deadline(deadline)?;
            let line = if mode == DirectiveMode::Sudo && sudo_include(raw).is_some() {
                trim_ascii(raw)
            } else {
                trim_ascii(strip_comment(raw))
            };

            if line.is_empty() {
                continue;
            }

            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }

            let (key, value) = split_directive(line);
            if matches!(mode, DirectiveMode::Ssh) && key.eq_ignore_ascii_case(b"match") {
                match_context = (!value.eq_ignore_ascii_case(b"all")).then(|| value.to_vec());
            }

            let data = match mode {
                DirectiveMode::Sudo => json!({
                    "source":path_value(&path),
                    "line":line_number + 1,
                    "directive":text(line),
                    "parse_mode":"static",
                }),
                DirectiveMode::Ssh => json!({
                    "source":path_value(&path),
                    "line":line_number + 1,
                    "key":text(key),
                    "value":text(value),
                    "match":match_context.as_deref().map(text),
                    "parse_mode":"static",
                }),
            };

            if !emit(sink, capability, data, &mut coverage)? {
                return Ok(output_limit(coverage));
            }
        }
    }

    if !any_source {
        let primary = match mode {
            DirectiveMode::Sudo => Path::new("/etc/sudoers"),
            DirectiveMode::Ssh => Path::new("/etc/ssh/sshd_config"),
        };
        return unavailable(
            sink,
            capability,
            primary,
            io::Error::new(io::ErrorKind::NotFound, "configuration source not found"),
            coverage,
        );
    }

    Ok(finish(coverage, partial, None))
}

fn authorized_keys<W: Write>(
    invocation: &Invocation,
    sink: &mut RecordSink<'_, W>,
    deadline: ExecutionContext<'_>,
) -> Result<CapabilityReport, ProtocolError> {
    let capability = CapabilityId::SshAuthorizedKeys;
    let max_items = item_limit(invocation)?;
    let mut coverage = Coverage::default();
    let users = match passwd_entries() {
        Ok(users) => users,
        Err(error) => {
            return unavailable(sink, capability, Path::new("/etc/passwd"), error, coverage);
        }
    };

    let (patterns, mut partial) = authorized_key_patterns();
    let mut total_bytes = 0_u64;

    for user in users {
        check_deadline(deadline)?;
        if user.home.as_os_str().is_empty() || is_remote_or_automount(&user.home) {
            partial = true;
            coverage.skipped += 1;
            coverage_diagnostic(
                sink,
                capability,
                &user.home,
                "home lies on an autofs or network-backed mount",
            )?;
            continue;
        }
        match usable_home_directory(&user.home) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                partial = true;
                source_failure(sink, capability, &user.home, &error, &mut coverage)?;
                continue;
            }
        }

        for path in authorized_key_paths(&user, &patterns) {
            if coverage.observed as usize >= max_items {
                return Ok(finish(coverage, true, Some("max_items")));
            }

            let (bytes, truncated) = match read_bounded(&path, CONFIG_SOURCE_LIMIT) {
                Ok(value) => value,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    partial = true;
                    source_failure(sink, capability, &path, &error, &mut coverage)?;
                    continue;
                }
            };

            total_bytes += bytes.len() as u64;
            coverage.scanned += bytes.len() as u64;
            partial |= truncated;
            if total_bytes > TOTAL_CONFIG_LIMIT {
                return Ok(finish(coverage, true, Some("max_source_bytes")));
            }

            for (line_number, raw) in bytes.split(|byte| *byte == b'\n').enumerate() {
                let line = trim_ascii(raw);
                if line.is_empty() || line.starts_with(b"#") {
                    continue;
                }

                let fields = line
                    .split(|byte| byte.is_ascii_whitespace())
                    .filter(|field| !field.is_empty())
                    .collect::<Vec<_>>();

                let Some(key_index) = fields.iter().position(|field| is_key_type(field)) else {
                    partial = true;
                    coverage.skipped += 1;
                    continue;
                };

                if fields.len() <= key_index + 1 {
                    partial = true;
                    coverage.skipped += 1;
                    continue;
                }

                let data = json!({
                    "user":text(&user.name),
                    "uid":user.uid,
                    "home":text(user.home.as_os_str().as_bytes()),
                    "source":path_value(&path),
                    "line":line_number + 1,
                    "options":fields[..key_index].iter().map(|value| text(value)).collect::<Vec<_>>(),
                    "key_type":text(fields[key_index]),
                    "fingerprint":fingerprint(fields[key_index + 1]),
                    "comment":fields.get(key_index + 2).map(|value| text(value)),
                });

                if !emit(sink, capability, data, &mut coverage)? {
                    return Ok(output_limit(coverage));
                }

                if coverage.observed as usize >= max_items {
                    return Ok(finish(coverage, true, Some("max_items")));
                }
            }
        }
    }

    Ok(finish(coverage, partial, None))
}

fn usable_home_directory(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

fn authorized_key_patterns() -> (Vec<Vec<u8>>, bool) {
    let mut initial = vec![PathBuf::from("/etc/ssh/sshd_config")];
    if let Ok(mut includes) = sorted_entries(Path::new("/etc/ssh/sshd_config.d")) {
        includes.retain(|path| {
            path.extension()
                .is_some_and(|extension| extension == "conf")
        });
        initial.append(&mut includes);
    }

    let files = discover_config_files(initial, DirectiveMode::Ssh);
    let mut patterns = Vec::new();
    let mut found = false;
    let mut partial = false;

    for path in files {
        let Ok((bytes, truncated)) = read_bounded(&path, CONFIG_SOURCE_LIMIT) else {
            partial = true;
            continue;
        };

        partial |= truncated;
        let mut in_match = false;

        for raw in bytes.split(|byte| *byte == b'\n') {
            let line = trim_ascii(strip_comment(raw));
            let (key, value) = split_directive(line);
            if key.is_empty() {
                continue;
            }
            if key.eq_ignore_ascii_case(b"match") {
                in_match = !value.eq_ignore_ascii_case(b"all");
            } else if !in_match && key.eq_ignore_ascii_case(b"authorizedkeysfile") {
                found = true;
                patterns.extend(
                    value
                        .split(|byte| byte.is_ascii_whitespace())
                        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case(b"none"))
                        .map(ToOwned::to_owned),
                );
            }
        }
    }

    if !found {
        patterns.extend([
            b".ssh/authorized_keys".to_vec(),
            b".ssh/authorized_keys2".to_vec(),
        ]);
    }

    (patterns, partial)
}

fn authorized_key_paths(user: &PasswdEntry, patterns: &[Vec<u8>]) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    for pattern in patterns {
        let mut expanded = Vec::with_capacity(pattern.len() + user.home.as_os_str().len());
        let mut index = 0;
        let mut valid = true;
        while index < pattern.len() {
            if pattern[index] != b'%' {
                expanded.push(pattern[index]);
                index += 1;
                continue;
            }
            if index + 1 == pattern.len() {
                valid = false;
                break;
            }
            match pattern[index + 1] {
                b'%' => expanded.push(b'%'),
                b'h' => expanded.extend_from_slice(user.home.as_os_str().as_bytes()),
                b'u' => expanded.extend_from_slice(&user.name),
                b'U' => expanded.extend_from_slice(user.uid.to_string().as_bytes()),
                _ => {
                    valid = false;
                    break;
                }
            }
            index += 2;
        }

        if !valid {
            continue;
        }

        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&expanded));
        paths.insert(if path.is_absolute() {
            path
        } else {
            user.home.join(path)
        });
    }

    paths.into_iter().collect()
}

fn passwd_entries() -> io::Result<Vec<PasswdEntry>> {
    let (bytes, truncated) = read_bounded(Path::new("/etc/passwd"), ACCOUNT_SOURCE_LIMIT)?;

    if truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "passwd exceeds source limit",
        ));
    }

    let mut entries = Vec::new();

    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }

        let fields = line.split(|byte| *byte == b':').collect::<Vec<_>>();
        if fields.len() != 7 {
            continue;
        }

        let (Ok(uid), Ok(gid)) = (parse_u64(fields[2]), parse_u64(fields[3])) else {
            continue;
        };

        entries.push(PasswdEntry {
            name: fields[0].to_vec(),
            uid,
            gid,
            gecos: fields[4].to_vec(),
            home: PathBuf::from(std::ffi::OsStr::from_bytes(fields[5])),
            shell: fields[6].to_vec(),
        });
    }

    entries.sort_by(|left, right| left.uid.cmp(&right.uid).then(left.name.cmp(&right.name)));

    Ok(entries)
}

fn split_directive(line: &[u8]) -> (&[u8], &[u8]) {
    let split = line
        .iter()
        .position(|byte| byte.is_ascii_whitespace() || *byte == b'=')
        .unwrap_or(line.len());
    let key = &line[..split];
    let value = trim_ascii(line.get(split + 1..).unwrap_or_default());
    (key, value)
}

fn strip_comment(line: &[u8]) -> &[u8] {
    line.iter()
        .position(|byte| *byte == b'#')
        .map_or(line, |index| &line[..index])
}

fn is_key_type(value: &[u8]) -> bool {
    value.starts_with(b"ssh-") || value.starts_with(b"ecdsa-") || value.starts_with(b"sk-")
}

fn fingerprint(encoded: &[u8]) -> Option<String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;

    let digest = Sha256::digest(decoded);

    Some(format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    ))
}

fn is_remote_or_automount(path: &Path) -> bool {
    let Ok((mountinfo, _)) = read_bounded(Path::new("/proc/self/mountinfo"), 16 << 20) else {
        return false;
    };

    let path = path.as_os_str().as_bytes();
    let mut best = 0;
    let mut remote = false;

    for line in mountinfo.split(|byte| *byte == b'\n') {
        let fields = line
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let Some(separator) = fields.iter().position(|field| *field == b"-") else {
            continue;
        };
        if separator < 5 || fields.len() <= separator + 1 {
            continue;
        }

        let target = unescape_mount(fields[4]);
        if path.starts_with(&target) && target.len() >= best {
            best = target.len();
            remote = matches!(
                fields[separator + 1],
                b"autofs" | b"nfs" | b"nfs4" | b"cifs" | b"smb3" | b"fuse.sshfs"
            );
        }
    }

    remote
}

fn unescape_mount(value: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'\\'
            && index + 3 < value.len()
            && value[index + 1..index + 4]
                .iter()
                .all(|byte| matches!(byte, b'0'..=b'7'))
        {
            output.push(
                (value[index + 1] - b'0') * 64
                    + (value[index + 2] - b'0') * 8
                    + (value[index + 3] - b'0'),
            );
            index += 4;
        } else {
            output.push(value[index]);
            index += 1;
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_include_graph_expands_globs_and_nested_includes() {
        let root = std::env::temp_dir().join(format!(
            "vzik-ssh-includes-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create fixture directory");
        let root = fs::canonicalize(root).expect("canonicalize fixture directory");
        let include_dir = root.join("sshd_config.d");
        fs::create_dir_all(&include_dir).expect("create include directory");
        let main = root.join("sshd_config");
        let included = include_dir.join("10-local.conf");
        let nested = root.join("nested.conf");
        fs::write(&main, b"Include sshd_config.d/*.conf\n").expect("write main");
        fs::write(&included, b"Include ../nested.conf\n").expect("write include");
        fs::write(&nested, b"PermitRootLogin no\n").expect("write nested");

        let files = discover_config_files(vec![main.clone()], DirectiveMode::Ssh);
        assert_eq!(files, vec![main, included, nested]);

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn authorized_key_paths_expand_supported_tokens() {
        let user = PasswdEntry {
            name: b"alice".to_vec(),
            uid: 1001,
            gid: 1001,
            gecos: Vec::new(),
            home: PathBuf::from("/home/alice"),
            shell: Vec::new(),
        };
        assert_eq!(
            authorized_key_paths(
                &user,
                &[b"%h/.ssh/authorized_keys".to_vec(), b"/keys/%u-%U".to_vec(),],
            ),
            vec![
                PathBuf::from("/home/alice/.ssh/authorized_keys"),
                PathBuf::from("/keys/alice-1001"),
            ]
        );
    }

    #[test]
    fn authorized_keys_skip_non_directory_homes() {
        let root =
            std::env::temp_dir().join(format!("vzik-authorized-home-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).expect("create home directory");
        fs::write(root.join("not-a-home"), b"").expect("create non-directory home");

        assert!(usable_home_directory(&root.join("home")).expect("inspect directory"));
        assert!(!usable_home_directory(&root.join("not-a-home")).expect("inspect file"));
        assert!(!usable_home_directory(&root.join("missing")).expect("inspect missing home"));

        fs::remove_dir_all(root).expect("remove home fixture");
    }
}
