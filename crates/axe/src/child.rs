use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path};
use std::process::Command;

pub(crate) struct ResolvedCommand {
    pub(crate) program: OsString,
    pub(crate) prefix_args: Vec<OsString>,
}

pub(crate) fn resolve(program: &OsStr) -> io::Result<ResolvedCommand> {
    let bundled_name = simple_name(program).and_then(OsStr::to_str).filter(|name| {
        brush_shell::bundled::registry().is_some_and(|registry| registry.contains_key(*name))
    });

    if let Some(name) = bundled_name {
        let executable = brush_shell::bundled::executable_path().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot locate axe executable for bundled child",
            )
        })?;
        Ok(ResolvedCommand {
            program: executable.into_os_string(),
            prefix_args: vec![
                OsString::from(brush_shell::bundled::DISPATCH_FLAG),
                OsString::from(name),
            ],
        })
    } else {
        Ok(ResolvedCommand {
            program: program.to_owned(),
            prefix_args: Vec::new(),
        })
    }
}

pub(crate) fn command(program: &OsStr) -> io::Result<Command> {
    let resolved = resolve(program)?;
    let mut command = Command::new(resolved.program);
    command.args(resolved.prefix_args);
    Ok(command)
}

pub(crate) fn rewrite(args: &mut Vec<OsString>, command_index: usize) -> io::Result<bool> {
    let Some(program) = args.get(command_index) else {
        return Ok(false);
    };
    let resolved = resolve(program)?;
    if resolved.prefix_args.is_empty() {
        return Ok(false);
    }

    args[command_index] = resolved.program;
    args.splice(command_index + 1..command_index + 1, resolved.prefix_args);
    Ok(true)
}

fn simple_name(path: &OsStr) -> Option<&OsStr> {
    let mut components = Path::new(path).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) => Some(name),
        _ => None,
    }
}
