// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::env;
use std::ffi::OsString;
use std::io::Write;

use clap::{Arg, ArgAction, Command};

use uucore::display::{OsWrite, print_all_env_vars};
use uucore::error::{UResult, USimpleError};
use uucore::line_ending::LineEnding;
use uucore::{format_usage, translate};

static OPT_NULL: &str = "null";

static ARG_VARIABLES: &str = "variables";

#[uucore::main(no_signals)]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let matches = uucore::clap_localization::handle_clap_result_with_exit_code(uu_app(), args, 2)?;

    let variables = matches
        .get_many::<OsString>(ARG_VARIABLES)
        .unwrap_or_default();

    let separator = LineEnding::from_zero_flag(matches.get_flag(OPT_NULL));

    if variables.len() == 0 {
        print_all_env_vars(separator).map_err(|e| USimpleError::new(2, e.to_string()))?;
        std::io::stdout()
            .flush()
            .map_err(|e| USimpleError::new(2, e.to_string()))?;
        return Ok(());
    }

    let mut error_found = false;
    for env_var in variables {
        // we silently ignore a=b as variable but we trigger an error
        if env_var.as_encoded_bytes().contains(&b'=') {
            error_found = true;
            continue;
        }
        if let Some(var) = env::var_os(env_var) {
            let mut stdout = std::io::stdout().lock();
            stdout
                .write_all_os(&var)
                .map_err(|e| USimpleError::new(2, e.to_string()))?;
            write!(stdout, "{separator}").map_err(|e| USimpleError::new(2, e.to_string()))?;
        } else {
            error_found = true;
        }
    }
    std::io::stdout()
        .flush()
        .map_err(|e| USimpleError::new(2, e.to_string()))?;

    if error_found { Err(1.into()) } else { Ok(()) }
}

pub fn uu_app() -> Command {
    let cmd = Command::new("printenv")
        .version(uucore::crate_version!())
        .about(translate!("printenv-about"))
        .override_usage(format_usage(&translate!("printenv-usage")))
        .infer_long_args(true);
    uucore::clap_localization::configure_localized_command(cmd)
        .arg(
            Arg::new(OPT_NULL)
                .short('0')
                .long(OPT_NULL)
                .help(translate!("printenv-help-null"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(ARG_VARIABLES)
                .action(ArgAction::Append)
                .value_parser(clap::value_parser!(OsString))
                .num_args(1..),
        )
}
