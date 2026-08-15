// This file is part of jinx. Copyright © 2024-2026 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::constants::CLAP_VERSION;
use clap::{Args, Parser, Subcommand};

/// Discord bot that handles Jinxxy license registration.
/// If ran with no subcommands the bot will start.
#[derive(Parser)]
#[command(version = CLAP_VERSION, long_about, author)]
pub struct JinxArgs {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize DB with a Discord bot token and exit.
    Init {
        /// Discord token. Depending on execution environment it may not be secure to pass secrets as a command-line argument.
        /// Instead, you may provide it with the `DISCORD_TOKEN` environment variable.
        discord_token: Option<String>,
    },
    /// Check GitHub for updates
    UpdateCheck,
    /// Modify bot owners
    Owner(OwnerArgs),
    /// Migrate v1 -> v2 database, and then exit
    Migrate,
    /// Set advanced configuration values
    Set(SetArgs),
}

#[derive(Args)]
pub struct OwnerArgs {
    #[command(subcommand)]
    pub command: OwnerCommand,
}

#[derive(Subcommand)]
pub enum OwnerCommand {
    /// Add a new bot owner
    Add {
        /// Discord ID to add as a new bot owner
        discord_id: String,
    },
    /// Remove a bot owner
    Rm {
        /// Discord ID to remove as a bot owner
        discord_id: String,
    },
    /// List bot owners
    Ls,
}

#[derive(Args)]
pub struct SetArgs {
    #[command(subcommand)]
    pub command: SetCommand,
}

#[derive(Subcommand)]
pub enum SetCommand {
    /// Enables the privileged GUILD_MEMBERS intent. Setting this to `true` will break bot startup unless the intent is
    /// enabled in the developer portal.
    GuildMembers {
        /// Should the intent be specified during bot startup?
        #[arg(action = clap::ArgAction::Set, default_value = "false")]
        enabled: bool,
    },
}
