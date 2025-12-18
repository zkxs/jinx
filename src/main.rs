// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::Bot;
use crate::cli_args::{JinxArgs, OwnerCommand};
use clap::Parser;
use std::process::ExitCode;
use std::sync::atomic;
use std::sync::atomic::AtomicBool;
use tokio::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod bot;
mod cli_args;
mod db;
mod error;
mod http;
mod license;
mod time;

/// constants generated in build.rs
pub mod constants {
    include!(env!("CONSTANTS_PATH"));
}

type Error = Box<dyn std::error::Error + Send + Sync>;

const DB_OPEN_ERROR_MESSAGE: &str = "Failed to open jinx2.sqlite";
const DB_READ_ERROR_MESSAGE: &str = "Failed to read from database";
const DB_WRITE_ERROR_MESSAGE: &str = "Failed to write to database";
const DISCORD_ID_PARSE_ERROR_MESSAGE: &str = "Failed to parse Discord ID";

/// If we should restart the bot on shutdown
static SHOULD_RESTART: AtomicBool = AtomicBool::new(false);

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    let cli_args = JinxArgs::parse();
    match cli_args.command {
        #[allow(clippy::print_stderr)]
        Some(cli_args::Command::Init { discord_token }) => {
            let discord_token = discord_token.or_else(|| std::env::var("DISCORD_TOKEN").ok());
            if let Some(discord_token) = discord_token {
                let db = db::JinxDb::open()
                    .await
                    .unwrap_or_else(|e| panic!("{DB_OPEN_ERROR_MESSAGE}: {e:?}"));
                db.set_discord_token(&discord_token)
                    .await
                    .expect("Failed to set discord token");
                db.close().await;
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "discord token must be provided either via command-line parameter or DISCORD_TOKEN environment variable"
                );
                ExitCode::FAILURE
            }
        }
        #[allow(clippy::print_stdout)]
        Some(cli_args::Command::UpdateCheck) => {
            println!("{}", http::update_checker::check_for_update().await);
            ExitCode::SUCCESS
        }
        #[allow(clippy::print_stdout)]
        Some(cli_args::Command::Owner(cli_args::OwnerArgs { command })) => {
            let db = db::JinxDb::open()
                .await
                .unwrap_or_else(|e| panic!("{DB_OPEN_ERROR_MESSAGE}: {e:?}"));
            match command {
                OwnerCommand::Add { discord_id } => {
                    let discord_id = discord_id
                        .parse()
                        .unwrap_or_else(|e| panic!("{DISCORD_ID_PARSE_ERROR_MESSAGE}: {e:?}"));
                    db.add_owner(discord_id)
                        .await
                        .unwrap_or_else(|e| panic!("{DB_WRITE_ERROR_MESSAGE}: {e:?}"));
                }
                OwnerCommand::Rm { discord_id } => {
                    let discord_id = discord_id
                        .parse()
                        .unwrap_or_else(|e| panic!("{DISCORD_ID_PARSE_ERROR_MESSAGE}: {e:?}"));
                    db.delete_owner(discord_id)
                        .await
                        .unwrap_or_else(|e| panic!("{DB_WRITE_ERROR_MESSAGE}: {e:?}"));
                }
                OwnerCommand::Ls => {
                    let owners = db
                        .get_owners()
                        .await
                        .unwrap_or_else(|e| panic!("{DB_READ_ERROR_MESSAGE}: {e:?}"));
                    owners.into_iter().for_each(|id| println!("{id}"));
                }
            }
            db.close().await;
            ExitCode::SUCCESS
        }
        Some(cli_args::Command::Migrate) => {
            init_logging();
            let db = db::JinxDb::open()
                .await
                .unwrap_or_else(|e| panic!("{DB_OPEN_ERROR_MESSAGE}: {e:?}"));
            db.migrate()
                .await
                .unwrap_or_else(|e| panic!("Error migrating database: {e:?}"));
            db.close().await;
            ExitCode::SUCCESS
        }
        None => {
            init_logging();
            info!(
                "starting {} {} {}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
                constants::GIT_COMMIT_HASH
            );

            let result = Toplevel::new(async |subsystem: &mut SubsystemHandle| {
                subsystem.start(SubsystemBuilder::new("Discord bot", bot_subsystem));
            })
            .catch_signals()
            .handle_shutdown_requests(Duration::from_millis(1000))
            .await;

            if SHOULD_RESTART.load(atomic::Ordering::Acquire) {
                info!("restarting now: {:?}", result);
                ExitCode::SUCCESS
            } else {
                info!("shutting down now: {:?}", result);
                ExitCode::FAILURE
            }
        }
    }
}

async fn bot_subsystem(subsystem: &mut SubsystemHandle) -> Result<(), Error> {
    let mut bot = Bot::new().await?;
    tokio::select! {
        _ = subsystem.on_shutdown_requested() => {
            info!("external shutdown requested");
            bot.close().await;
            Ok(())
        },
        result = bot.start() => {
            info!("bot subsystem stopped itself");
            result
        }
    }
}

/// Initialize logging to stdout
fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_new("info,jinx=debug,serenity::gateway::shard=error").expect("Failed to create EnvFilter"),
        )
        .init();
}
