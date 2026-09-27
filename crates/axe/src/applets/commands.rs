use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::PathBuf;

use serde::Serialize;

use crate::applets::doctor::{EncodedOsValue, encode_os};
use crate::registry::{CommandInfo, CommandSource};

const HELP: &str = "Usage: commands [NAME] [--json]\n\n\
List commands available through AXE. Output is one JSON document by default.\n\
Pass NAME to describe one command without running it.\n\
Each command reports local, on_demand, or blocked availability. local_path is\n\
set only when a published local applet path currently resolves.\n\n\
Options:\n\
  --json        explicitly select the default JSON output\n\
  -h, --help    display this help and exit\n";

pub fn commands(args: Vec<OsString>) -> i32 {
    match &args[1..] {
        [] => list(),
        [format] if format == "--json" => list(),
        [help] if help == "--help" || help == "-h" => {
            print!("{HELP}");
            0
        }
        [name] => command(name),
        [name, format] if format == "--json" => command(name),
        _ => {
            eprintln!("commands: invalid arguments; run 'commands --help'");
            2
        }
    }
}

fn list() -> i32 {
    let commands = crate::registry::command_info()
        .iter()
        .map(CommandRecord::new)
        .collect::<Vec<_>>();
    write_json(&CommandList {
        schema: "axe_commands",
        schema_version: 1,
        commands,
    })
}

fn command(name: &OsStr) -> i32 {
    let Some(name) = name.to_str() else {
        eprintln!("commands: command name is not valid UTF-8");
        return 2;
    };

    let commands = crate::registry::command_info();
    let command = commands
        .binary_search_by(|command| command.name.as_str().cmp(name))
        .ok()
        .map(|index| CommandRecord::new(&commands[index]));
    let found = command.is_some();
    let store_disabled = matches!(
        crate::registry::StoreMode::from_environment(),
        Ok(crate::registry::StoreMode::Off)
    );
    let error = command.is_none().then_some(UnknownCommand {
        kind: "unknown_command",
        name,
        store_mode: store_disabled.then_some("off"),
        store_registry: store_disabled.then_some("disabled"),
    });
    let output = CommandLookup {
        schema: "axe_commands",
        schema_version: 1,
        command,
        error,
    };

    let status = write_json(&output);
    if status != 0 {
        status
    } else if found {
        0
    } else {
        127
    }
}

#[derive(Serialize)]
struct CommandList<'a> {
    schema: &'static str,
    schema_version: u32,
    commands: Vec<CommandRecord<'a>>,
}

#[derive(Serialize)]
struct CommandLookup<'a> {
    schema: &'static str,
    schema_version: u32,
    command: Option<CommandRecord<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<UnknownCommand<'a>>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CommandAvailability {
    Local,
    OnDemand,
    Blocked,
}

#[derive(Serialize)]
struct CommandRecord<'a> {
    #[serde(flatten)]
    info: &'a CommandInfo,
    availability: CommandAvailability,
    local_path: Option<EncodedOsValue>,
}

impl<'a> CommandRecord<'a> {
    fn new(info: &'a CommandInfo) -> Self {
        let availability = match info.source {
            CommandSource::Bundled => CommandAvailability::Local,
            CommandSource::Store if crate::ondemand::store_is_blocked() => {
                CommandAvailability::Blocked
            }
            CommandSource::Store => CommandAvailability::OnDemand,
        };
        let local_path = if matches!(info.source, CommandSource::Bundled) {
            published_applet_path(&info.name)
        } else {
            None
        };
        Self {
            info,
            availability,
            local_path,
        }
    }
}

fn published_applet_path(name: &str) -> Option<EncodedOsValue> {
    let directory = std::env::var_os("AXE_APPLET_DIR").filter(|value| !value.is_empty())?;
    let path = PathBuf::from(directory).join(name);
    path.is_file().then(|| encode_os(path.as_os_str()))
}

#[derive(Serialize)]
struct UnknownCommand<'a> {
    kind: &'static str,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_registry: Option<&'static str>,
}

fn write_json(value: &impl Serialize) -> i32 {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    match serde_json::to_writer(&mut output, value)
        .and_then(|()| output.write_all(b"\n").map_err(serde_json::Error::io))
    {
        Ok(()) => 0,
        Err(_) => 5,
    }
}
