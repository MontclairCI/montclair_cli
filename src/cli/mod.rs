use clap::Parser;
use clap::Subcommand;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use muzanci_observability::TError;
use muzanci_observability::TracingConfig;

use crate::cli::check::CheckArgs;
use crate::cli::check::run_check;
use crate::cli::debug::DebugInteractiveArgs;
use crate::cli::debug::DebugSubcommand;
use crate::cli::debug::run_debug_create;
use crate::cli::debug::run_debug_exec;
use crate::cli::debug::run_debug_exec_step;
use crate::cli::debug::run_debug_interactive;
use crate::cli::show::ShowArgs;
use crate::cli::show::run_show;
use crate::cli::user_config::UserConfig;
use crate::signal_receiver::SignalReceiver;

mod check;
mod debug;
mod parse;
mod show;
mod user_config;

#[derive(Parser)]
#[command(
    name = "muzanci-cli",
    about = "CLI tool for debugging MuzanCI pipeline configurations and job execution"
)]
pub struct Cli {
    #[arg(long, value_name = "FILE", default_value = "muzan.toml", default_value_os_t = UserConfig::default_path(), global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: CliCommand,
}

impl Cli {
    pub async fn run(self) -> Result<(), TError> {
        let user_config = self.user_config()?;

        let disable_telemetry = user_config.fields().disable_telemetry.unwrap_or(false)
            || std::env::var("DO_NOT_TRACK").is_ok_and(|v| v == "1");

        if !disable_telemetry {
            muzanci_observability::init_tracing(TracingConfig::default())?;
        }

        let mut join_set = JoinSet::new();
        let signal_receiver_enabled = Arc::new(AtomicBool::new(false));
        let client_shutdown = CancellationToken::new();
        SignalReceiver::spawn(
            &mut join_set,
            client_shutdown.clone(),
            signal_receiver_enabled.clone(),
        );

        join_set.spawn(async move {
            let result = match self.command {
                CliCommand::Show { args } => run_show(args),
                CliCommand::Check { args } => run_check(args),
                CliCommand::Debug {
                    subcommand: Some(DebugSubcommand::Create { args }),
                    ..
                } => run_debug_create(args, user_config, client_shutdown.clone()).await,
                CliCommand::Debug {
                    subcommand: Some(DebugSubcommand::Exec { args }),
                    ..
                } => {
                    run_debug_exec(
                        args,
                        user_config,
                        client_shutdown.clone(),
                        signal_receiver_enabled,
                    )
                    .await
                }
                CliCommand::Debug {
                    subcommand: Some(DebugSubcommand::ExecStep { args }),
                    ..
                } => {
                    run_debug_exec_step(
                        args,
                        user_config,
                        client_shutdown.clone(),
                        signal_receiver_enabled,
                    )
                    .await
                }
                CliCommand::Debug {
                    subcommand: Some(DebugSubcommand::Interactive { args }),
                    ..
                } => {
                    run_debug_interactive(
                        args,
                        user_config,
                        client_shutdown.clone(),
                        signal_receiver_enabled,
                    )
                    .await
                }
                CliCommand::Debug {
                    subcommand: None,
                    args,
                } => {
                    run_debug_interactive(
                        args,
                        user_config,
                        client_shutdown.clone(),
                        signal_receiver_enabled,
                    )
                    .await
                }
            };

            client_shutdown.cancel();

            result
        });

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => eprintln!("error: {e}"),
                Err(e) => eprintln!("join error: {e}"),
            }
        }

        Ok(())
    }

    fn user_config(&self) -> Result<UserConfig, TError> {
        let config_path = &self.config;

        if !std::fs::exists(config_path)? {
            if config_path == &UserConfig::default_path() {
                eprintln!("Creating new config file at {}", config_path.display());
                if let Some(parent) = config_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::File::create(config_path)?;
            } else {
                return Err(TError::new("config file not found"));
            }
        }

        eprintln!("Using config file at {}", config_path.display());

        let user_config = UserConfig::from_file(config_path)?;

        Ok(user_config)
    }
}

#[derive(Subcommand)]
enum CliCommand {
    #[command(
        name = "show",
        about = "Prints the dependency graph in the specified format"
    )]
    Show {
        #[command(flatten)]
        args: ShowArgs,
    },

    #[command(
        name = "check",
        about = "Checks for syntax errors and cyclical dependencies"
    )]
    Check {
        #[command(flatten)]
        args: CheckArgs,
    },

    #[command(name = "debug", about = "Start an interactive debug session")]
    Debug {
        #[command(flatten)]
        args: DebugInteractiveArgs,

        #[command(subcommand)]
        subcommand: Option<DebugSubcommand>,
    },
}
