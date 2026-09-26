use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;

pub type AppletFn = fn(Vec<OsString>) -> i32;

fn prepare(util_crate_name: &str) -> Result<(), i32> {
    util_linux_uucore::panic::mute_sigpipe_panic();

    let name = util_linux_uucore::get_canonical_util_name(util_crate_name);
    util_linux_uucore::locale::setup_localization(name).map_err(|error| {
        eprintln!("axe: could not initialize localization for '{name}': {error}");
        99
    })
}

fn finish() {
    if let Err(error) = std::io::stdout().flush() {
        eprintln!("axe: error flushing stdout: {error}");
    }
}

macro_rules! register {
    ($commands:expr, $name:literal, $util:ident) => {{
        fn adapter(args: Vec<OsString>) -> i32 {
            if let Err(code) = prepare(stringify!($util)) {
                return code;
            }

            let code = $util::uumain(args.into_iter());
            finish();
            code
        }
        $commands.insert($name.to_string(), adapter as AppletFn);
    }};
}

pub fn commands() -> HashMap<String, AppletFn> {
    let mut commands = HashMap::new();

    register!(commands, "dmesg", uu_dmesg);
    register!(commands, "hexdump", uu_hexdump);
    register!(commands, "last", uu_last);
    register!(commands, "mountpoint", uu_mountpoint);
    commands
}
