mod build_pipeline;
mod fs_atomic;
mod keys;
mod package;
mod publish;
mod release;
mod validation;

use std::path::{Path, PathBuf};

use axe_artifact::Target;
use clap::{Parser, Subcommand, ValueEnum};

use build_pipeline::{BuildOptions, build};
use publish::{Backend, DiagnoseUploadOptions, PublishOptions, diagnose_upload, publish};
use release::{ReleaseOptions, ReleasePhase, release};

#[derive(Debug, Parser)]
#[command(
    name = "axe-store",
    version,
    about = "Build and publish signed AXE Store snapshots"
)]
struct Cli {
    #[arg(long, default_value = "config/store.json", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Keys {
        #[command(subcommand)]
        command: KeysCommand,
    },
    Build(BuildArgs),
    Publish(PublishArgs),
    DiagnoseUpload(DiagnoseUploadArgs),
    Release(ReleaseArgs),
    Sync(SyncArgs),
}

#[derive(Debug, Subcommand)]
enum KeysCommand {
    Generate {
        #[arg(long, default_value = "keys/store")]
        output: PathBuf,
        #[arg(long, default_value = "store/trusted")]
        trusted_output: PathBuf,
    },
    RelayToken {
        #[arg(long, default_value = "keys/relay/token")]
        output: PathBuf,
    },
    RelayIdentities {
        #[arg(long, default_value = "keys/relay")]
        output: PathBuf,
    },
}

#[derive(Clone, Debug, clap::Args)]
struct BuildArgs {
    #[arg(long, default_value = ".")]
    flake: PathBuf,
    #[arg(long)]
    package: Option<String>,
    #[arg(long)]
    target: Option<Target>,
    #[arg(long, default_value = "store/dist")]
    output: PathBuf,
}

#[derive(Clone, Debug, clap::Args)]
struct PublishArgs {
    #[arg(long, default_value = "store/dist")]
    input: PathBuf,
    #[arg(long, value_enum, default_value = "s3")]
    backend: BackendArg,
    #[arg(long)]
    directory: Option<PathBuf>,
    #[arg(long)]
    allow_target_removal: bool,
}

#[derive(Clone, Debug, clap::Args)]
struct DiagnoseUploadArgs {
    /// Payload sizes in bytes, separated by commas
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "65536,524288,1048576,3145728,5242880"
    )]
    size: Vec<u64>,
    /// Number of probes for each size and connection mode
    #[arg(long, default_value_t = 3)]
    rounds: usize,
    /// Per-request timeout
    #[arg(long, default_value_t = 60)]
    timeout_secs: u64,
}

#[derive(Clone, Debug, clap::Args)]
struct ReleaseArgs {
    #[arg(long, default_value = "dist")]
    input: PathBuf,
    #[arg(long, default_value = "nix/axe-releases.json")]
    metadata: PathBuf,
    #[arg(long)]
    target: Vec<Target>,
    #[arg(long, value_enum, default_value = "s3")]
    backend: BackendArg,
    #[arg(long)]
    directory: Option<PathBuf>,
    #[arg(long, conflicts_with = "stable_only")]
    immutable_only: bool,
    #[arg(long)]
    stable_only: bool,
}

#[derive(Clone, Debug, clap::Args)]
struct SyncArgs {
    #[command(flatten)]
    build: BuildArgs,
    #[arg(long, value_enum, default_value = "s3")]
    backend: BackendArg,
    #[arg(long)]
    directory: Option<PathBuf>,
    #[arg(long)]
    allow_target_removal: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BackendArg {
    S3,
    Directory,
}

impl From<BackendArg> for Backend {
    fn from(value: BackendArg) -> Self {
        match value {
            BackendArg::S3 => Self::S3,
            BackendArg::Directory => Self::Directory,
        }
    }
}

fn main() {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("axe-store: {error}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let workspace =
        std::env::current_dir().map_err(|error| format!("read current directory: {error}"))?;
    match cli.command {
        Command::Keys { command } => match command {
            KeysCommand::Generate {
                output,
                trusted_output,
            } => {
                keys::generate(&output, &trusted_output)?;
                println!(
                    "generated signing key in {} and trusted public key in {}",
                    output.display(),
                    trusted_output.display()
                );
            }
            KeysCommand::RelayToken { output } => {
                keys::generate_relay_token(&output)?;
                println!("generated relay token in {}", output.display());
            }
            KeysCommand::RelayIdentities { output } => {
                keys::generate_relay_identities(&output)?;
                println!(
                    "generated relay server and sshd client QUIC identities in {}",
                    output.display()
                );
            }
        },
        Command::Build(args) => run_build(&workspace, &args)?,
        Command::Publish(args) => {
            let generation = run_publish(&workspace, &cli.config, &args)?;
            println!("published Index generation {generation}");
        }
        Command::DiagnoseUpload(args) => {
            let result = diagnose_upload(&DiagnoseUploadOptions {
                workspace: &workspace,
                config: &cli.config,
                sizes: &args.size,
                rounds: args.rounds,
                timeout_secs: args.timeout_secs,
            })?;
            println!(
                "completed {} upload probes: {} succeeded, {} failed; {} cleanup failures",
                result.samples,
                result.samples - result.failures,
                result.failures,
                result.cleanup_failures
            );
            if result.failures != 0 || result.cleanup_failures != 0 {
                return Err(format!(
                    "upload diagnostics observed {} failed probes and {} cleanup failures",
                    result.failures, result.cleanup_failures
                ));
            }
        }
        Command::Release(args) => {
            let targets = if args.target.is_empty() {
                Target::ALL.as_slice()
            } else {
                args.target.as_slice()
            };
            let published = release(&ReleaseOptions {
                workspace: &workspace,
                input: &args.input,
                metadata: &args.metadata,
                targets,
                backend: args.backend.into(),
                directory: args.directory.as_deref(),
                config: &cli.config,
                phase: if args.immutable_only {
                    ReleasePhase::ImmutableOnly
                } else if args.stable_only {
                    ReleasePhase::StableOnly
                } else {
                    ReleasePhase::All
                },
            })?;
            println!("published {published} Axe release artifacts");
        }
        Command::Sync(args) => {
            run_build(&workspace, &args.build)?;
            let publish_args = PublishArgs {
                input: args.build.output,
                backend: args.backend,
                directory: args.directory,
                allow_target_removal: args.allow_target_removal,
            };
            let generation = run_publish(&workspace, &cli.config, &publish_args)?;
            println!("published Index generation {generation}");
        }
    }
    Ok(())
}

fn run_build(workspace: &Path, args: &BuildArgs) -> Result<(), String> {
    let result = build(&BuildOptions {
        workspace,
        flake: &args.flake,
        package: args.package.as_deref(),
        target: args.target,
        output: &args.output,
    })?;
    println!(
        "built {} packages with {} targets; {} unsupported",
        result.packages,
        result.targets,
        result.failures.len()
    );
    Ok(())
}

fn run_publish(workspace: &Path, config: &Path, args: &PublishArgs) -> Result<u64, String> {
    publish(&PublishOptions {
        workspace,
        input: &args.input,
        backend: args.backend.into(),
        directory: args.directory.as_deref(),
        config,
        allow_target_removal: args.allow_target_removal,
    })
}
